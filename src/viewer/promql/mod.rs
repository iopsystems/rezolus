use promql_parser::parser::{self, token::TokenType, Expr};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use thiserror::Error;

use crate::viewer::tsdb::{GaugeSeries, Labels, Tsdb, UntypedCollection};

mod api;

#[cfg(test)]
mod tests;

pub use api::routes;

#[derive(Error, Debug)]
pub enum QueryError {
    #[error("Parse error: {0}")]
    ParseError(String),

    #[error("Evaluation error: {0}")]
    EvaluationError(String),

    #[allow(dead_code)]
    #[error("Unsupported operation: {0}")]
    Unsupported(String),

    #[error("Metric not found: {0}")]
    MetricNotFound(String),
}

/// A single sample in the result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sample {
    pub metric: HashMap<String, String>,
    pub value: (f64, f64), // (timestamp_seconds, value)
}

/// A matrix sample with multiple values over time
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatrixSample {
    pub metric: HashMap<String, String>,
    pub values: Vec<(f64, f64)>, // Vec of (timestamp_seconds, value)
}

/// Result of a PromQL query
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "resultType", rename_all = "camelCase")]
pub enum QueryResult {
    #[serde(rename = "vector")]
    Vector { result: Vec<Sample> },

    #[serde(rename = "matrix")]
    Matrix { result: Vec<MatrixSample> },

    #[serde(rename = "scalar")]
    Scalar { result: (f64, f64) }, // (timestamp, value)
}

/// The PromQL query engine
pub struct QueryEngine {
    tsdb: Arc<Tsdb>,
}

impl QueryEngine {
    pub fn new(tsdb: Arc<Tsdb>) -> Self {
        Self { tsdb }
    }

    /// Get the time range (min, max) of all data in seconds
    pub fn get_time_range(&self) -> (f64, f64) {
        let mut min_time = f64::INFINITY;
        let mut max_time = f64::NEG_INFINITY;

        // Try a few known metrics to get the time range
        let known_metrics = [
            "cpu_cores",
            "memory_total",
            "syscall",
            "network_bytes",
            "cpu_usage",
        ];
        for metric_name in &known_metrics {
            if let Some(collection) = self.tsdb.gauges(metric_name, Labels::default()) {
                let sum_series = collection.filtered_sum(&Labels::default());
                for (&timestamp_ns, _) in sum_series.inner.iter() {
                    let timestamp_s = timestamp_ns as f64 / 1e9;
                    min_time = min_time.min(timestamp_s);
                    max_time = max_time.max(timestamp_s);
                }
            }

            if let Some(collection) = self.tsdb.counters(metric_name, Labels::default()) {
                let rate_collection = collection.filtered_rate(&Labels::default());
                let sum_series = rate_collection.sum();
                for (&timestamp_ns, _) in sum_series.inner.iter() {
                    let timestamp_s = timestamp_ns as f64 / 1e9;
                    min_time = min_time.min(timestamp_s);
                    max_time = max_time.max(timestamp_s);
                }
            }
        }

        if min_time == f64::INFINITY {
            // No data found, return a reasonable default
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs_f64();
            (now - 3600.0, now) // 1 hour ago to now
        } else {
            (min_time, max_time)
        }
    }

    /// Execute a simple query - for now, just basic metric access and irate() function
    pub fn query(&self, query_str: &str, time: Option<f64>) -> Result<QueryResult, QueryError> {
        // For now, handle very simple cases manually
        // TODO: Replace with proper PromQL parser once we fix the API issues

        if query_str.starts_with("irate(") && query_str.ends_with(")") {
            self.handle_simple_rate(query_str, time)
        } else if query_str.starts_with("sum(irate(") && query_str.ends_with("))") {
            self.handle_sum_rate(query_str, time)
        } else {
            self.handle_simple_metric(query_str, time)
        }
    }

