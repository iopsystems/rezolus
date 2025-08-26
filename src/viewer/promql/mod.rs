use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use thiserror::Error;

use crate::viewer::tsdb::{Labels, Tsdb};

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
    pub fn query_range(
        &self,
        query_str: &str,
        start: f64,
        end: f64,
        step: f64,
    ) -> Result<QueryResult, QueryError> {
        // Check for histogram_percentiles() function (multi-percentile)
        if query_str.starts_with("histogram_percentiles(") && query_str.ends_with(")") {
            return self.handle_histogram_percentiles_range(query_str, start, end);
        }

        // Check for histogram_quantile() function (single percentile)
        if query_str.starts_with("histogram_quantile(") && query_str.ends_with(")") {
            return self.handle_histogram_quantile_range(query_str, start, end);
        }

        // First check for simple arithmetic operations with constants
        // This needs to be before complex division check

        // Check for multiplication by constant (e.g., "... * 8")
        if let Some(mult_pos) = query_str.rfind(" * ") {
            let multiplier_str = query_str[mult_pos + 3..].trim();
            if let Ok(multiplier) = multiplier_str.parse::<f64>() {
                // Execute the base query
                let base_query = query_str[..mult_pos].trim();
                let mut result = self.query_range(base_query, start, end, step)?;

                // Apply the multiplication to all values
                if let QueryResult::Matrix {
                    result: ref mut samples,
                } = result
                {
                    for sample in samples {
                        for value in &mut sample.values {
                            value.1 *= multiplier;
                        }
                    }
                }

                return Ok(result);
            }
        }

        // Check for division operations (e.g., "A / B")
        if let Some(div_pos) = query_str.rfind(" / ") {
            let numerator_query = query_str[..div_pos].trim();
            let denominator_query = query_str[div_pos + 3..].trim();

            // Check if denominator is a simple metric (for things like "rate(cpu_usage[5m]) / cpu_cores")
            if !denominator_query.contains('(') && !denominator_query.contains('[') {
                // Check if it's a numeric constant first
                if let Ok(divisor) = denominator_query.parse::<f64>() {
                    // Division by constant
                    let mut result = self.query_range(numerator_query, start, end, step)?;

                    // Apply the division to all values
                    if let QueryResult::Matrix {
                        result: ref mut samples,
                    } = result
                    {
                        for sample in samples {
                            for value in &mut sample.values {
                                value.1 /= divisor;
                            }
                        }
                    }

                    return Ok(result);
                }

                // It's a simple metric, handle division by metric
                let numerator_result = self.query_range(numerator_query, start, end, step)?;
                let denominator_result = self.query_range(denominator_query, start, end, step)?;

                // Perform division on matching timestamps
                if let (
                    QueryResult::Matrix {
                        result: num_samples,
                    },
                    QueryResult::Matrix {
                        result: denom_samples,
                    },
                ) = (numerator_result, denominator_result)
                {
                    if !num_samples.is_empty() && !denom_samples.is_empty() {
                        let mut result_samples = Vec::new();

                        // For each numerator series
                        for num_sample in &num_samples {
                            let num_values = &num_sample.values;
                            let denom_values = &denom_samples[0].values; // Use first denominator series

                            let mut result_values = Vec::new();

                            // If denominator is a single value (like cpu_cores), use that for all
                            let single_denom_value = if denom_values.len() == 1 {
                                Some(denom_values[0].1)
                            } else {
                                None
                            };

                            if let Some(denom_val) = single_denom_value {
                                // Use single denominator value for all numerator values
                                if denom_val != 0.0 {
                                    for (num_ts, num_val) in num_values {
                                        result_values.push((*num_ts, num_val / denom_val));
                                    }
                                }
                            } else {
                                // Create a map of denominator values by timestamp for efficient lookup
                                let denom_map: std::collections::HashMap<u64, f64> = denom_values
                                    .iter()
                                    .map(|(ts, val)| ((*ts * 1e9) as u64, *val))
                                    .collect();

                                // For each numerator value, find matching denominator and divide
                                for (num_ts, num_val) in num_values {
                                    let ts_ns = (*num_ts * 1e9) as u64;

                                    // Find the closest denominator value at this timestamp
                                    if let Some(&denom_val) = denom_map.get(&ts_ns) {
                                        if denom_val != 0.0 {
                                            result_values.push((*num_ts, num_val / denom_val));
                                        }
                                    }
                                }
                            }

                            if !result_values.is_empty() {
                                result_samples.push(MatrixSample {
                                    metric: num_sample.metric.clone(),
                                    values: result_values,
                                });
                            }
                        }

                        if !result_samples.is_empty() {
                            return Ok(QueryResult::Matrix {
                                result: result_samples,
                            });
                        }
                    }
                }
            } else {
                // Complex expression division - execute both and divide
                let numerator_result = self.query_range(numerator_query, start, end, step)?;
                let denominator_result = self.query_range(denominator_query, start, end, step)?;

                // Perform division on matching timestamps
                if let (
                    QueryResult::Matrix {
                        result: num_samples,
                    },
                    QueryResult::Matrix {
                        result: denom_samples,
                    },
                ) = (numerator_result, denominator_result)
                {
                    if !num_samples.is_empty() && !denom_samples.is_empty() {
                        let mut result_samples = Vec::new();

                        // Check if we have multiple series (grouped results)
                        if num_samples.len() > 1 || denom_samples.len() > 1 {
                            // Match series by labels
                            for num_sample in &num_samples {
                                // Find matching denominator series by comparing labels
                                let matching_denom = denom_samples.iter().find(|d| {
                                    // Compare all labels except __name__
                                    num_sample
                                        .metric
                                        .iter()
                                        .filter(|(k, _)| *k != "__name__")
                                        .all(|(k, v)| d.metric.get(k) == Some(v))
                                });

                                if let Some(denom_sample) = matching_denom {
                                    let num_values = &num_sample.values;
                                    let denom_values = &denom_sample.values;

                                    let mut result_values = Vec::new();

                                    // Create a map of denominator values by timestamp
                                    let denom_map: std::collections::HashMap<u64, f64> =
                                        denom_values
                                            .iter()
                                            .map(|(ts, val)| ((*ts * 1e9) as u64, *val))
                                            .collect();

                                    // For each numerator value, find matching denominator and divide
                                    for (num_ts, num_val) in num_values {
                                        let ts_ns = (*num_ts * 1e9) as u64;

                                        if let Some(&denom_val) = denom_map.get(&ts_ns) {
                                            if denom_val != 0.0 {
                                                result_values.push((*num_ts, num_val / denom_val));
                                            }
                                        }
                                    }

                                    if !result_values.is_empty() {
                                        result_samples.push(MatrixSample {
                                            metric: num_sample.metric.clone(),
                                            values: result_values,
                                        });
                                    }
                                }
                            }
                        } else {
                            // Single series division (original logic)
                            let num_values = &num_samples[0].values;
                            let denom_values = &denom_samples[0].values;

                            let mut result_values = Vec::new();

                            // Create a map of denominator values by timestamp for efficient lookup
                            let denom_map: std::collections::HashMap<u64, f64> = denom_values
                                .iter()
                                .map(|(ts, val)| ((*ts * 1e9) as u64, *val))
                                .collect();

                            // For each numerator value, find matching denominator and divide
                            for (num_ts, num_val) in num_values {
                                let ts_ns = (*num_ts * 1e9) as u64;

                                // Find the closest denominator value at this timestamp
                                if let Some(&denom_val) = denom_map.get(&ts_ns) {
                                    if denom_val != 0.0 {
                                        result_values.push((*num_ts, num_val / denom_val));
                                    }
                                }
                            }

                            if !result_values.is_empty() {
                                let mut metric_labels = HashMap::new();
                                metric_labels
                                    .insert("__name__".to_string(), "division_result".to_string());

                                result_samples.push(MatrixSample {
                                    metric: metric_labels,
                                    values: result_values,
                                });
                            }
                        }

                        if !result_samples.is_empty() {
                            return Ok(QueryResult::Matrix {
                                result: result_samples,
                            });
                        }
                    }
                }
            }

            return Err(QueryError::EvaluationError(
                "Division failed: incompatible results".to_string(),
            ));
        }

        // Check for sum by() function (e.g., "sum by (cpu) (rate(...))")
        if query_str.starts_with("sum by ") || query_str.starts_with("sum by(") {
            return self.handle_sum_by_range(query_str, start, end);
        }

        // Check for irate() function
        if query_str.starts_with("irate(") && query_str.ends_with(")") {
            return self.handle_rate_range(query_str, start, end);
        }

        // Check for sum(irate()) function
        if query_str.starts_with("sum(irate(") && query_str.ends_with("))") {
            return self.handle_sum_rate_range(query_str, start, end);
        }

        // Check for sum() function
        if query_str.starts_with("sum(") && query_str.ends_with(")") {
            return self.handle_sum_range(query_str, start, end);
        }

        // Handle simple metric queries
        let (metric_name, labels) = self.parse_metric_selector(query_str)?;

        // Try gauges first
        if let Some(collection) = self.tsdb.gauges(&metric_name, labels.clone()) {
            // If no labels specified, return all series separately
            if labels.inner.is_empty() {
                let mut result_samples = Vec::new();
                let start_ns = (start * 1e9) as u64;
                let end_ns = (end * 1e9) as u64;

                for (series_labels, series) in collection.iter() {
                    let untyped = series.untyped();
                    let values: Vec<(f64, f64)> = untyped
                        .inner
                        .range(start_ns..=end_ns)
                        .map(|(ts, val)| (*ts as f64 / 1e9, *val))
                        .collect();

                    if !values.is_empty() {
                        let mut metric_labels = HashMap::new();
                        metric_labels.insert("__name__".to_string(), metric_name.to_string());

                        // Add all labels from this series
                        for (key, value) in series_labels.inner.iter() {
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
            } else {
                // Labels specified, filter and sum matching series
                let sum_series = collection.filtered_sum(&labels);

                // Get all data points within the time range
                let start_ns = (start * 1e9) as u64;
                let end_ns = (end * 1e9) as u64;

                let values: Vec<(f64, f64)> = sum_series
                    .inner
                    .range(start_ns..=end_ns)
                    .map(|(ts, val)| (*ts as f64 / 1e9, *val))
                    .collect();

                // For constant gauges like cpu_cores, if we have any value, replicate it across the time range
                let final_values = if values.len() == 1 && metric_name == "cpu_cores" {
                    // Create a value for each step in the time range using the single value
                    let const_value = values[0].1;
                    let mut expanded = Vec::new();
                    let mut current = start;
                    while current <= end {
                        expanded.push((current, const_value));
                        current += step;
                    }
                    expanded
                } else {
                    values
                };

                if !final_values.is_empty() {
                    let mut metric_labels = HashMap::new();
                    metric_labels.insert("__name__".to_string(), metric_name.to_string());

                    // Add the labels from the query to the result
                    for (key, value) in labels.inner.iter() {
                        metric_labels.insert(key.clone(), value.clone());
                    }

                    return Ok(QueryResult::Matrix {
                        result: vec![MatrixSample {
                            metric: metric_labels,
                            values: final_values,
                        }],
                    });
                }
            }
        }

        // Try counters - return rate values
        if let Some(collection) = self.tsdb.counters(&metric_name, labels.clone()) {
            // If no labels specified, return all series separately
            if labels.inner.is_empty() {
                let rate_collection = collection.rate();
                let mut result_samples = Vec::new();
                let start_ns = (start * 1e9) as u64;
                let end_ns = (end * 1e9) as u64;

                for (series_labels, series) in rate_collection.iter() {
                    let values: Vec<(f64, f64)> = series
                        .inner
                        .range(start_ns..=end_ns)
                        .map(|(ts, val)| (*ts as f64 / 1e9, *val))
                        .collect();

                    if !values.is_empty() {
                        let mut metric_labels = HashMap::new();
                        metric_labels.insert("__name__".to_string(), metric_name.to_string());

                        // Add all labels from this series
                        for (key, value) in series_labels.inner.iter() {
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
            } else {
                // Labels specified, filter and sum matching series
                let rate_collection = collection.filtered_rate(&labels);
                let sum_series = rate_collection.sum();

                let start_ns = (start * 1e9) as u64;
                let end_ns = (end * 1e9) as u64;

                let values: Vec<(f64, f64)> = sum_series
                    .inner
                    .range(start_ns..=end_ns)
                    .map(|(ts, val)| (*ts as f64 / 1e9, *val))
                    .collect();

                if !values.is_empty() {
                    let mut metric_labels = HashMap::new();
                    metric_labels.insert("__name__".to_string(), metric_name.to_string());

                    // Add the labels from the query to the result
                    for (key, value) in labels.inner.iter() {
                        metric_labels.insert(key.clone(), value.clone());
                    }

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
            "Metric not found: {metric_name}"
        )))
    }

    /// Handle histogram_quantile() queries over a time range
    fn handle_histogram_quantile_range(
        &self,
        query: &str,
        start: f64,
        end: f64,
    ) -> Result<QueryResult, QueryError> {
        // Parse histogram_quantile(0.95, metric_name) or histogram_quantile(0.95, rate(metric_name[5m]))
        let inner = &query[19..query.len() - 1]; // Remove "histogram_quantile(" and ")"

        // Find the comma separating quantile from metric
        if let Some(comma_pos) = inner.find(',') {
            let quantile_str = inner[..comma_pos].trim();
            let metric_part = inner[comma_pos + 1..].trim();

            if let Ok(quantile) = quantile_str.parse::<f64>() {
                if !(0.0..=1.0).contains(&quantile) {
                    return Err(QueryError::EvaluationError(format!(
                        "Quantile must be between 0 and 1, got {quantile}"
                    )));
                }

                // Check if it's an irate() query
                let (metric_name, labels) =
                    if metric_part.starts_with("irate(") && metric_part.ends_with(")") {
                        // Extract metric from irate()
                        let rate_inner = &metric_part[6..metric_part.len() - 1];
                        if let Some(bracket_pos) = rate_inner.find('[') {
                            let metric_selector = rate_inner[..bracket_pos].trim();
                            self.parse_metric_selector(metric_selector)?
                        } else {
                            return Err(QueryError::ParseError(
                                "Invalid irate() in histogram_quantile".to_string(),
                            ));
                        }
                    } else {
                        // Direct metric reference
                        self.parse_metric_selector(metric_part)?
                    };

                // Get histogram data and calculate percentiles
                if let Some(collection) = self.tsdb.histograms(&metric_name, labels.clone()) {
                    let histogram = collection.sum();

                    // Calculate the percentile for each timestamp
                    if let Some(percentile_series_vec) = histogram.percentiles(&[quantile * 100.0])
                    {
                        if let Some(percentile_series) = percentile_series_vec.first() {
                            let start_ns = (start * 1e9) as u64;
                            let end_ns = (end * 1e9) as u64;

                            let values: Vec<(f64, f64)> = percentile_series
                                .inner
                                .range(start_ns..=end_ns)
                                .map(|(ts, val)| (*ts as f64 / 1e9, *val))
                                .collect();

                            if !values.is_empty() {
                                let mut metric_labels = HashMap::new();
                                metric_labels
                                    .insert("__name__".to_string(), metric_name.to_string());
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
                }

                return Err(QueryError::MetricNotFound(format!(
                    "No histogram data found for: {metric_name}"
                )));
            }
        }

        Err(QueryError::ParseError(format!(
            "Invalid histogram_quantile syntax: {query}"
        )))
    }

    /// Handle histogram_percentiles() queries over a time range - computes multiple percentiles efficiently
    fn handle_histogram_percentiles_range(
        &self,
        query: &str,
        start: f64,
        end: f64,
    ) -> Result<QueryResult, QueryError> {
        // Parse histogram_percentiles([0.5, 0.9, 0.99, 0.999], metric_name) or histogram_percentiles([0.5, 0.9, 0.99, 0.999], rate(metric_name[5m]))
        let inner = &query[22..query.len() - 1]; // Remove "histogram_percentiles(" and ")"

        // Find the comma separating percentiles array from metric
        if let Some(comma_pos) = inner.find("], ") {
            let percentiles_str = &inner[1..comma_pos]; // Remove opening bracket
            let metric_part = inner[comma_pos + 3..].trim(); // Skip "], "

            // Parse the percentiles array
            let percentiles: Result<Vec<f64>, _> = percentiles_str
                .split(',')
                .map(|s| s.trim().parse::<f64>())
                .collect();

            match percentiles {
                Ok(percentiles) => {
                    // Validate percentiles are in valid range
                    for &p in &percentiles {
                        if !(0.0..=1.0).contains(&p) {
                            return Err(QueryError::EvaluationError(format!(
                                "Percentile must be between 0 and 1, got {p}"
                            )));
                        }
                    }

                    // Convert to 0-100 scale for the underlying histogram percentiles() method
                    let percentiles_100: Vec<f64> =
                        percentiles.iter().map(|&p| p * 100.0).collect();

                    // Check if it's an irate() query
                    let (metric_name, labels) =
                        if metric_part.starts_with("irate(") && metric_part.ends_with(")") {
                            // Extract metric from irate()
                            let rate_inner = &metric_part[6..metric_part.len() - 1];
                            if let Some(bracket_pos) = rate_inner.find('[') {
                                let metric_selector = rate_inner[..bracket_pos].trim();
                                self.parse_metric_selector(metric_selector)?
                            } else {
                                return Err(QueryError::ParseError(
                                    "Invalid irate() in histogram_percentiles".to_string(),
                                ));
                            }
                        } else {
                            // Direct metric reference
                            self.parse_metric_selector(metric_part)?
                        };

                    // Get histogram data and calculate percentiles
                    if let Some(collection) = self.tsdb.histograms(&metric_name, labels.clone()) {
                        let histogram = collection.sum();

                        // Calculate all percentiles in one pass
                        if let Some(percentile_series_vec) = histogram.percentiles(&percentiles_100)
                        {
                            let start_ns = (start * 1e9) as u64;
                            let end_ns = (end * 1e9) as u64;

                            let mut result_samples = Vec::new();

                            // Create a series for each percentile
                            for (i, percentile_series) in percentile_series_vec.iter().enumerate() {
                                let values: Vec<(f64, f64)> = percentile_series
                                    .inner
                                    .range(start_ns..=end_ns)
                                    .map(|(ts, val)| (*ts as f64 / 1e9, *val))
                                    .collect();

                                if !values.is_empty() {
                                    let mut metric_labels = HashMap::new();
                                    metric_labels
                                        .insert("__name__".to_string(), metric_name.to_string());
                                    metric_labels
                                        .insert("quantile".to_string(), percentiles[i].to_string());

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
                    }

                    return Err(QueryError::MetricNotFound(format!(
                        "No histogram data found for: {metric_name}"
                    )));
                }
                Err(_) => {
                    return Err(QueryError::ParseError(
                        "Invalid percentiles array in histogram_percentiles".to_string(),
                    ));
                }
            }
        }

        Err(QueryError::ParseError(format!(
            "Invalid histogram_percentiles syntax: {query}"
        )))
    }

    /// Handle irate() queries over a time range
    fn handle_rate_range(
        &self,
        query: &str,
        start: f64,
        end: f64,
    ) -> Result<QueryResult, QueryError> {
        // Extract metric name from irate(metric_name[duration])
        let inner = &query[6..query.len() - 1]; // Remove "irate(" and ")"

        if let Some(bracket_pos) = inner.find('[') {
            let metric_part = inner[..bracket_pos].trim();
            let (metric_name, labels) = self.parse_metric_selector(metric_part)?;

            if let Some(collection) = self.tsdb.counters(&metric_name, labels.clone()) {
                let rate_collection = collection.filtered_rate(&labels);

                // If no specific labels were provided, return all series separately
                // Otherwise, sum matching series
                if labels.inner.is_empty() {
                    // Return all series separately
                    let mut result_samples = Vec::new();
                    let start_ns = (start * 1e9) as u64;
                    let end_ns = (end * 1e9) as u64;

                    for (series_labels, series) in rate_collection.iter() {
                        let values: Vec<(f64, f64)> = series
                            .inner
                            .range(start_ns..=end_ns)
                            .map(|(ts, val)| (*ts as f64 / 1e9, *val))
                            .collect();

                        if !values.is_empty() {
                            let mut metric_labels = HashMap::new();
                            metric_labels.insert("__name__".to_string(), metric_name.to_string());

                            // Add all labels from this series
                            for (key, value) in series_labels.inner.iter() {
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
                } else {
                    // Labels specified, sum matching series
                    let sum_series = rate_collection.sum();

                    let start_ns = (start * 1e9) as u64;
                    let end_ns = (end * 1e9) as u64;

                    let values: Vec<(f64, f64)> = sum_series
                        .inner
                        .range(start_ns..=end_ns)
                        .map(|(ts, val)| (*ts as f64 / 1e9, *val))
                        .collect();

                    if !values.is_empty() {
                        let mut metric_labels = HashMap::new();
                        metric_labels.insert("__name__".to_string(), metric_name.to_string());

                        // Add the labels from the query to the result
                        for (key, value) in labels.inner.iter() {
                            metric_labels.insert(key.clone(), value.clone());
                        }

                        return Ok(QueryResult::Matrix {
                            result: vec![MatrixSample {
                                metric: metric_labels,
                                values,
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

    /// Handle sum(irate()) queries over a time range
    fn handle_sum_rate_range(
        &self,
        query: &str,
        start: f64,
        end: f64,
    ) -> Result<QueryResult, QueryError> {
        // Extract the irate() part
        if query.starts_with("sum(irate(") && query.ends_with("))") {
            let rate_part = &query[4..query.len() - 1]; // Remove "sum(" and ")"

            // First get the rate results
            let rate_result = self.handle_rate_range(rate_part, start, end)?;

            // Sum all the series together
            if let QueryResult::Matrix { result: samples } = rate_result {
                if samples.is_empty() {
                    return Err(QueryError::MetricNotFound(
                        "No data found for sum(irate()) query".to_string(),
                    ));
                }

                // Create a map to sum values at each timestamp across all series
                let mut summed_values: std::collections::BTreeMap<u64, f64> =
                    std::collections::BTreeMap::new();

                for sample in &samples {
                    for (timestamp, value) in &sample.values {
                        let ts_ns = (*timestamp * 1e9) as u64;
                        *summed_values.entry(ts_ns).or_insert(0.0) += value;
                    }
                }

                // Convert back to vector of (timestamp, value) pairs
                let final_values: Vec<(f64, f64)> = summed_values
                    .into_iter()
                    .map(|(ts_ns, val)| (ts_ns as f64 / 1e9, val))
                    .collect();

                if !final_values.is_empty() {
                    // Extract metric name from the first sample
                    let metric_name = samples[0]
                        .metric
                        .get("__name__")
                        .cloned()
                        .unwrap_or_else(|| "sum".to_string());

                    let mut metric_labels = HashMap::new();
                    metric_labels.insert("__name__".to_string(), metric_name);

                    return Ok(QueryResult::Matrix {
                        result: vec![MatrixSample {
                            metric: metric_labels,
                            values: final_values,
                        }],
                    });
                }
            }

            Err(QueryError::MetricNotFound(format!(
                "Could not process sum(irate()) query: {query}"
            )))
        } else {
            Err(QueryError::ParseError(format!(
                "Invalid sum(irate()) query: {query}"
            )))
        }
    }

    /// Handle sum() queries over a time range
    fn handle_sum_range(
        &self,
        query: &str,
        start: f64,
        end: f64,
    ) -> Result<QueryResult, QueryError> {
        // Extract metric from sum(metric)
        let inner = &query[4..query.len() - 1]; // Remove "sum(" and ")"
        let (metric_name, labels) = self.parse_metric_selector(inner)?;

        // Try gauges
        if let Some(collection) = self.tsdb.gauges(&metric_name, labels.clone()) {
            let sum_series = collection.filtered_sum(&labels);

            let start_ns = (start * 1e9) as u64;
            let end_ns = (end * 1e9) as u64;

            let values: Vec<(f64, f64)> = sum_series
                .inner
                .range(start_ns..=end_ns)
                .map(|(ts, val)| (*ts as f64 / 1e9, *val))
                .collect();

            if !values.is_empty() {
                let mut metric_labels = HashMap::new();
                metric_labels.insert("__name__".to_string(), metric_name.to_string());

                return Ok(QueryResult::Matrix {
                    result: vec![MatrixSample {
                        metric: metric_labels,
                        values,
                    }],
                });
            }
        }

        // Try counters
        if let Some(collection) = self.tsdb.counters(&metric_name, labels.clone()) {
            let rate_collection = collection.filtered_rate(&labels);
            let sum_series = rate_collection.sum();

            let start_ns = (start * 1e9) as u64;
            let end_ns = (end * 1e9) as u64;

            let values: Vec<(f64, f64)> = sum_series
                .inner
                .range(start_ns..=end_ns)
                .map(|(ts, val)| (*ts as f64 / 1e9, *val))
                .collect();

            if !values.is_empty() {
                let mut metric_labels = HashMap::new();
                metric_labels.insert("__name__".to_string(), metric_name.to_string());

                return Ok(QueryResult::Matrix {
                    result: vec![MatrixSample {
                        metric: metric_labels,
                        values,
                    }],
                });
            }
        }

        Err(QueryError::MetricNotFound(format!(
            "Metric not found for sum: {metric_name}"
        )))
    }

    /// Handle sum by() queries over a time range (e.g., "sum by (cpu) (rate(cpu_usage[5m]))")
    fn handle_sum_by_range(
        &self,
        query: &str,
        start: f64,
        end: f64,
    ) -> Result<QueryResult, QueryError> {
        // Parse "sum by (label1, label2) (inner_query)" or "sum by(label1, label2) (inner_query)"

        // Find where the by clause starts - after "sum by"
        let by_clause_start = if query.starts_with("sum by(") {
            "sum by(".len()
        } else if query.starts_with("sum by (") {
            "sum by (".len()
        } else {
            return Err(QueryError::ParseError("Invalid sum by syntax".to_string()));
        };

        // Find the closing parenthesis for the by clause
        let mut paren_count = 1;
        let mut by_end = by_clause_start;
        for (i, ch) in query[by_clause_start..].chars().enumerate() {
            match ch {
                '(' => paren_count += 1,
                ')' => {
                    paren_count -= 1;
                    if paren_count == 0 {
                        by_end = by_clause_start + i;
                        break;
                    }
                }
                _ => {}
            }
        }

        if paren_count != 0 {
            return Err(QueryError::ParseError(
                "Unmatched parentheses in by clause".to_string(),
            ));
        }

        // Extract the grouping labels
        let by_labels_str = &query[by_clause_start..by_end].trim();
        let group_by_labels: Vec<String> = by_labels_str
            .split(',')
            .map(|s| s.trim().to_string())
            .collect();

        // Extract the inner query (everything after the by clause)
        let inner_query_start = query[by_end + 1..].trim_start();
        if !inner_query_start.starts_with('(') || !inner_query_start.ends_with(')') {
            return Err(QueryError::ParseError(
                "Expected (query) after by clause".to_string(),
            ));
        }

        let inner_query = &inner_query_start[1..inner_query_start.len() - 1];

        // For now, handle irate() as the inner query
        if inner_query.starts_with("irate(") && inner_query.ends_with(")") {
            let rate_inner = &inner_query[6..inner_query.len() - 1]; // Remove "irate(" and ")"

            if let Some(bracket_pos) = rate_inner.find('[') {
                let metric_part = rate_inner[..bracket_pos].trim();
                let (metric_name, filter_labels) = self.parse_metric_selector(metric_part)?;

                // Get counters and group by the specified labels
                if let Some(collection) = self.tsdb.counters(&metric_name, filter_labels.clone()) {
                    // Get rate collection
                    let rate_collection = collection.filtered_rate(&filter_labels);

                    // Group by the specified labels
                    let grouped = rate_collection.group_by(&group_by_labels);

                    let start_ns = (start * 1e9) as u64;
                    let end_ns = (end * 1e9) as u64;

                    let mut result_samples = Vec::new();

                    for (label_values, series) in grouped {
                        let values: Vec<(f64, f64)> = series
                            .inner
                            .range(start_ns..=end_ns)
                            .map(|(ts, val)| (*ts as f64 / 1e9, *val))
                            .collect();

                        if !values.is_empty() {
                            let mut metric_labels = HashMap::new();
                            metric_labels.insert("__name__".to_string(), metric_name.to_string());

                            // Add the grouped label values
                            for (label_name, label_value) in label_values {
                                metric_labels.insert(label_name, label_value);
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
            }
        }

        // Also handle simple metrics
        let (metric_name, filter_labels) = self.parse_metric_selector(inner_query)?;

        // Try counters first (without rate)
        if let Some(collection) = self.tsdb.counters(&metric_name, filter_labels.clone()) {
            // For counters without irate(), we still need to compute rate
            let rate_collection = if filter_labels.inner.is_empty() {
                collection.rate()
            } else {
                collection.filtered_rate(&filter_labels)
            };

            let grouped = rate_collection.group_by(&group_by_labels);

            let start_ns = (start * 1e9) as u64;
            let end_ns = (end * 1e9) as u64;

            let mut result_samples = Vec::new();

            for (label_values, series) in grouped {
                let values: Vec<(f64, f64)> = series
                    .inner
                    .range(start_ns..=end_ns)
                    .map(|(ts, val)| (*ts as f64 / 1e9, *val))
                    .collect();

                if !values.is_empty() {
                    let mut metric_labels = HashMap::new();
                    metric_labels.insert("__name__".to_string(), metric_name.to_string());

                    // Add the grouped label values
                    for (label_name, label_value) in label_values {
                        metric_labels.insert(label_name, label_value);
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

        // Try gauges
        if let Some(collection) = self.tsdb.gauges(&metric_name, filter_labels.clone()) {
            let grouped = collection.group_by(&group_by_labels, &filter_labels);

            let start_ns = (start * 1e9) as u64;
            let end_ns = (end * 1e9) as u64;

            let mut result_samples = Vec::new();

            for (label_values, series) in grouped {
                let values: Vec<(f64, f64)> = series
                    .inner
                    .range(start_ns..=end_ns)
                    .map(|(ts, val)| (*ts as f64 / 1e9, *val))
                    .collect();

                if !values.is_empty() {
                    let mut metric_labels = HashMap::new();
                    metric_labels.insert("__name__".to_string(), metric_name.to_string());

                    // Add the grouped label values
                    for (label_name, label_value) in label_values {
                        metric_labels.insert(label_name, label_value);
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

        Err(QueryError::MetricNotFound(format!(
            "Could not process sum by query: {query}"
        )))
    }

    fn get_value_at_time(
        &self,
        series: &std::collections::BTreeMap<u64, f64>,
        time: Option<f64>,
    ) -> Option<(f64, f64)> {
        if series.is_empty() {
            return None;
        }

        if let Some(target_time_seconds) = time {
            // Find the value at or before the target time
            let target_time_ns = (target_time_seconds * 1e9) as u64;

            // Find the last entry <= target time
            let entry = series
                .range(..=target_time_ns)
                .next_back()
                .or_else(|| series.iter().next());

            entry.map(|(ts, val)| (*ts as f64 / 1e9, *val))
        } else {
            // Return the last value
            series
                .iter()
                .next_back()
                .map(|(ts, val)| (*ts as f64 / 1e9, *val))
        }
    }

    /// Parse a metric selector like "metric_name" or "metric_name{label1=\"value1\",label2=\"value2\"}"
    fn parse_metric_selector(&self, selector: &str) -> Result<(String, Labels), QueryError> {
        let selector = selector.trim();

        if let Some(brace_pos) = selector.find('{') {
            // Handle metric_name{labels}
            if !selector.ends_with('}') {
                return Err(QueryError::MetricNotFound(
                    "Invalid label selector format".to_string(),
                ));
            }

            let metric_name = selector[..brace_pos].trim().to_string();
            let labels_str = &selector[brace_pos + 1..selector.len() - 1]; // Remove { and }
            let mut labels = Labels::default();

            if !labels_str.trim().is_empty() {
                // Parse labels like label1="value1",label2="value2"
                // Need to handle commas inside quoted values
                let mut label_pairs = Vec::new();
                let mut current_pair = String::new();
                let mut in_quotes = false;
                let mut quote_char = ' ';

                for c in labels_str.chars() {
                    match c {
                        '"' | '\'' if !in_quotes => {
                            in_quotes = true;
                            quote_char = c;
                            current_pair.push(c);
                        }
                        c if in_quotes && c == quote_char => {
                            in_quotes = false;
                            current_pair.push(c);
                        }
                        ',' if !in_quotes => {
                            if !current_pair.trim().is_empty() {
                                label_pairs.push(current_pair.trim().to_string());
                                current_pair.clear();
                            }
                        }
                        _ => {
                            current_pair.push(c);
                        }
                    }
                }

                // Don't forget the last pair
                if !current_pair.trim().is_empty() {
                    label_pairs.push(current_pair.trim().to_string());
                }

                for label_pair in label_pairs {
                    if let Some(eq_pos) = label_pair.find("!~") {
                        // Negative regex match operator
                        let key = label_pair[..eq_pos].trim();
                        let value_with_quotes = label_pair[eq_pos + 2..].trim(); // Skip "!~"

                        // Remove quotes if present
                        let value = if (value_with_quotes.starts_with('"')
                            && value_with_quotes.ends_with('"'))
                            || (value_with_quotes.starts_with('\'')
                                && value_with_quotes.ends_with('\''))
                        {
                            &value_with_quotes[1..value_with_quotes.len() - 1]
                        } else {
                            value_with_quotes
                        };

                        // Store with a special prefix to indicate negative match
                        labels.inner.insert(key.to_string(), format!("!{value}"));
                    } else if let Some(eq_pos) = label_pair.find("=~") {
                        // Regex match operator
                        let key = label_pair[..eq_pos].trim();
                        let value_with_quotes = label_pair[eq_pos + 2..].trim(); // Skip "=~"

                        // Remove quotes if present
                        let value = if (value_with_quotes.starts_with('"')
                            && value_with_quotes.ends_with('"'))
                            || (value_with_quotes.starts_with('\'')
                                && value_with_quotes.ends_with('\''))
                        {
                            &value_with_quotes[1..value_with_quotes.len() - 1]
                        } else {
                            value_with_quotes
                        };

                        // Store the regex pattern - the matches() method will handle it
                        labels.inner.insert(key.to_string(), value.to_string());
                    } else if let Some(eq_pos) = label_pair.find('=') {
                        // Exact match operator
                        let key = label_pair[..eq_pos].trim();
                        let value_with_quotes = label_pair[eq_pos + 1..].trim();

                        // Remove quotes if present
                        let value = if (value_with_quotes.starts_with('"')
                            && value_with_quotes.ends_with('"'))
                            || (value_with_quotes.starts_with('\'')
                                && value_with_quotes.ends_with('\''))
                        {
                            &value_with_quotes[1..value_with_quotes.len() - 1]
                        } else {
                            value_with_quotes
                        };

                        // Store the value for exact matching
                        labels.inner.insert(key.to_string(), value.to_string());
                    }
                }
            }
            Ok((metric_name, labels))
        } else {
            // Simple metric name
            Ok((selector.to_string(), Labels::default()))
        }
    }
}
