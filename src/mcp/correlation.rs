use crate::viewer::promql::{MatrixSample, QueryEngine, QueryResult};
use rayon::prelude::*;
use std::collections::HashMap;
use std::sync::Arc;

// Fixed time intervals optimized for system performance analysis
// Balances coverage with computational efficiency
const TIME_LAGS: &[i32] = &[
    // Immediate effects (key correlations happen here)
    1, 2, 3, 5, 10, // Short-term effects (every 5s)
    15, 20, 25, 30, // Medium-term effects (every 10s)
    40, 50, 60, // Longer-term effects (every 30s)
    90, 120, 150, 180, 210, 240, 270, 300,
];

#[derive(Debug, Clone)]
pub struct CorrelationResult {
    pub metric1: String,
    pub metric2: String,
    pub metric1_name: Option<String>, // Human-readable name
    pub metric2_name: Option<String>, // Human-readable name
    pub max_correlation: f64,
    pub optimal_lag: i64, // Lag in seconds (positive = metric2 lags metric1)
    pub sample_count: usize,
    pub correlations_at_lag: Vec<LagCorrelation>,
    pub series_pairs: Vec<SeriesCorrelation>,
}

#[derive(Debug, Clone)]
pub struct LagCorrelation {
    pub lag: i64,
    pub correlation: f64,
}

#[derive(Debug, Clone)]
pub struct SeriesCorrelation {
    pub labels1: HashMap<String, String>,
    pub labels2: HashMap<String, String>,
    pub max_correlation: f64,
    pub optimal_lag: i64,
    pub sample_count: usize,
}

/// Calculate cross-correlation between two PromQL expressions
///
/// This handles various cases:
/// - Simple metrics: `cpu_usage` vs `memory_used`
/// - Rate queries: `irate(cpu_cycles[5m])` vs `irate(instructions[5m])`
/// - Aggregations: `sum by (name) (irate(cgroup_cpu_usage[5m]))` vs `sum by (id) (irate(cpu_usage[5m]))`
/// - Complex expressions: `cpu_usage / cpu_total` vs `memory_used / memory_total`
///
/// It also detects lag relationships - for example, if memory pressure leads to
/// increased CPU usage due to garbage collection after a delay.
pub fn calculate_correlation(
    engine: &Arc<QueryEngine>,
    expr1: &str,
    expr2: &str,
    start: f64,
    end: f64,
    step: f64,
) -> Result<CorrelationResult, Box<dyn std::error::Error>> {
    // For complex expressions, use the expression itself as the "name"
    calculate_correlation_with_names(
        engine,
        expr1,
        expr2,
        Some(expr1),
        Some(expr2),
        start,
        end,
        step,
    )
}