    /// Handle simple irate() queries like irate(cpu_cycles[5m]) or irate(network_bytes{direction="transmit"}[5m])
    fn handle_simple_rate(
        &self,
        query: &str,
        time: Option<f64>,
    ) -> Result<QueryResult, QueryError> {
        // Extract metric name from irate(metric_name[duration])
        let inner = &query[6..query.len() - 1]; // Remove "irate(" and ")"

        if let Some(bracket_pos) = inner.find('[') {
            let metric_part = inner[..bracket_pos].trim();

            // Parse metric name and labels
            let (metric_name, labels) = self.parse_metric_selector(metric_part)?;

            if let Some(collection) = self.tsdb.counters(&metric_name, labels.clone()) {
                let rate_collection = collection.filtered_rate(&labels);

                // If no specific labels were provided, return all series separately
                // Otherwise, sum matching series
                if labels.inner.is_empty() {
                    // Return all series separately
                    let mut result_samples = Vec::new();

                    for (series_labels, series) in rate_collection.iter() {
                        if let Some((timestamp, value)) =
                            self.get_value_at_time(&series.inner, time)
                        {
                            let mut metric_labels = HashMap::new();
                            metric_labels.insert("__name__".to_string(), metric_name.to_string());

                            // Add all labels from this series
                            for (key, value) in series_labels.inner.iter() {
                                metric_labels.insert(key.clone(), value.clone());
                            }

                            result_samples.push(Sample {
                                metric: metric_labels,
                                value: (timestamp, value),
                            });
                        }
                    }

                    if !result_samples.is_empty() {
                        return Ok(QueryResult::Vector {
                            result: result_samples,
                        });
                    }
                } else {
                    // Labels specified, sum matching series
                    let sum_series = rate_collection.sum();
                    if let Some((timestamp, value)) =
                        self.get_value_at_time(&sum_series.inner, time)
                    {
                        let mut metric_labels = HashMap::new();
                        metric_labels.insert("__name__".to_string(), metric_name.to_string());

                        // Add the labels from the query to the result
                        for (key, value) in labels.inner.iter() {
                            metric_labels.insert(key.clone(), value.clone());
                        }

                        return Ok(QueryResult::Vector {
                            result: vec![Sample {
                                metric: metric_labels,
                                value: (timestamp, value),
                            }],
                        });
                    }
                }
            }
        }

        Err(QueryError::MetricNotFound(format!(
            "Could not find metric for query: {query}"
        )))
    }

    /// Handle sum(irate()) queries like sum(irate(cpu_cycles[5m]))
    fn handle_sum_rate(&self, query: &str, time: Option<f64>) -> Result<QueryResult, QueryError> {
        // Extract the irate() part
        let rate_part = &query[4..query.len() - 1]; // Remove "sum(" and ")"

        // First get the rate results
        let rate_result = self.handle_simple_rate(rate_part, time)?;

        // Sum all the series together
        if let QueryResult::Vector { result: samples } = rate_result {
            if samples.is_empty() {
                return Err(QueryError::MetricNotFound(
                    "No data found for sum(irate()) query".to_string(),
                ));
            }

            // For instant queries, sum all values at the same timestamp
            let timestamp = samples[0].value.0;
            let summed_value: f64 = samples.iter().map(|s| s.value.1).sum();

            // Extract metric name from the first sample
            let metric_name = samples[0]
                .metric
                .get("__name__")
                .cloned()
                .unwrap_or_else(|| "sum".to_string());

            let mut metric_labels = HashMap::new();
            metric_labels.insert("__name__".to_string(), metric_name);

            return Ok(QueryResult::Vector {
                result: vec![Sample {
                    metric: metric_labels,
                    value: (timestamp, summed_value),
                }],
            });
        }

        Err(QueryError::MetricNotFound(format!(
            "Could not process sum(irate()) query: {query}"
        )))
    }

    /// Parse a metric selector like "metric_name{label1=\"value1\",label2=\"value2\"}"
    fn parse_metric_selector(&self, selector: &str) -> Result<(String, Labels), QueryError> {
        if let Some(brace_pos) = selector.find('{') {
            let metric_name = selector[..brace_pos].trim().to_string();
            let labels_part = &selector[brace_pos + 1..selector.len() - 1];

            let mut labels = Labels::default();
            for part in labels_part.split(',') {
                let kv: Vec<&str> = part.split('=').collect();
                if kv.len() == 2 {
                    let key = kv[0].trim().to_string();
                    let value = kv[1].trim().trim_matches('"').to_string();
                    labels.inner.insert(key, value);
                }
            }
            Ok((metric_name, labels))
        } else {
            // Simple metric name
            Ok((selector.to_string(), Labels::default()))
        }
    }

    /// Get a value at a specific time from a time series
    fn get_value_at_time(
        &self,
        series: &BTreeMap<u64, f64>,
        time: Option<f64>,
    ) -> Option<(f64, f64)> {
        if series.is_empty() {
            return None;
        }

        let target_ns = if let Some(t) = time {
            (t * 1e9) as u64
        } else {
            // Use the latest value
            *series.keys().next_back()?
        };

        // Find the closest value
        if let Some((ts, val)) = series.range(..=target_ns).next_back() {
            Some((*ts as f64 / 1e9, *val))
        } else if let Some((ts, val)) = series.iter().next() {
            Some((*ts as f64 / 1e9, *val))
        } else {
            None
        }
    }

    /// Handle simple metric queries like cpu_cores or cpu_cores{cpu="0"}
    fn handle_simple_metric(
        &self,
        query: &str,
        time: Option<f64>,
    ) -> Result<QueryResult, QueryError> {
        // Parse metric name and labels
        let (metric_name, labels) = self.parse_metric_selector(query)?;

        // Try gauges first
        if let Some(collection) = self.tsdb.gauges(&metric_name, labels.clone()) {
            let sum_series = collection.filtered_sum(&labels);
            if let Some((timestamp, value)) = self.get_value_at_time(&sum_series.inner, time) {
                let mut metric_labels = HashMap::new();
                metric_labels.insert("__name__".to_string(), metric_name.to_string());

                return Ok(QueryResult::Vector {
                    result: vec![Sample {
                        metric: metric_labels,
                        value: (timestamp, value),
                    }],
                });
            }
        }

        // Try counters
        if let Some(collection) = self.tsdb.counters(&metric_name, labels.clone()) {
            // Return raw counter values
            let _filtered = collection.filter(&labels);

            // For now, just return a placeholder
            let mut metric_labels = HashMap::new();
            metric_labels.insert("__name__".to_string(), metric_name.to_string());

            return Ok(QueryResult::Vector {
                result: vec![Sample {
                    metric: metric_labels,
                    value: (time.unwrap_or(0.0), 0.0), // Placeholder
                }],
            });
        }

        Err(QueryError::MetricNotFound(format!(
            "Metric not found: {metric_name}"
        )))
    }