/// Calculate cross-correlation with optional human-readable names
pub fn calculate_correlation_with_names(
    engine: &Arc<QueryEngine>,
    expr1: &str,
    expr2: &str,
    name1: Option<&str>,
    name2: Option<&str>,
    start: f64,
    end: f64,
    step: f64,
) -> Result<CorrelationResult, Box<dyn std::error::Error>> {
    // Query both expressions
    let result1 = engine.query_range(expr1, start, end, step)?;
    let result2 = engine.query_range(expr2, start, end, step)?;

    // Extract matrix samples
    let samples1 = extract_matrix_samples(&result1)?;
    let samples2 = extract_matrix_samples(&result2)?;

    if samples1.is_empty() || samples2.is_empty() {
        return Err("No data returned from queries".into());
    }

    // Calculate cross-correlations between all series pairs in parallel
    let series_results: Vec<_> = samples1
        .par_iter()
        .flat_map(|s1| {
            samples2.par_iter().filter_map(move |s2| {
                calculate_series_cross_correlation(s1, s2, step).map(|corr| {
                    (
                        SeriesCorrelation {
                            labels1: s1.metric.clone(),
                            labels2: s2.metric.clone(),
                            max_correlation: corr.max_correlation,
                            optimal_lag: corr.optimal_lag,
                            sample_count: corr.sample_count,
                        },
                        (corr.max_correlation, corr.optimal_lag, corr.sample_count),
                    )
                })
            })
        })
        .collect();

    let mut series_pairs: Vec<SeriesCorrelation> = Vec::new();
    let mut all_correlations: Vec<(f64, i64, usize)> = Vec::new();

    for (series_corr, corr_tuple) in series_results {
        series_pairs.push(series_corr);
        all_correlations.push(corr_tuple);
    }

    if all_correlations.is_empty() {
        return Err("No valid correlations found (insufficient overlapping data)".into());
    }

    // Find the best overall correlation (highest absolute value)
    let (max_correlation, optimal_lag, total_samples) = if all_correlations.len() == 1 {
        all_correlations[0]
    } else {
        // For multiple series pairs, find the strongest correlation
        all_correlations
            .iter()
            .max_by(|a, b| {
                a.0.abs()
                    .partial_cmp(&b.0.abs())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .cloned()
            .unwrap_or((0.0, 0, 0))
    };

    // Calculate correlations at different lags for the overall result
    let correlations_at_lag = if samples1.len() == 1 && samples2.len() == 1 {
        calculate_lag_correlations(&samples1[0], &samples2[0], step)
    } else {
        vec![]
    };

    // Sort series pairs by absolute correlation
    series_pairs.sort_by(|a, b| {
        b.max_correlation
            .abs()
            .partial_cmp(&a.max_correlation.abs())
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    Ok(CorrelationResult {
        metric1: expr1.to_string(),
        metric2: expr2.to_string(),
        metric1_name: name1.map(|s| s.to_string()),
        metric2_name: name2.map(|s| s.to_string()),
        max_correlation,
        optimal_lag,
        sample_count: total_samples,
        correlations_at_lag,
        series_pairs,
    })
}

/// Calculate cross-correlation between two specific time series
fn calculate_series_cross_correlation(
    series1: &MatrixSample,
    series2: &MatrixSample,
    step: f64,
) -> Option<CrossCorrelationResult> {
    // Prepare the data
    let data1 = prepare_series_data(series1);
    let data2 = prepare_series_data(series2);

    if data1.len() < 10 || data2.len() < 10 {
        return None; // Need sufficient data for meaningful correlation
    }

    // Calculate the maximum lag based on recording duration
    let series_len = data1.len().min(data2.len());
    let duration_seconds = series_len as f64 * step;

    // Determine max lag to test:
    // - Start with the highest configured lag (e.g., 300s from TIME_LAGS)
    // - Cap at 1/3 of recording duration (for statistical validity)
    let max_configured_lag = *TIME_LAGS.last().unwrap_or(&300) as f64;
    let one_third_duration = duration_seconds / 3.0;

    // Take the smaller of: configured max or 1/3 duration
    let max_lag_seconds = max_configured_lag.min(one_third_duration);

    let max_lag = ((max_lag_seconds / step) as usize).max(1);

    // Find optimal lag using cross-correlation
    // For efficiency, we sample lags rather than testing every single one
    let mut best_correlation: f64 = 0.0;
    let mut best_lag = 0i64;
    let mut best_sample_count = 0;

    // Use a consistent set of time lags regardless of sampling rate
    // This ensures comparable results across different data resolutions
    let mut samples = vec![0]; // Always test zero lag

    // Convert time points to lag samples based on step size
    for &seconds in TIME_LAGS {
        let lag = (seconds as f64 / step) as i64; // Truncate towards zero
        if lag > 0 && lag <= max_lag as i64 {
            samples.push(lag);
            samples.push(-lag);
        }
    }

    // Sort and deduplicate (handles cases where rounding creates duplicates)
    samples.sort();
    samples.dedup();
    let lag_samples = samples;

    // Calculate correlations for all lags in parallel
    let lag_correlations: Vec<(i64, f64, usize)> = lag_samples
        .into_par_iter()
        .filter_map(|lag| {
            calculate_correlation_at_lag(&data1, &data2, lag)
                .map(|(corr, count)| (lag, corr, count))
        })
        .collect();

    // Find the best correlation
    for (lag, corr, count) in lag_correlations {
        if corr.abs() > best_correlation.abs() {
            best_correlation = corr;
            best_lag = lag * step as i64; // Convert to seconds
            best_sample_count = count;
        }
    }

    // Need at least 5 overlapping points for valid correlation
    if best_sample_count < 5 {
        return None;
    }

    Some(CrossCorrelationResult {
        max_correlation: best_correlation,
        optimal_lag: best_lag,
        sample_count: best_sample_count,
    })
}

#[derive(Debug, Clone)]
struct CrossCorrelationResult {
    max_correlation: f64,
    optimal_lag: i64,
    sample_count: usize,
}

/// Prepare time series data by normalizing and detrending
fn prepare_series_data(series: &MatrixSample) -> Vec<f64> {
    let values: Vec<f64> = series.values.iter().map(|(_, v)| *v).collect();

    if values.is_empty() {
        return values;
    }

    // Remove linear trend (detrending)
    let detrended = detrend(&values);

    // Normalize to zero mean and unit variance
    normalize(&detrended)
}

/// Remove linear trend from data
fn detrend(data: &[f64]) -> Vec<f64> {
    let n = data.len() as f64;
    if n < 2.0 {
        return data.to_vec();
    }

    // Calculate linear regression coefficients
    let mut sum_x = 0.0;
    let mut sum_y = 0.0;
    let mut sum_xx = 0.0;
    let mut sum_xy = 0.0;

    for (i, &y) in data.iter().enumerate() {
        let x = i as f64;
        sum_x += x;
        sum_y += y;
        sum_xx += x * x;
        sum_xy += x * y;
    }

    let slope = (n * sum_xy - sum_x * sum_y) / (n * sum_xx - sum_x * sum_x);
    let intercept = (sum_y - slope * sum_x) / n;

    // Remove the trend
    data.iter()
        .enumerate()
        .map(|(i, &y)| y - (slope * i as f64 + intercept))
        .collect()
}

/// Normalize data to zero mean and unit variance
fn normalize(data: &[f64]) -> Vec<f64> {
    if data.is_empty() {
        return vec![];
    }

    let mean: f64 = data.iter().sum::<f64>() / data.len() as f64;
    let variance: f64 = data.iter().map(|&x| (x - mean).powi(2)).sum::<f64>() / data.len() as f64;

    let std_dev = variance.sqrt();

    if std_dev < 1e-10 {
        // Constant series
        return vec![0.0; data.len()];
    }

    data.iter().map(|&x| (x - mean) / std_dev).collect()
}

/// Calculate correlation at a specific lag
fn calculate_correlation_at_lag(data1: &[f64], data2: &[f64], lag: i64) -> Option<(f64, usize)> {
    let n1 = data1.len();
    let n2 = data2.len();

    // Determine overlap based on lag
    let (start1, end1, start2, end2) = if lag >= 0 {
        // data2 lags behind data1
        let lag_idx = lag as usize;
        if lag_idx >= n1 {
            return None;
        }
        (lag_idx, n1, 0, (n2).min(n1 - lag_idx))
    } else {
        // data1 lags behind data2
        let lag_idx = (-lag) as usize;
        if lag_idx >= n2 {
            return None;
        }
        (0, (n1).min(n2 - lag_idx), lag_idx, n2)
    };

    let slice1 = &data1[start1..end1];
    let slice2 = &data2[start2..end2];

    let overlap_len = slice1.len().min(slice2.len());
    if overlap_len < 3 {
        return None;
    }

    // Calculate Pearson correlation on the overlapping segments
    let mut sum_x = 0.0;
    let mut sum_y = 0.0;
    let mut sum_xx = 0.0;
    let mut sum_yy = 0.0;
    let mut sum_xy = 0.0;

    for i in 0..overlap_len {
        let x = slice1[i];
        let y = slice2[i];
        sum_x += x;
        sum_y += y;
        sum_xx += x * x;
        sum_yy += y * y;
        sum_xy += x * y;
    }

    let n = overlap_len as f64;
    let numerator = n * sum_xy - sum_x * sum_y;
    let denominator = ((n * sum_xx - sum_x * sum_x) * (n * sum_yy - sum_y * sum_y)).sqrt();

    if denominator < 1e-10 {
        // One or both series are constant in this window
        Some((0.0, overlap_len))
    } else {
        Some((numerator / denominator, overlap_len))
    }
}

/// Calculate correlations at multiple lags for display
fn calculate_lag_correlations(
    series1: &MatrixSample,
    series2: &MatrixSample,
    step: f64,
) -> Vec<LagCorrelation> {
    let data1 = prepare_series_data(series1);
    let data2 = prepare_series_data(series2);

    if data1.len() < 10 || data2.len() < 10 {
        return vec![];
    }

    // Use same duration-based lag calculation as main correlation
    let series_len = data1.len().min(data2.len());
    let duration_seconds = series_len as f64 * step;

    let max_configured_lag = *TIME_LAGS.last().unwrap_or(&0) as f64;

    let max_lag_seconds = max_configured_lag.min(duration_seconds / 3.0);
    let max_lag = ((max_lag_seconds / step) as usize).max(1);

    // Show all the TIME_LAGS we actually tested - no need to filter
    let mut lag_samples = vec![0]; // Always include zero lag

    for &seconds in TIME_LAGS {
        let lag = (seconds as f64 / step) as i64;
        if lag > 0 && lag <= max_lag as i64 {
            lag_samples.push(lag);
            lag_samples.push(-lag);
        }
    }

    lag_samples.sort();
    lag_samples.dedup();

    // Calculate all correlations in parallel
    let correlations = lag_samples
        .into_par_iter()
        .filter_map(|lag| {
            calculate_correlation_at_lag(&data1, &data2, lag).map(|(corr, _)| LagCorrelation {
                lag: lag * step as i64,
                correlation: corr,
            })
        })
        .collect();

    correlations
}

/// Extract matrix samples from a query result
fn extract_matrix_samples(
    result: &QueryResult,
) -> Result<Vec<MatrixSample>, Box<dyn std::error::Error>> {
    match result {
        QueryResult::Matrix { result } => Ok(result.clone()),
        QueryResult::Vector { result } => {
            // Convert vector to single-sample matrix
            Ok(result
                .iter()
                .map(|s| MatrixSample {
                    metric: s.metric.clone(),
                    values: vec![s.value],
                })
                .collect())
        }
        QueryResult::Scalar { result } => {
            // Convert scalar to single-sample matrix
            Ok(vec![MatrixSample {
                metric: HashMap::new(),
                values: vec![*result],
            }])
        }
    }
}

/// Format correlation result for display
pub fn format_correlation_result(result: &CorrelationResult) -> String {
    let mut output = String::new();

    // Use human-readable names if available, otherwise fall back to queries
    let display1 = result.metric1_name.as_ref().unwrap_or(&result.metric1);
    let display2 = result.metric2_name.as_ref().unwrap_or(&result.metric2);

    output.push_str(&format!(
        "Cross-Correlation Analysis\n\
         ==========================\n\
         Metric 1: {}\n",
        display1
    ));

    // If we have a name, also show the query
    if result.metric1_name.is_some() {
        output.push_str(&format!("  Query: {}\n", result.metric1));
    }

    output.push_str(&format!("Metric 2: {}\n", display2));

    if result.metric2_name.is_some() {
        output.push_str(&format!("  Query: {}\n", result.metric2));
    }

    output.push_str(&format!(
        "\nMax correlation: {:.4}\n\
         Optimal lag: {} seconds\n",
        result.max_correlation, result.optimal_lag,
    ));

    // Interpret the lag
    if result.optimal_lag > 0 {
        output.push_str(&format!(
            "  → {} leads {} by {} seconds\n",
            display1,
            display2,
            result.optimal_lag.abs()
        ));
    } else if result.optimal_lag < 0 {
        output.push_str(&format!(
            "  → {} leads {} by {} seconds\n",
            display2,
            display1,
            result.optimal_lag.abs()
        ));
    } else {
        output.push_str("  → No lag detected (synchronous correlation)\n");
    }

    output.push_str(&format!(
        "Sample pairs: {}\n\
         Interpretation: {}\n",
        result.sample_count,
        interpret_correlation(result.max_correlation)
    ));

    // Show correlation at different lags if available
    if !result.correlations_at_lag.is_empty() {
        output.push_str("\nCorrelation at different lags:\n");
        for lag_corr in &result.correlations_at_lag {
            output.push_str(&format!(
                "  Lag {:+3}s: {:.4}\n",
                lag_corr.lag, lag_corr.correlation
            ));
        }
    }

    // Always show series details to identify which specific series correlated
    if !result.series_pairs.is_empty() {
        // Check if we have complex expressions
        let expr1_is_complex = result.metric1.contains('/')
            || result.metric1.contains('*')
            || result.metric1.contains('+')
            || result.metric1.contains('-');
        let expr2_is_complex = result.metric2.contains('/')
            || result.metric2.contains('*')
            || result.metric2.contains('+')
            || result.metric2.contains('-');

        if expr1_is_complex || expr2_is_complex {
            output.push_str("\nNote: Complex expressions used in correlation\n");
            if expr1_is_complex {
                output.push_str(&format!("  Expression 1: {}\n", result.metric1));
            }
            if expr2_is_complex {
                output.push_str(&format!("  Expression 2: {}\n", result.metric2));
            }
        }

        output.push_str(&format!(
            "\nSeries-level correlations ({} pair{}):\n",
            result.series_pairs.len(),
            if result.series_pairs.len() == 1 {
                ""
            } else {
                "s"
            }
        ));

        // Show top correlations (or the single correlation if only one)
        let show_count = 10.min(result.series_pairs.len()); // Show up to 10 for better visibility

        for (i, pair) in result.series_pairs.iter().take(show_count).enumerate() {
            output.push_str(&format!(
                "\n{}. r={:.4} at lag={}s (n={})",
                i + 1,
                pair.max_correlation,
                pair.optimal_lag,
                pair.sample_count
            ));

            // Format labels compactly with deterministic ordering
            let format_labels = |labels: &HashMap<String, String>| -> String {
                // Get the metric name first
                let metric_name = labels
                    .get("metric")
                    .or_else(|| labels.get("__name__"))
                    .map(|s| s.as_str());

                // Labels to omit from the label selector
                let omit_labels = ["metric", "metric_type", "unit", "__name__"];

                let mut label_parts = Vec::new();

                // First add 'id' if present
                if let Some(id_value) = labels.get("id") {
                    label_parts.push(format!("id=\"{}\"", id_value));
                }

                // Collect and sort remaining labels alphabetically
                let mut remaining_labels: Vec<(String, String)> = labels
                    .iter()
                    .filter(|(k, _)| k.as_str() != "id" && !omit_labels.contains(&k.as_str()))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                remaining_labels.sort_by(|a, b| a.0.cmp(&b.0));

                // Add sorted labels with proper quoting for PromQL
                for (k, v) in remaining_labels {
                    label_parts.push(format!("{}=\"{}\"", k, v));
                }

                // Format based on whether we have a metric name
                match metric_name {
                    Some(name)
                        if !name.is_empty()
                            && name != "division_result"
                            && name != "multiplication_result" =>
                    {
                        // Normal metric with a proper name
                        if label_parts.is_empty() {
                            name.to_string()
                        } else {
                            format!("{}{{{}}}", name, label_parts.join(","))
                        }
                    }
                    _ => {
                        // Complex expression or unknown metric - just show labels
                        if label_parts.is_empty() {
                            "{}".to_string()
                        } else {
                            format!("{{{}}}", label_parts.join(","))
                        }
                    }
                }
            };

            output.push_str(&format!(
                "\n   {} vs {}",
                format_labels(&pair.labels1),
                format_labels(&pair.labels2)
            ));
        }

        if result.series_pairs.len() > show_count {
            output.push_str(&format!(
                "\n... and {} more pairs",
                result.series_pairs.len() - show_count
            ));
        }
    }

    // Add interpretation note about the methodology
    output.push_str(
        "\nNote: Data has been detrended and normalized to detect non-linear relationships.\n",
    );
    output.push_str(
        "Positive lag means metric 2 follows metric 1 (metric 1 is leading indicator).\n",
    );

    output
}

fn interpret_correlation(r: f64) -> &'static str {
    let abs_r = r.abs();
    if abs_r >= 0.9 {
        if r > 0.0 {
            "Very strong positive correlation"
        } else {
            "Very strong negative correlation"
        }
    } else if abs_r >= 0.7 {
        if r > 0.0 {
            "Strong positive correlation"
        } else {
            "Strong negative correlation"
        }
    } else if abs_r >= 0.5 {
        if r > 0.0 {
            "Moderate positive correlation"
        } else {
            "Moderate negative correlation"
        }
    } else if abs_r >= 0.3 {
        if r > 0.0 {
            "Weak positive correlation"
        } else {
            "Weak negative correlation"
        }
    } else {
        "Very weak or no correlation"
    }
}