    /// Range queries - return multiple data points over time
    /// Handle function calls from the AST
    fn handle_function_call(
        &self,
        call: &parser::Call,
        start: f64,
        end: f64,
        step: f64,
    ) -> Result<QueryResult, QueryError> {
        match call.func.name {
            "irate" | "rate" => {
                // Both irate and rate expect a matrix selector
                if let Some(first_arg) = call.args.args.first() {
                    if let Expr::MatrixSelector(selector) = &**first_arg {
                        let metric_name = selector.vs.name.as_deref().ok_or_else(|| {
                            QueryError::ParseError("Matrix selector missing name".to_string())
                        })?;

                        // Extract label matchers from the selector
                        let mut filter_labels = Labels::default();
                        for matcher in &selector.vs.matchers.matchers {
                            // Only handle equality matchers for now
                            if matcher.op.to_string() == "=" {
                                filter_labels
                                    .inner
                                    .insert(matcher.name.clone(), matcher.value.clone());
                            }
                        }

                        // Return rate calculation for all series (not summed)
                        if let Some(collection) = self.tsdb.counters(metric_name, Labels::default())
                        {
                            // If we have a filter, use filtered_rate; otherwise get rates for all series
                            let rate_collection = if filter_labels.inner.is_empty() {
                                collection.rate() // Get rates for all series
                            } else {
                                collection.filtered_rate(&filter_labels) // Only calculate rates for matching series
                            };

                            let start_ns = (start * 1e9) as u64;
                            let end_ns = (end * 1e9) as u64;

                            let mut result_samples = Vec::new();

                            // Iterate through all individual series (already filtered)
                            for (labels, series) in rate_collection.iter() {
                                let values: Vec<(f64, f64)> = series
                                    .inner
                                    .range(start_ns..=end_ns)
                                    .map(|(ts, val)| (*ts as f64 / 1e9, *val))
                                    .collect();

                                if !values.is_empty() {
                                    // Build metric labels including the series labels
                                    let mut metric_labels = HashMap::new();
                                    metric_labels
                                        .insert("__name__".to_string(), metric_name.to_string());

                                    // Add all labels from this series
                                    for (key, value) in labels.inner.iter() {
                                        metric_labels.insert(key.clone(), value.clone());
                                    }

                                    result_samples.push(MatrixSample {
                                        metric: metric_labels,
                                        values,
                                    });
                                }
                            }

                            // If no individual series, return empty result
                            if result_samples.is_empty() {
                                return Err(QueryError::MetricNotFound(metric_name.to_string()));
                            }

                            Ok(QueryResult::Matrix {
                                result: result_samples,
                            })
                        } else {
                            Err(QueryError::MetricNotFound(metric_name.to_string()))
                        }
                    } else {
                        Err(QueryError::ParseError(format!(
                            "{} requires matrix selector argument",
                            call.func.name
                        )))
                    }
                } else {
                    Err(QueryError::ParseError(format!(
                        "{} requires an argument",
                        call.func.name
                    )))
                }
            }
            "deriv" => {
                // deriv expects a gauge range vector and calculates derivative using linear regression
                if let Some(first_arg) = call.args.args.first() {
                    if let Expr::MatrixSelector(selector) = &**first_arg {
                        let metric_name = selector.vs.name.as_deref().ok_or_else(|| {
                            QueryError::ParseError("Matrix selector missing name".to_string())
                        })?;

                        // Extract label matchers
                        let mut filter_labels = Labels::default();
                        for matcher in &selector.vs.matchers.matchers {
                            if matcher.op.to_string() == "=" {
                                filter_labels
                                    .inner
                                    .insert(matcher.name.clone(), matcher.value.clone());
                            }
                        }

                        // Try gauges first (deriv typically used on gauges or rates)
                        let result_samples = if let Some(collection) =
                            self.tsdb.gauges(metric_name, Labels::default())
                        {
                            self.calculate_deriv_for_collection(
                                collection.iter(),
                                &filter_labels,
                                start,
                                end,
                                step,
                            )?
                        } else if let Some(collection) =
                            self.tsdb.counters(metric_name, Labels::default())
                        {
                            // Also support deriv on counter rates (for 2nd derivative)
                            let rate_collection = if filter_labels.inner.is_empty() {
                                collection.rate()
                            } else {
                                collection.filtered_rate(&filter_labels)
                            };
                            self.calculate_deriv_for_rate_collection(
                                &rate_collection,
                                start,
                                end,
                                step,
                            )?
                        } else {
                            return Err(QueryError::MetricNotFound(metric_name.to_string()));
                        };

                        Ok(QueryResult::Matrix {
                            result: result_samples,
                        })
                    } else {
                        Err(QueryError::ParseError(
                            "deriv requires matrix selector argument".to_string(),
                        ))
                    }
                } else {
                    Err(QueryError::ParseError(
                        "deriv requires an argument".to_string(),
                    ))
                }
            }
            "histogram_quantile" => {
                // histogram_quantile(quantile, histogram)
                if call.args.args.len() >= 2 {
                    // First argument should be a quantile value (0.0 to 1.0)
                    let quantile = match &*call.args.args[0] {
                        Expr::NumberLiteral(num) => num.val,
                        _ => {
                            return Err(QueryError::ParseError(
                                "histogram_quantile first argument must be a number".to_string(),
                            ))
                        }
                    };

                    // Second argument should be a vector selector (histogram metric)
                    let metric_name = match &*call.args.args[1] {
                        Expr::VectorSelector(selector) => {
                            selector.name.as_deref().ok_or_else(|| {
                                QueryError::ParseError(
                                    "Vector selector missing name".to_string()
                                )
                            })?
                        }
                        _ => {
                            return Err(QueryError::ParseError(
                                "histogram_quantile second argument must be a metric name".to_string(),
                            ))
                        }
                    };

                    // Get the histogram data
                    if let Some(collection) = self.tsdb.histograms(metric_name, Labels::default()) {
                        // Sum all histogram series together
                        let summed_series = collection.sum();

                        // Calculate the percentile
                        if let Some(percentile_series) = summed_series.percentiles(&[quantile]) {
                            if let Some(series) = percentile_series.first() {
                                let start_ns = (start * 1e9) as u64;
                                let end_ns = (end * 1e9) as u64;

                                let values: Vec<(f64, f64)> = series
                                    .inner
                                    .range(start_ns..=end_ns)
                                    .map(|(ts, val)| (*ts as f64 / 1e9, *val))
                                    .collect();

                                if !values.is_empty() {
                                    let mut metric_labels = HashMap::new();
                                    metric_labels.insert("__name__".to_string(), metric_name.to_string());
                                    metric_labels.insert("quantile".to_string(), quantile.to_string());

                                    return Ok(QueryResult::Matrix {
                                        result: vec![MatrixSample {
                                            metric: metric_labels,
                                            values,
                                        }],
                                    });
                                }
                            }
                        }

                        Err(QueryError::MetricNotFound(format!(
                            "No histogram data found for {}",
                            metric_name
                        )))
                    } else {
                        Err(QueryError::MetricNotFound(metric_name.to_string()))
                    }
                } else {
                    Err(QueryError::ParseError(
                        "histogram_quantile requires 2 arguments".to_string(),
                    ))
                }
            }
            "histogram_percentiles" => {
                // histogram_percentiles(percentiles_array, histogram)
                if call.args.args.len() >= 2 {
                    // For now, reconstruct the query string and use legacy handler
                    // TODO: Implement histogram percentiles support
                    Err(QueryError::Unsupported(
                        "histogram_percentiles not yet implemented".to_string(),
                    ))
                } else {
                    Err(QueryError::ParseError(
                        "histogram_percentiles requires 2 arguments".to_string(),
                    ))
                }
            }
            _ => Err(QueryError::Unsupported(format!(
                "Function {} not yet supported",
                call.func.name
            ))),
        }
    }

    /// Handle aggregate expressions like sum, sum by, avg, etc.
    fn handle_aggregate(
        &self,
        agg: &parser::AggregateExpr,
        start: f64,
        end: f64,
        step: f64,
    ) -> Result<QueryResult, QueryError> {
        let op_str = agg.op.to_string();

        match op_str.as_str() {
            "sum" => {
                // Evaluate the inner expression first
                let inner = self.evaluate_expr(&agg.expr, start, end, step)?;

                // Check if there's a grouping modifier
                if let Some(modifier) = &agg.modifier {
                    // Handle "sum by (labels)" grouping
                    // The modifier is an enum that can be Include (for "by") or Exclude (for "without")
                    match inner {
                        QueryResult::Matrix { result: samples } => {
                            // Group samples by the specified labels
                            // Use BTreeMap as key since HashMap doesn't implement Hash
                            let mut grouped: HashMap<BTreeMap<String, String>, Vec<&MatrixSample>> =
                                HashMap::new();

                            for sample in &samples {
                                let mut group_key = BTreeMap::new();

                                // Check what kind of modifier we have
                                match modifier {
                                    parser::LabelModifier::Include(labels) => {
                                        // "sum by (labels)" - keep only specified labels
                                        for label_name in &labels.labels {
                                            if let Some(value) = sample.metric.get(label_name) {
                                                group_key.insert(label_name.clone(), value.clone());
                                            }
                                        }
                                    }
                                    parser::LabelModifier::Exclude(labels) => {
                                        // "sum without (labels)" - keep all labels except specified
                                        for (key, value) in &sample.metric {
                                            if !labels.labels.contains(key) && key != "__name__" {
                                                group_key.insert(key.clone(), value.clone());
                                            }
                                        }
                                    }
                                }

                                grouped
                                    .entry(group_key)
                                    .or_insert_with(Vec::new)
                                    .push(sample);
                            }

                            // Now aggregate each group
                            let mut result_samples = Vec::new();

                            for (group_labels, group_samples) in grouped {
                                // Collect all timestamps and sum values
                                let mut timestamp_map: BTreeMap<u64, f64> = BTreeMap::new();
                                let mut sample_count_map: BTreeMap<u64, usize> = BTreeMap::new();

                                for sample in group_samples {
                                    for (ts, val) in &sample.values {
                                        let ts_key = (*ts * 1e9) as u64;
                                        *timestamp_map.entry(ts_key).or_insert(0.0) += val;
                                        *sample_count_map.entry(ts_key).or_insert(0) += 1;
                                    }
                                }

                                // Convert back to vector
                                let result_values: Vec<(f64, f64)> = timestamp_map
                                    .into_iter()
                                    .map(|(ts_ns, val)| (ts_ns as f64 / 1e9, val))
                                    .collect();

                                if !result_values.is_empty() {
                                    // Convert BTreeMap back to HashMap for the result
                                    let mut metric_map = HashMap::new();
                                    for (k, v) in group_labels {
                                        metric_map.insert(k, v);
                                    }

                                    result_samples.push(MatrixSample {
                                        metric: metric_map,
                                        values: result_values,
                                    });
                                }
                            }

                            Ok(QueryResult::Matrix {
                                result: result_samples,
                            })
                        }
                        _ => Ok(inner),
                    }
                } else {
                    // Simple sum without grouping - aggregate all series
                    match inner {
                        QueryResult::Matrix { result: samples } => {
                            // Sum all time series together
                            if samples.is_empty() {
                                return Ok(QueryResult::Matrix { result: vec![] });
                            }

                            // Collect all timestamps
                            let mut timestamp_map: std::collections::BTreeMap<u64, f64> =
                                std::collections::BTreeMap::new();

                            for sample in &samples {
                                for (ts, val) in &sample.values {
                                    let ts_key = (*ts * 1e9) as u64;
                                    *timestamp_map.entry(ts_key).or_insert(0.0) += val;
                                }
                            }

                            // Convert back to vector
                            let result_values: Vec<(f64, f64)> = timestamp_map
                                .into_iter()
                                .map(|(ts_ns, val)| (ts_ns as f64 / 1e9, val))
                                .collect();

                            Ok(QueryResult::Matrix {
                                result: vec![MatrixSample {
                                    metric: HashMap::new(),
                                    values: result_values,
                                }],
                            })
                        }
                        _ => Ok(inner),
                    }
                }
            }
            "avg" | "min" | "max" | "count" => {
                // For other aggregations, fall back to legacy for now
                Err(QueryError::Unsupported(format!(
                    "Aggregation {} not yet fully implemented",
                    op_str
                )))
            }
            _ => Err(QueryError::Unsupported(format!(
                "Unknown aggregation: {}",
                op_str
            ))),
        }
    }

    /// Apply a binary operation to two query results
    fn apply_binary_op(
        &self,
        op: &TokenType,
        left: QueryResult,
        right: QueryResult,
    ) -> Result<QueryResult, QueryError> {
        match (left, right) {
            // Both sides are matrices (time series)
            (
                QueryResult::Matrix {
                    result: left_samples,
                },
                QueryResult::Matrix {
                    result: right_samples,
                },
            ) => {
                let mut result_samples = Vec::new();

                // Build a map of right samples by their label set for efficient matching
                let mut right_by_labels: HashMap<BTreeMap<String, String>, &MatrixSample> =
                    HashMap::new();
                for right_sample in &right_samples {
                    // Extract labels (excluding __name__)
                    let mut labels = BTreeMap::new();
                    for (k, v) in &right_sample.metric {
                        if k != "__name__" {
                            labels.insert(k.clone(), v.clone());
                        }
                    }
                    right_by_labels.insert(labels, right_sample);
                }

                for left_sample in &left_samples {
                    // Extract labels from left sample (excluding __name__)
                    let mut left_labels = BTreeMap::new();
                    for (k, v) in &left_sample.metric {
                        if k != "__name__" {
                            left_labels.insert(k.clone(), v.clone());
                        }
                    }

                    // Find matching right sample by labels
                    let right_sample = if left_labels.is_empty() && right_samples.len() == 1 {
                        // Special case: if left has no labels and right has only one series, use it
                        right_samples.first()
                    } else {
                        // Match by labels
                        let matched = right_by_labels.get(&left_labels).copied();
                        matched
                    };

                    if let Some(right_sample) = right_sample {
                        let mut result_values = Vec::new();

                        // Create a map of right values by timestamp
                        let right_map: HashMap<u64, f64> = right_sample
                            .values
                            .iter()
                            .map(|(ts, val)| ((*ts * 1e9) as u64, *val))
                            .collect();

                        for (left_ts, left_val) in &left_sample.values {
                            let ts_ns = (*left_ts * 1e9) as u64;

                            if let Some(&right_val) = right_map.get(&ts_ns) {
                                let op_str = op.to_string();
                                let result_val = match op_str.as_str() {
                                    "+" => left_val + right_val,
                                    "-" => left_val - right_val,
                                    "*" => left_val * right_val,
                                    "/" => {
                                        if right_val != 0.0 {
                                            left_val / right_val
                                        } else {
                                            continue; // Skip division by zero
                                        }
                                    }
                                    _ => {
                                        return Err(QueryError::Unsupported(format!(
                                            "Unsupported operator: {}",
                                            op_str
                                        )))
                                    }
                                };
                                result_values.push((*left_ts, result_val));
                            }
                        }

                        if !result_values.is_empty() {
                            result_samples.push(MatrixSample {
                                metric: left_sample.metric.clone(),
                                values: result_values,
                            });
                        }
                    }
                }

                Ok(QueryResult::Matrix {
                    result: result_samples,
                })
            }
            // Left is matrix, right is scalar (constant or single-value metric)
            (
                QueryResult::Matrix {
                    result: mut samples,
                },
                QueryResult::Scalar { result: scalar },
            ) => {
                let scalar_val = scalar.1;
                let op_str = op.to_string();

                for sample in &mut samples {
                    for value in &mut sample.values {
                        value.1 = match op_str.as_str() {
                            "+" => value.1 + scalar_val,
                            "-" => value.1 - scalar_val,
                            "*" => value.1 * scalar_val,
                            "/" => {
                                if scalar_val != 0.0 {
                                    value.1 / scalar_val
                                } else {
                                    continue; // Skip division by zero
                                }
                            }
                            _ => {
                                return Err(QueryError::Unsupported(format!(
                                    "Unsupported operator: {}",
                                    op_str
                                )))
                            }
                        };
                    }
                }
                Ok(QueryResult::Matrix { result: samples })
            }
            // Right is matrix, left is scalar (for commutative operations)
            (
                QueryResult::Scalar { result: scalar },
                QueryResult::Matrix {
                    result: mut samples,
                },
            ) => {
                let scalar_val = scalar.1;
                let op_str = op.to_string();

                for sample in &mut samples {
                    for value in &mut sample.values {
                        value.1 = match op_str.as_str() {
                            "+" => scalar_val + value.1,
                            "-" => scalar_val - value.1,
                            "*" => scalar_val * value.1,
                            "/" => {
                                if value.1 != 0.0 {
                                    scalar_val / value.1
                                } else {
                                    continue; // Skip division by zero
                                }
                            }
                            _ => {
                                return Err(QueryError::Unsupported(format!(
                                    "Unsupported operator: {}",
                                    op_str
                                )))
                            }
                        };
                    }
                }
                Ok(QueryResult::Matrix { result: samples })
            }
            // Handle other cases as needed
            _ => Err(QueryError::EvaluationError(
                "Incompatible operands for binary operation".to_string(),
            )),
        }
    }

    /// Evaluate an AST expression
    fn evaluate_expr(
        &self,
        expr: &Expr,
        start: f64,
        end: f64,
        step: f64,
    ) -> Result<QueryResult, QueryError> {
        match expr {
            Expr::Binary(binary) => {
                // Evaluate left and right sides
                let left = self.evaluate_expr(&binary.lhs, start, end, step)?;
                let right = self.evaluate_expr(&binary.rhs, start, end, step)?;

                // Apply the binary operation
                self.apply_binary_op(&binary.op, left, right)
            }
            Expr::VectorSelector(selector) => {
                // Handle simple metric selection
                let metric_name = selector.name.as_deref().ok_or_else(|| {
                    QueryError::ParseError("Vector selector missing name".to_string())
                })?;

                // Extract label matchers from the selector
                let mut filter_labels = Labels::default();
                for matcher in &selector.matchers.matchers {
                    // Only handle equality matchers for now
                    if matcher.op.to_string() == "=" {
                        filter_labels
                            .inner
                            .insert(matcher.name.clone(), matcher.value.clone());
                    }
                }

                // Handle simple metric selection - return all series with their labels
                // Check for gauges first
                if let Some(collection) = self.tsdb.gauges(metric_name, Labels::default()) {
                    // Special case for cpu_cores - return as scalar
                    if metric_name == "cpu_cores" {
                        let sum_series = collection.filtered_sum(&Labels::default());
                        if let Some((_ts, value)) = sum_series.inner.iter().next() {
                            return Ok(QueryResult::Scalar {
                                result: (start, *value as f64),
                            });
                        }
                    }

                    let start_ns = (start * 1e9) as u64;
                    let end_ns = (end * 1e9) as u64;

                    let mut result_samples = Vec::new();

                    // Return all series with their labels
                    for (labels, series) in collection.iter() {
                        // Skip series that don't match the filter
                        if !filter_labels.inner.is_empty() && !labels.matches(&filter_labels) {
                            continue;
                        }
                        let untyped = series.untyped();
                        let values: Vec<(f64, f64)> = untyped
                            .inner
                            .range(start_ns..=end_ns)
                            .map(|(ts, val)| (*ts as f64 / 1e9, *val as f64))
                            .collect();

                        if !values.is_empty() {
                            let mut metric_labels = HashMap::new();
                            metric_labels.insert("__name__".to_string(), metric_name.to_string());

                            // Add all labels from this series
                            for (key, value) in labels.inner.iter() {
                                metric_labels.insert(key.clone(), value.clone());
                            }

                            result_samples.push(MatrixSample {
                                metric: metric_labels,
                                values,
                            });
                        }
                    }

                    if !result_samples.is_empty() {
                        return Ok(QueryResult::Matrix {
                            result: result_samples,
                        });
                    }
                }

                // Counters should only be accessed through rate/irate functions
                // Direct access to raw counter values is not meaningful

                Err(QueryError::MetricNotFound(metric_name.to_string()))
            }
            Expr::NumberLiteral(num) => {
                // Return a scalar value
                Ok(QueryResult::Scalar {
                    result: (start, num.val),
                })
            }
            Expr::Call(call) => {
                // Handle function calls
                self.handle_function_call(call, start, end, step)
            }
            Expr::Aggregate(agg) => {
                // Handle aggregation operations like sum()
                self.handle_aggregate(agg, start, end, step)
            }
            Expr::MatrixSelector(_selector) => {
                // This shouldn't appear at top level, but handle it anyway
                Err(QueryError::Unsupported(
                    "Direct matrix selector not supported".to_string(),
                ))
            }
            _ => Err(QueryError::Unsupported(format!(
                "Unsupported expression type: {:?}",
                expr
            ))),
        }
    }

    /// Calculate derivative using linear regression for gauge collections
    fn calculate_deriv_for_collection<'a>(
        &self,
        series_iter: impl Iterator<Item = (&'a Labels, &'a GaugeSeries)>,
        filter_labels: &Labels,
        start: f64,
        end: f64,
        step: f64,
    ) -> Result<Vec<MatrixSample>, QueryError> {
        let mut result_samples = Vec::new();
        let start_ns = (start * 1e9) as u64;
        let end_ns = (end * 1e9) as u64;
        let step_ns = (step * 1e9) as u64;

        for (labels, series) in series_iter {
            // Skip series that don't match the filter
            if !filter_labels.inner.is_empty() && !labels.matches(filter_labels) {
                continue;
            }

            let untyped = series.untyped();
            let mut deriv_values = Vec::new();
            let mut current = start_ns;

            while current <= end_ns {
                // Get points in a window for linear regression
                let window_start = current.saturating_sub(step_ns * 2);
                let window_end = current + step_ns;

                let points: Vec<(f64, f64)> = untyped
                    .inner
                    .range(window_start..=window_end)
                    .map(|(ts, val)| (*ts as f64 / 1e9, *val as f64))
                    .collect();

                if points.len() >= 2 {
                    // Calculate linear regression slope (derivative)
                    let slope = self.calculate_slope(&points);
                    deriv_values.push((current as f64 / 1e9, slope));
                }
                current += step_ns;
            }

            if !deriv_values.is_empty() {
                let mut metric_labels = HashMap::new();
                metric_labels.insert("__name__".to_string(), "deriv".to_string());
                for (key, value) in labels.inner.iter() {
                    metric_labels.insert(key.clone(), value.clone());
                }
                result_samples.push(MatrixSample {
                    metric: metric_labels,
                    values: deriv_values,
                });
            }
        }

        Ok(result_samples)
    }

    /// Calculate derivative for rate collections (2nd derivative of counters)
    fn calculate_deriv_for_rate_collection(
        &self,
        rate_collection: &UntypedCollection,
        start: f64,
        end: f64,
        step: f64,
    ) -> Result<Vec<MatrixSample>, QueryError> {
        let mut result_samples = Vec::new();
        let start_ns = (start * 1e9) as u64;
        let end_ns = (end * 1e9) as u64;
        let step_ns = (step * 1e9) as u64;

        for (labels, series) in rate_collection.iter() {
            let mut deriv_values = Vec::new();
            let mut current = start_ns;

            while current <= end_ns {
                // Get points in a window for linear regression
                let window_start = current.saturating_sub(step_ns * 2);
                let window_end = current + step_ns;

                let points: Vec<(f64, f64)> = series
                    .inner
                    .range(window_start..=window_end)
                    .map(|(ts, val)| (*ts as f64 / 1e9, *val))
                    .collect();

                if points.len() >= 2 {
                    // Calculate linear regression slope (derivative)
                    let slope = self.calculate_slope(&points);
                    deriv_values.push((current as f64 / 1e9, slope));
                }
                current += step_ns;
            }

            if !deriv_values.is_empty() {
                let mut metric_labels = HashMap::new();
                metric_labels.insert("__name__".to_string(), "deriv".to_string());
                for (key, value) in labels.inner.iter() {
                    metric_labels.insert(key.clone(), value.clone());
                }
                result_samples.push(MatrixSample {
                    metric: metric_labels,
                    values: deriv_values,
                });
            }
        }

        Ok(result_samples)
    }

    /// Calculate slope using least squares linear regression
    fn calculate_slope(&self, points: &[(f64, f64)]) -> f64 {
        if points.len() < 2 {
            return 0.0;
        }

        let n = points.len() as f64;
        let mut sum_x = 0.0;
        let mut sum_y = 0.0;
        let mut sum_xy = 0.0;
        let mut sum_x2 = 0.0;

        for (x, y) in points {
            sum_x += x;
            sum_y += y;
            sum_xy += x * y;
            sum_x2 += x * x;
        }

        // Calculate slope: (n*sum_xy - sum_x*sum_y) / (n*sum_x2 - sum_x*sum_x)
        let denominator = n * sum_x2 - sum_x * sum_x;
        if denominator.abs() < 1e-10 {
            return 0.0; // Avoid division by zero
        }

        (n * sum_xy - sum_x * sum_y) / denominator
    }

    pub fn query_range(
        &self,
        query_str: &str,
        start: f64,
        end: f64,
        step: f64,
    ) -> Result<QueryResult, QueryError> {
        // Parse the query into an AST
        match parser::parse(query_str) {
            Ok(expr) => {
                // Evaluate the AST
                self.evaluate_expr(&expr, start, end, step)
            }
            Err(err) => {
                // Provide more helpful error messages for common mistakes
                let error_msg = format!("{:?}", err);
                if error_msg.contains("invalid promql query") && query_str.contains(" by ") {
                    Err(QueryError::ParseError(
                        "Invalid query syntax. Aggregation operators require parentheses around the expression, e.g., 'sum by (id) (irate(metric[5m]))' not 'sum by (id) irate(metric[5m])'".to_string()
                    ))
                } else {
                    Err(QueryError::ParseError(format!(
                        "Failed to parse query: {}",
                        error_msg
                    )))
                }
            }
        }
    }
}
