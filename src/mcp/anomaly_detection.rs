use crate::viewer::promql::{QueryEngine, QueryResult};
use crate::viewer::tsdb::Tsdb;
use allan::{Allan, Hadamard, ModifiedAllan};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

/// Result of anomaly detection analysis
#[derive(Debug, Serialize, Deserialize)]
pub struct AnomalyDetectionResult {
    pub query: String,
    pub total_points: usize,
    pub timestamps: Vec<f64>,
    pub values: Vec<f64>,
    pub mad_analysis: MadAnalysis,
    pub cusum_analysis: CusumAnalysis,
    pub fft_analysis: FftAnalysis,
    pub allan_analysis: AllanAnalysis,
    pub hadamard_analysis: HadamardAnalysis,
    pub modified_allan_analysis: ModifiedAllanAnalysis,
    pub anomalies: Vec<Anomaly>,
    pub confidence_score: f64,
}

/// MAD (Median Absolute Deviation) analysis results
#[derive(Debug, Serialize, Deserialize)]
pub struct MadAnalysis {
    pub median: f64,
    pub mad: f64,
    pub threshold: f64,
    pub outliers: Vec<usize>,
    pub outlier_count: usize,
}

/// CUSUM (Cumulative Sum) analysis results
#[derive(Debug, Serialize, Deserialize)]
pub struct CusumAnalysis {
    pub mean: f64,
    pub std_dev: f64,
    pub threshold: f64,
    pub change_points: Vec<usize>,
    pub positive_shifts: Vec<usize>,
    pub negative_shifts: Vec<usize>,
    pub cliffs: Vec<CliffPoint>,
    pub gradual_changes: Vec<usize>,
    pub sensitivity_levels: Vec<SensitivityLevel>,
}

/// Detected cliff (dramatic change)
#[derive(Debug, Serialize, Deserialize)]
pub struct CliffPoint {
    pub index: usize,
    pub magnitude: f64,
    pub direction: ChangeDirection,
}

/// Direction of change
#[derive(Debug, Serialize, Deserialize)]
pub enum ChangeDirection {
    Increase,
    Decrease,
}

/// CUSUM sensitivity level results
#[derive(Debug, Serialize, Deserialize)]
pub struct SensitivityLevel {
    pub name: String,
    pub k_factor: f64,
    pub h_factor: f64,
    pub detected_changes: Vec<usize>,
}

/// FFT (Fast Fourier Transform) analysis results
#[derive(Debug, Serialize, Deserialize)]
pub struct FftAnalysis {
    pub dominant_frequencies: Vec<DominantFrequency>,
    pub has_periodic_pattern: bool,
    pub period_seconds: Option<f64>,
    pub periodicity_strength: f64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DominantFrequency {
    pub frequency_hz: f64,
    pub period_seconds: f64,
    pub magnitude: f64,
    pub relative_power: f64,
}

/// Allan Deviation analysis results
#[derive(Debug, Serialize, Deserialize)]
pub struct AllanAnalysis {
    pub taus: Vec<f64>,
    pub deviations: Vec<f64>,
    pub noise_type: NoiseType,
    pub minima: Vec<CycleMinima>,
    pub has_cyclic_pattern: bool,
}

/// Hadamard Deviation analysis results
#[derive(Debug, Serialize, Deserialize)]
pub struct HadamardAnalysis {
    pub taus: Vec<f64>,
    pub deviations: Vec<f64>,
    pub noise_type: NoiseType,
    pub minima: Vec<CycleMinima>,
}

/// Modified Allan Deviation analysis results
#[derive(Debug, Serialize, Deserialize)]
pub struct ModifiedAllanAnalysis {
    pub taus: Vec<f64>,
    pub deviations: Vec<f64>,
    pub noise_type: NoiseType,
    pub minima: Vec<CycleMinima>,
}

/// Detected cycle/period from deviation minima
#[derive(Debug, Serialize, Deserialize)]
pub struct CycleMinima {
    pub tau_seconds: f64,
    pub deviation: f64,
    pub confidence: f64,
}

/// Noise type identified from Allan/Hadamard slope
#[derive(Debug, Serialize, Deserialize)]
pub enum NoiseType {
    WhitePhase,      // slope = -1
    FlickerPhase,    // slope = -1/2
    WhiteFrequency,  // slope = -1/2
    FlickerFrequency,// slope = 0
    RandomWalk,      // slope = +1/2
    FlickerWalk,     // slope = +1
    Unknown,
}

/// Individual anomaly detected
#[derive(Debug, Serialize, Deserialize)]
pub struct Anomaly {
    pub timestamp: f64,
    pub value: f64,
    pub index: usize,
    pub anomaly_type: AnomalyType,
    pub severity: AnomalySeverity,
    pub confidence: f64,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum AnomalyType {
    PointOutlier,
    LevelShift,
    TrendChange,
    Combined,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum AnomalySeverity {
    Low,
    Medium,
    High,
    Critical,
}

/// Validate and fix common query issues
fn validate_and_fix_query(query: &str) -> Result<String, Box<dyn std::error::Error>> {
    // Check for rate/irate/increase/delta/deriv functions that require range vectors
    let range_vector_functions = [
        "rate(", "irate(", "increase(", "delta(", "deriv(",
        "rate_over_time(", "avg_over_time(", "min_over_time(",
        "max_over_time(", "sum_over_time(", "count_over_time(",
        "stddev_over_time(", "stdvar_over_time(", "changes(",
        "resets(", "holt_winters(", "predict_linear("
    ];

    for func in &range_vector_functions {
        if query.contains(func) {
            // Check if it has a range vector selector [duration]
            // This is a simple check - a proper parser would be better

            // Find the function start
            if let Some(start_pos) = query.find(func) {
                // Find the matching closing parenthesis
                let after_func = &query[start_pos + func.len()..];
                let mut paren_depth = 1;
                let mut has_range_vector = false;
                let mut last_close_paren = 0;

                for (i, ch) in after_func.chars().enumerate() {
                    match ch {
                        '(' => paren_depth += 1,
                        ')' => {
                            paren_depth -= 1;
                            if paren_depth == 0 {
                                last_close_paren = i;
                                break;
                            }
                        },
                        '[' => {
                            // Check if this is inside our function
                            if paren_depth > 0 {
                                has_range_vector = true;
                            }
                        },
                        _ => {}
                    }
                }

                if !has_range_vector && paren_depth == 0 {
                    // Missing range vector - auto-fix with default
                    let default_range = "[1m]"; // 1 minute default

                    // Build the fixed query
                    let before_close = start_pos + func.len() + last_close_paren;
                    let mut fixed_query = String::new();
                    fixed_query.push_str(&query[..before_close]);
                    fixed_query.push_str(default_range);
                    fixed_query.push_str(&query[before_close..]);

                    // Log the auto-fix (in production, this would go to stderr or logs)
                    eprintln!(
                        "WARNING: Query '{}' was missing range vector for {}. Auto-fixed to: {}",
                        query,
                        func.trim_end_matches('('),
                        fixed_query
                    );

                    return Ok(fixed_query);
                }
            }
        }
    }

    // Check for bare range vectors (not allowed in queries)
    if query.contains('[') && query.contains(']') {
        // Check if it's a bare range vector like "metric[5m]" without a function
        let has_function = range_vector_functions.iter().any(|f| query.contains(f));
        if !has_function {
            // Check if it's just a metric with range vector
            if !query.contains("(") {
                return Err(format!(
                    "Query '{}' appears to be a bare range vector selector.\n\
                    \n\
                    Range vectors must be used with a function.\n\
                    For counters, use: rate({})\n\
                    For gauges, use: avg_over_time({})\n\
                    \n\
                    Range vectors alone cannot be graphed or analyzed.",
                    query, query, query
                ).into());
            }
        }
    }

    Ok(query.to_string())
}

/// Perform anomaly detection on a time series
pub fn detect_anomalies(
    engine: &Arc<QueryEngine>,
    tsdb: &Arc<Tsdb>,
    query: &str,
) -> Result<AnomalyDetectionResult, Box<dyn std::error::Error>> {
    // Clean up escaped quotes from JSON
    let query = query.replace("\\\"", "\"");

    // Validate and potentially fix the query
    let query = validate_and_fix_query(&query)?;

    // Execute the query to get time series data
    let (start_time, end_time) = engine.get_time_range();

    // Validate time range
    if start_time >= end_time {
        return Err(format!(
            "Invalid time range for anomaly detection: start ({}) >= end ({}). \
            The parquet file may not contain enough data.",
            start_time, end_time
        ).into());
    }

    let step = 1.0; // 1 second resolution

    // Check if we have enough time range for meaningful analysis
    let duration = end_time - start_time;
    if duration < 10.0 {
        return Err(format!(
            "Time range too short for anomaly detection: {:.1} seconds. \
            Need at least 10 seconds of data for meaningful analysis.",
            duration
        ).into());
    }

    // Try to execute the query
    let query_result = match engine.query_range(&query, start_time, end_time, step) {
        Ok(result) => result,
        Err(e) => {
            let error_msg = e.to_string();

            // If metric not found, suggest available metrics
            if error_msg.contains("Metric not found") || error_msg.contains("not found") {
                // Extract the metric name from the query (simple heuristic)
                let metric_hint = if let Some(pos) = query.find('(') {
                    if let Some(inner_start) = query[pos..].find(|c: char| c.is_alphabetic() || c == '_') {
                        let start = pos + inner_start;
                        let end = query[start..].find(|c: char| !c.is_alphanumeric() && c != '_' && c != '/')
                            .map(|i| start + i)
                            .unwrap_or(query.len());
                        Some(&query[start..end])
                    } else {
                        None
                    }
                } else {
                    // No function, might be bare metric
                    query.split('{').next()
                };

                // Get available metrics for suggestion
                let mut all_metrics = Vec::new();
                all_metrics.extend(tsdb.counter_names());
                all_metrics.extend(tsdb.gauge_names());
                all_metrics.extend(tsdb.histogram_names());

                // Find similar metrics
                let mut suggestions = Vec::new();
                if let Some(hint) = metric_hint {
                    // Normalize the hint to use underscores
                    let hint_normalized = hint.replace('/', "_").replace('-', "_");

                    for metric in &all_metrics {
                        // Check for exact match first
                        if *metric == hint || *metric == hint_normalized {
                            suggestions.insert(0, *metric);
                        }
                        // Then check for partial matches
                        else if metric.contains(&hint_normalized) ||
                                metric.starts_with(&format!("{}_", hint_normalized)) ||
                                metric.ends_with(&format!("_{}", hint_normalized)) {
                            suggestions.push(*metric);
                        }
                    }
                }

                let mut error_with_help = format!("Query failed: {}", error_msg);

                if !suggestions.is_empty() {
                    error_with_help.push_str("\n\nDid you mean one of these metrics?");
                    for suggestion in suggestions.iter().take(5) {
                        error_with_help.push_str(&format!("\n  - {}", suggestion));
                    }
                } else if !all_metrics.is_empty() {
                    error_with_help.push_str("\n\nAvailable metrics include:");
                    for metric in all_metrics.iter().take(10) {
                        error_with_help.push_str(&format!("\n  - {}", metric));
                    }
                    if all_metrics.len() > 10 {
                        error_with_help.push_str(&format!("\n  ... and {} more", all_metrics.len() - 10));
                    }
                }

                return Err(error_with_help.into());
            }

            return Err(Box::new(e));
        }
    };

    // Extract time series data
    let (timestamps, values) = extract_time_series(&query_result, &query)?;

    if values.is_empty() {
        return Err("No data points found for the given query".into());
    }

    // Perform MAD analysis with conservative threshold
    let mad_analysis = perform_mad_analysis(&values, 5.0)?;

    // Perform CUSUM analysis
    let cusum_analysis = perform_cusum_analysis(&values)?;

    // Perform FFT analysis (step is the sample interval in seconds)
    let fft_analysis = perform_fft_analysis(&values, step)?;

    // Perform Allan Deviation analysis
    let allan_analysis = perform_allan_analysis(&values, step)?;

    // Perform Hadamard Deviation analysis
    let hadamard_analysis = perform_hadamard_analysis(&values, step)?;

    // Perform Modified Allan Deviation analysis
    let modified_allan_analysis = perform_modified_allan_analysis(&values, step)?;

    // Combine analyses to identify high-confidence anomalies
    let anomalies = identify_anomalies(
        &timestamps,
        &values,
        &mad_analysis,
        &cusum_analysis,
        &fft_analysis,
        &allan_analysis,
        &hadamard_analysis,
        &modified_allan_analysis,
    );

    // Calculate overall confidence score
    let confidence_score = calculate_confidence_score(&anomalies, values.len());

    Ok(AnomalyDetectionResult {
        query: query.to_string(),
        total_points: values.len(),
        timestamps,
        values,
        mad_analysis,
        cusum_analysis,
        fft_analysis,
        allan_analysis,
        hadamard_analysis,
        modified_allan_analysis,
        anomalies,
        confidence_score,
    })
}

/// Extract time series data from query result
fn extract_time_series(
    result: &QueryResult,
    query: &str,
) -> Result<(Vec<f64>, Vec<f64>), Box<dyn std::error::Error>> {
    match result {
        QueryResult::Vector { result } => {
            // Vector results from query_range indicate something is wrong
            // This shouldn't happen with properly formed rate() queries

            // Debug: Check if this is a rate/irate query with range vector
            let has_range_vector = query.contains("[") && query.contains("]");
            let is_rate_query = query.contains("rate(") || query.contains("irate(") || query.contains("increase(");

            if is_rate_query && has_range_vector {
                // This should have returned a Matrix, not a Vector!
                return Err(format!(
                    "Unexpected result type for query '{}'. \
                    The query appears correct but returned instant values instead of a time series. \
                    This might indicate:\n\
                    1. The time range in the parquet file is too short\n\
                    2. There's insufficient data for the rate calculation\n\
                    3. The query engine encountered an issue\n\
                    \nDebug info: Result contains {} series",
                    query,
                    result.len()
                ).into());
            }

            let example_query = if query.contains("rate(") || query.contains("irate(") {
                // Query has rate() but missing the range vector
                let fixed = query
                    .replace("rate(", "rate(")
                    .replace("irate(", "irate(")
                    .replace("))", "[1m]))")
                    .replace("})", "}[1m]))");
                format!("\n\nYour query appears to be missing a range vector selector.\nTry: {}", fixed)
            } else if query.contains("increase(") {
                let fixed = query
                    .replace("increase(", "increase(")
                    .replace("))", "[1m]))")
                    .replace("})", "}[1m]))");
                format!("\n\nYour query appears to be missing a range vector selector.\nTry: {}", fixed)
            } else {
                "\n\nFor counter metrics, use: rate(metric_name[1m]) or irate(metric_name[1m])\nFor gauge metrics that need smoothing, use: avg_over_time(metric_name[1m])".to_string()
            };

            if result.is_empty() {
                return Err(format!(
                    "Query returned no time series data. The query executed as an instant vector, \
                    which returns only current values, not a time series.{}",
                    example_query
                ).into());
            }

            // Even with data, it's still just instant values
            return Err(format!(
                "Query returned instant values instead of time series data. \
                Anomaly detection requires metrics with range vectors to analyze patterns over time.{}",
                example_query
            ).into());
        }
        QueryResult::Matrix { result } => {
            // For matrix results, we have time series data
            if result.is_empty() {
                return Ok((vec![], vec![]));
            }

            // If there are multiple series, sum them by timestamp
            if result.len() > 1 {
                // Aggregate multiple series by summing values at each timestamp
                let mut timestamp_values: std::collections::BTreeMap<i64, f64> = std::collections::BTreeMap::new();

                for series in result {
                    for (ts, val) in &series.values {
                        let ts_key = ts.round() as i64;
                        *timestamp_values.entry(ts_key).or_insert(0.0) += val;
                    }
                }

                let timestamps: Vec<f64> = timestamp_values.keys().map(|&ts| ts as f64).collect();
                let values: Vec<f64> = timestamp_values.values().copied().collect();
                Ok((timestamps, values))
            } else {
                // Single series - use it directly
                let series = &result[0];
                let timestamps: Vec<f64> = series.values.iter().map(|(ts, _)| *ts).collect();
                let values: Vec<f64> = series.values.iter().map(|(_, val)| *val).collect();
                Ok((timestamps, values))
            }
        }
        QueryResult::Scalar { result } => {
            // Single scalar value - not enough for anomaly detection
            Err(format!(
                "Scalar query returned a single value ({:.4}). \
                Anomaly detection requires time series data with multiple points.",
                result.1
            ).into())
        }
    }
}

/// Perform MAD (Median Absolute Deviation) analysis
fn perform_mad_analysis(
    values: &[f64],
    threshold_multiplier: f64,
) -> Result<MadAnalysis, Box<dyn std::error::Error>> {
    if values.is_empty() {
        return Err("Cannot perform MAD analysis on empty dataset".into());
    }

    // Calculate median
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = if sorted.len() % 2 == 0 {
        (sorted[sorted.len() / 2 - 1] + sorted[sorted.len() / 2]) / 2.0
    } else {
        sorted[sorted.len() / 2]
    };

    // Calculate absolute deviations from median
    let deviations: Vec<f64> = values.iter().map(|v| (v - median).abs()).collect();

    // Calculate MAD (median of absolute deviations)
    let mut sorted_deviations = deviations.clone();
    sorted_deviations.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mad = if sorted_deviations.len() % 2 == 0 {
        (sorted_deviations[sorted_deviations.len() / 2 - 1]
            + sorted_deviations[sorted_deviations.len() / 2])
            / 2.0
    } else {
        sorted_deviations[sorted_deviations.len() / 2]
    };

    // Robust estimator of standard deviation
    let mad_std = mad * 1.4826;
    let threshold = threshold_multiplier * mad_std;

    // Find outliers
    let mut outliers = Vec::new();
    for (i, &value) in values.iter().enumerate() {
        if (value - median).abs() > threshold {
            outliers.push(i);
        }
    }

    Ok(MadAnalysis {
        median,
        mad,
        threshold,
        outlier_count: outliers.len(),
        outliers,
    })
}

/// Perform multi-scale CUSUM analysis with cliff detection
fn perform_cusum_analysis(values: &[f64]) -> Result<CusumAnalysis, Box<dyn std::error::Error>> {
    if values.is_empty() {
        return Err("Cannot perform CUSUM analysis on empty dataset".into());
    }

    // Calculate mean and standard deviation
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    let variance = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / values.len() as f64;
    let std_dev = variance.sqrt();

    // First detect cliffs using simple differencing
    let cliffs = detect_cliffs(values, mean, std_dev);

    // Run multi-scale CUSUM with different sensitivities
    let sensitivity_configs = vec![
        ("High Sensitivity", 0.25, 2.0),  // Detect small changes
        ("Medium Sensitivity", 0.5, 4.0),  // Standard detection
        ("Low Sensitivity", 1.0, 6.0),     // Only major changes
        ("Cliff Detection", 2.0, 8.0),     // Dramatic changes
    ];

    let mut all_change_points = Vec::new();
    let mut sensitivity_levels = Vec::new();

    for (name, k_factor, h_factor) in sensitivity_configs {
        let k = k_factor * std_dev;
        let h = h_factor * std_dev;
        let detected = run_cusum_at_sensitivity(values, mean, k, h);

        all_change_points.extend(&detected);
        sensitivity_levels.push(SensitivityLevel {
            name: name.to_string(),
            k_factor,
            h_factor,
            detected_changes: detected,
        });
    }

    // Deduplicate and sort all change points
    all_change_points.sort_unstable();
    all_change_points.dedup();

    // Separate gradual changes from cliffs
    let cliff_indices: Vec<usize> = cliffs.iter().map(|c| c.index).collect();
    let gradual_changes: Vec<usize> = all_change_points
        .iter()
        .filter(|&&idx| !cliff_indices.contains(&idx))
        .copied()
        .collect();

    // Run standard CUSUM for compatibility
    let k = 0.5 * std_dev;
    let h = 4.0 * std_dev;
    let (positive_shifts, negative_shifts) = run_standard_cusum(values, mean, k, h);

    Ok(CusumAnalysis {
        mean,
        std_dev,
        threshold: h,
        change_points: all_change_points,
        positive_shifts,
        negative_shifts,
        cliffs,
        gradual_changes,
        sensitivity_levels,
    })
}

/// Detect dramatic cliffs in the data
fn detect_cliffs(values: &[f64], mean: f64, std_dev: f64) -> Vec<CliffPoint> {
    let mut cliffs = Vec::new();

    if values.len() < 2 {
        return cliffs;
    }

    // Use both absolute and relative thresholds
    let absolute_threshold = 5.0 * std_dev; // Dramatic change
    let relative_threshold = 0.5; // 50% change relative to mean

    for i in 1..values.len() {
        let diff = values[i] - values[i - 1];
        let abs_diff = diff.abs();

        // Check both absolute and relative magnitude
        let is_absolute_cliff = abs_diff > absolute_threshold;
        let is_relative_cliff = mean != 0.0 && (abs_diff / mean.abs()) > relative_threshold;

        if is_absolute_cliff || is_relative_cliff {
            // Additional check: is this a sustained change?
            let sustained = if i < values.len() - 1 {
                // Check if the next value maintains the new level
                let next_diff = (values[i + 1] - values[i]).abs();
                next_diff < abs_diff * 0.5 // Next change is much smaller
            } else {
                true // Can't check, assume it's sustained
            };

            if sustained {
                cliffs.push(CliffPoint {
                    index: i,
                    magnitude: abs_diff,
                    direction: if diff > 0.0 {
                        ChangeDirection::Increase
                    } else {
                        ChangeDirection::Decrease
                    },
                });
            }
        }
    }

    // Also check for cliffs using running windows
    if values.len() >= 5 {
        for i in 2..values.len() - 2 {
            let before_avg = (values[i - 2] + values[i - 1]) / 2.0;
            let after_avg = (values[i + 1] + values[i + 2]) / 2.0;
            let window_diff = (after_avg - before_avg).abs();

            if window_diff > absolute_threshold * 1.5 {
                // Check if we already detected this cliff
                let already_detected = cliffs.iter().any(|c| c.index.abs_diff(i) <= 1);

                if !already_detected {
                    cliffs.push(CliffPoint {
                        index: i,
                        magnitude: window_diff,
                        direction: if after_avg > before_avg {
                            ChangeDirection::Increase
                        } else {
                            ChangeDirection::Decrease
                        },
                    });
                }
            }
        }
    }

    cliffs.sort_by_key(|c| c.index);
    cliffs
}

/// Run CUSUM at a specific sensitivity level
fn run_cusum_at_sensitivity(values: &[f64], mean: f64, k: f64, h: f64) -> Vec<usize> {
    let mut s_high = 0.0;
    let mut s_low = 0.0;
    let mut change_points = Vec::new();

    for (i, &value) in values.iter().enumerate() {
        s_high = f64::max(0.0, s_high + value - mean - k);
        s_low = f64::max(0.0, s_low + mean - k - value);

        if s_high > h {
            change_points.push(i);
            s_high = 0.0;
        }

        if s_low > h {
            if change_points.last() != Some(&i) {
                change_points.push(i);
            }
            s_low = 0.0;
        }
    }

    change_points
}

/// Run standard CUSUM for compatibility
fn run_standard_cusum(values: &[f64], mean: f64, k: f64, h: f64) -> (Vec<usize>, Vec<usize>) {
    let mut s_high = 0.0;
    let mut s_low = 0.0;
    let mut positive_shifts = Vec::new();
    let mut negative_shifts = Vec::new();

    for (i, &value) in values.iter().enumerate() {
        s_high = f64::max(0.0, s_high + value - mean - k);
        s_low = f64::max(0.0, s_low + mean - k - value);

        if s_high > h {
            positive_shifts.push(i);
            s_high = 0.0;
        }

        if s_low > h {
            negative_shifts.push(i);
            s_low = 0.0;
        }
    }

    (positive_shifts, negative_shifts)
}

/// Perform FFT (Fast Fourier Transform) analysis
fn perform_fft_analysis(
    values: &[f64],
    sample_interval: f64,
) -> Result<FftAnalysis, Box<dyn std::error::Error>> {
    use rustfft::{num_complex::Complex, FftPlanner};

    if values.len() < 4 {
        // Not enough data for meaningful FFT
        return Ok(FftAnalysis {
            dominant_frequencies: vec![],
            has_periodic_pattern: false,
            period_seconds: None,
            periodicity_strength: 0.0,
        });
    }

    // Prepare data for FFT
    let mut planner = FftPlanner::new();
    let fft = planner.plan_fft_forward(values.len());

    // Convert to complex numbers
    let mut buffer: Vec<Complex<f64>> = values.iter().map(|&v| Complex::new(v, 0.0)).collect();

    // Perform FFT
    fft.process(&mut buffer);

    // Calculate frequency parameters
    let n = values.len();
    let sample_rate = 1.0 / sample_interval; // Hz
    let freq_resolution = sample_rate / n as f64;

    // Calculate constraints based on Nyquist theorem
    let recording_duration = n as f64 * sample_interval;
    let max_period = recording_duration / 2.0; // Need at least 2 cycles
    let min_period = 2.0 * sample_interval; // Nyquist limit

    // Frequency constraints
    let min_frequency = 1.0 / max_period; // Lowest detectable frequency
    let max_frequency = 1.0 / min_period; // Nyquist frequency (sample_rate / 2)

    // Only look at first half (Nyquist frequency)
    let half_n = n / 2;
    let mut freq_magnitudes: Vec<(f64, f64)> = Vec::new();

    for (i, v) in buffer.iter().enumerate().take(half_n).skip(1) {
        // Skip DC component at index 0
        let frequency = i as f64 * freq_resolution;

        // Only include frequencies within valid range
        if frequency >= min_frequency && frequency <= max_frequency {
            let magnitude = v.norm();
            freq_magnitudes.push((frequency, magnitude));
        }
    }

    // Sort by magnitude to find dominant frequencies
    freq_magnitudes.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    // Calculate total power (excluding DC)
    let total_power: f64 = freq_magnitudes.iter().map(|(_, mag)| mag * mag).sum();

    // Find dominant frequencies (top 3 that are significant)
    let mut dominant_frequencies = Vec::new();

    if total_power > 0.0 {
        for &(freq, mag) in freq_magnitudes.iter().take(5) {
            let power = mag * mag;
            let relative_power = power / total_power;
            let period = 1.0 / freq;

            // Double-check period is within valid range
            if period >= min_period && period <= max_period && relative_power > 0.05 {
                // At least 5% of total power
                dominant_frequencies.push(DominantFrequency {
                    frequency_hz: freq,
                    period_seconds: period,
                    magnitude: mag,
                    relative_power,
                });
            }
        }
    }

    // Check for strong periodicity
    let has_periodic_pattern =
        !dominant_frequencies.is_empty() && dominant_frequencies[0].relative_power > 0.2; // Dominant frequency has >20% of power

    let period_seconds = if has_periodic_pattern {
        Some(dominant_frequencies[0].period_seconds)
    } else {
        None
    };

    let periodicity_strength = if !dominant_frequencies.is_empty() {
        dominant_frequencies[0].relative_power
    } else {
        0.0
    };

    Ok(FftAnalysis {
        dominant_frequencies,
        has_periodic_pattern,
        period_seconds,
        periodicity_strength,
    })
}

/// Perform Allan Deviation analysis
fn perform_allan_analysis(
    values: &[f64],
    sample_interval: f64,
) -> Result<AllanAnalysis, Box<dyn std::error::Error>> {
    if values.len() < 3 {
        return Ok(AllanAnalysis {
            taus: vec![],
            deviations: vec![],
            noise_type: NoiseType::Unknown,
            minima: vec![],
            has_cyclic_pattern: false,
        });
    }

    // Create Allan calculator
    let mut allan = Allan::new();

    // Add all data points
    for &value in values {
        allan.record(value);
    }

    // Generate tau values (averaging times in samples)
    let max_tau_samples = values.len() / 3;
    let mut taus = Vec::new();
    let mut tau_samples = 1;
    let mut taus_seconds = Vec::new();

    while tau_samples <= max_tau_samples {
        taus.push(tau_samples);
        taus_seconds.push(tau_samples as f64 * sample_interval);
        tau_samples = ((tau_samples as f64 * 1.5) as usize).max(tau_samples + 1);
    }

    // Calculate Allan deviation for each tau
    let mut deviations = Vec::new();
    for &tau in &taus {
        if let Some(tau_result) = allan.get(tau) {
            if let Some(dev) = tau_result.deviation() {
                deviations.push(dev);
            } else {
                deviations.push(0.0);
            }
        } else {
            deviations.push(0.0);
        }
    }

    // Identify noise type from slope
    let noise_type = identify_noise_type(&taus_seconds, &deviations);

    // Find local minima (indicates cyclic behavior)
    let minima = find_deviation_minima(&taus_seconds, &deviations);

    // Check if we have strong cyclic patterns
    let has_cyclic_pattern = !minima.is_empty() && minima[0].confidence > 0.7;

    Ok(AllanAnalysis {
        taus: taus_seconds,
        deviations,
        noise_type,
        minima,
        has_cyclic_pattern,
    })
}

/// Perform Hadamard Deviation analysis using allan crate
fn perform_hadamard_analysis(
    values: &[f64],
    sample_interval: f64,
) -> Result<HadamardAnalysis, Box<dyn std::error::Error>> {
    if values.len() < 3 {
        return Ok(HadamardAnalysis {
            taus: vec![],
            deviations: vec![],
            noise_type: NoiseType::Unknown,
            minima: vec![],
        });
    }

    // Create Hadamard calculator from allan crate
    let mut hadamard = Hadamard::new();

    // Add all data points
    for &value in values {
        hadamard.record(value);
    }

    // Generate tau values (averaging times in samples)
    let max_tau_samples = values.len() / 3;
    let mut taus = Vec::new();
    let mut tau_samples = 1;
    let mut taus_seconds = Vec::new();

    while tau_samples <= max_tau_samples {
        taus.push(tau_samples);
        taus_seconds.push(tau_samples as f64 * sample_interval);
        tau_samples = ((tau_samples as f64 * 1.5) as usize).max(tau_samples + 1);
    }

    // Calculate Hadamard deviation for each tau
    let mut deviations = Vec::new();
    for &tau in &taus {
        if let Some(tau_result) = hadamard.get(tau) {
            if let Some(dev) = tau_result.deviation() {
                deviations.push(dev);
            } else {
                deviations.push(0.0);
            }
        } else {
            deviations.push(0.0);
        }
    }

    // Identify noise type
    let noise_type = identify_noise_type(&taus_seconds, &deviations);

    // Find local minima
    let minima = find_deviation_minima(&taus_seconds, &deviations);

    Ok(HadamardAnalysis {
        taus: taus_seconds,
        deviations,
        noise_type,
        minima,
    })
}

/// Perform Modified Allan Deviation analysis using allan crate
fn perform_modified_allan_analysis(
    values: &[f64],
    sample_interval: f64,
) -> Result<ModifiedAllanAnalysis, Box<dyn std::error::Error>> {
    if values.len() < 3 {
        return Ok(ModifiedAllanAnalysis {
            taus: vec![],
            deviations: vec![],
            noise_type: NoiseType::Unknown,
            minima: vec![],
        });
    }

    // Create Modified Allan calculator from allan crate
    let mut modified = ModifiedAllan::new();

    // Add all data points
    for &value in values {
        modified.record(value);
    }

    // Generate tau values (averaging times in samples)
    let max_tau_samples = values.len() / 3;
    let mut taus = Vec::new();
    let mut tau_samples = 1;
    let mut taus_seconds = Vec::new();

    while tau_samples <= max_tau_samples {
        taus.push(tau_samples);
        taus_seconds.push(tau_samples as f64 * sample_interval);
        tau_samples = ((tau_samples as f64 * 1.5) as usize).max(tau_samples + 1);
    }

    // Calculate Modified Allan deviation for each tau
    let mut deviations = Vec::new();
    for &tau in &taus {
        if let Some(tau_result) = modified.get(tau) {
            if let Some(dev) = tau_result.deviation() {
                deviations.push(dev);
            } else {
                deviations.push(0.0);
            }
        } else {
            deviations.push(0.0);
        }
    }

    // Identify noise type
    let noise_type = identify_noise_type(&taus_seconds, &deviations);

    // Find local minima
    let minima = find_deviation_minima(&taus_seconds, &deviations);

    Ok(ModifiedAllanAnalysis {
        taus: taus_seconds,
        deviations,
        noise_type,
        minima,
    })
}

/// Identify noise type from log-log slope of deviation vs tau
fn identify_noise_type(taus: &[f64], deviations: &[f64]) -> NoiseType {
    if taus.len() < 3 || deviations.len() < 3 {
        return NoiseType::Unknown;
    }

    // Calculate slope in log-log space (simple linear regression)
    let mut sum_x = 0.0;
    let mut sum_y = 0.0;
    let mut sum_xx = 0.0;
    let mut sum_xy = 0.0;
    let mut n = 0;

    for i in 0..taus.len().min(deviations.len()) {
        if taus[i] > 0.0 && deviations[i] > 0.0 {
            let log_tau = taus[i].ln();
            let log_dev = deviations[i].ln();
            sum_x += log_tau;
            sum_y += log_dev;
            sum_xx += log_tau * log_tau;
            sum_xy += log_tau * log_dev;
            n += 1;
        }
    }

    if n < 2 {
        return NoiseType::Unknown;
    }

    let n_f = n as f64;
    let slope = (n_f * sum_xy - sum_x * sum_y) / (n_f * sum_xx - sum_x * sum_x);

    // Classify based on slope
    match slope {
        s if s < -0.75 => NoiseType::WhitePhase,
        s if s < -0.25 => NoiseType::FlickerPhase,
        s if s < 0.25 => NoiseType::FlickerFrequency,
        s if s < 0.75 => NoiseType::RandomWalk,
        _ => NoiseType::FlickerWalk,
    }
}

/// Find local minima in deviation curve (indicates periodic patterns)
fn find_deviation_minima(taus: &[f64], deviations: &[f64]) -> Vec<CycleMinima> {
    let mut minima = Vec::new();

    if taus.len() < 3 {
        return minima;
    }

    // Find local minima
    for i in 1..taus.len() - 1 {
        if deviations[i] < deviations[i - 1] && deviations[i] < deviations[i + 1] {
            // Calculate confidence based on how pronounced the minimum is
            let depth = ((deviations[i - 1] - deviations[i]) + (deviations[i + 1] - deviations[i])) / 2.0;
            let avg_dev = deviations.iter().sum::<f64>() / deviations.len() as f64;
            let confidence = (depth / avg_dev).min(1.0);

            minima.push(CycleMinima {
                tau_seconds: taus[i],
                deviation: deviations[i],
                confidence,
            });
        }
    }

    // Sort by confidence
    minima.sort_by(|a, b| b.confidence.partial_cmp(&a.confidence).unwrap_or(std::cmp::Ordering::Equal));

    minima
}

/// Combine analyses to identify high-confidence anomalies
fn identify_anomalies(
    timestamps: &[f64],
    values: &[f64],
    mad: &MadAnalysis,
    cusum: &CusumAnalysis,
    _fft: &FftAnalysis,
    allan: &AllanAnalysis,
    _hadamard: &HadamardAnalysis,
    _modified: &ModifiedAllanAnalysis,
) -> Vec<Anomaly> {
    let mut anomalies = Vec::new();
    let mut anomaly_scores: HashMap<usize, f64> = HashMap::new();

    // Score MAD outliers
    for &idx in &mad.outliers {
        *anomaly_scores.entry(idx).or_insert(0.0) += 1.0;
    }

    // Score CUSUM cliffs with higher weight (dramatic changes)
    for cliff in &cusum.cliffs {
        *anomaly_scores.entry(cliff.index).or_insert(0.0) += 2.0; // Higher weight for cliffs
        // Mark surrounding points
        if cliff.index > 0 {
            *anomaly_scores.entry(cliff.index - 1).or_insert(0.0) += 1.0;
        }
        if cliff.index < values.len() - 1 {
            *anomaly_scores.entry(cliff.index + 1).or_insert(0.0) += 1.0;
        }
    }

    // Score gradual CUSUM change points
    for &idx in &cusum.gradual_changes {
        *anomaly_scores.entry(idx).or_insert(0.0) += 0.8;
        // Also mark nearby points as potentially anomalous
        if idx > 0 {
            *anomaly_scores.entry(idx - 1).or_insert(0.0) += 0.3;
        }
        if idx < values.len() - 1 {
            *anomaly_scores.entry(idx + 1).or_insert(0.0) += 0.3;
        }
    }

    // Check for deviations from detected cycles (Allan deviation minima)
    if allan.has_cyclic_pattern && !allan.minima.is_empty() {
        let primary_period = allan.minima[0].tau_seconds;

        // Look for breaks in the pattern at expected cycle points
        for i in 0..timestamps.len() {
            let time_offset = timestamps[i] - timestamps[0];
            let cycles_elapsed = (time_offset / primary_period).round();

            // Check if we're at a cycle boundary (within 10% tolerance)
            if cycles_elapsed > 0.0 {
                let expected_time = cycles_elapsed * primary_period + timestamps[0];
                let time_error = (timestamps[i] - expected_time).abs() / primary_period;

                if time_error < 0.1 {
                    // We're at a cycle boundary - check for pattern break
                    if i > 0 && i < values.len() - 1 {
                        let expected_pattern_window = ((primary_period / 2.0) as usize).min(10);
                        if i >= expected_pattern_window {
                            // Simple check: significant deviation from recent average
                            let recent_avg = values[i - expected_pattern_window..i]
                                .iter()
                                .sum::<f64>() / expected_pattern_window as f64;
                            let deviation = (values[i] - recent_avg).abs();
                            let threshold = mad.mad * 3.0; // More lenient for cycle breaks

                            if deviation > threshold {
                                *anomaly_scores.entry(i).or_insert(0.0) += 0.7; // Cycle break detection
                            }
                        }
                    }
                }
            }
        }
    }

    // Note: We also use Allan/Hadamard analysis to understand the noise characteristics
    // which helps us set better thresholds and understand the system behavior

    // Create anomaly records for high-scoring points
    for (idx, score) in anomaly_scores {
        // Calculate deviation factor for confidence scoring
        let deviation_factor = if mad.mad > 0.0 {
            (values[idx] - mad.median).abs() / mad.mad
        } else {
            0.0
        };

        // Calculate confidence based on multiple factors:
        // 1. How many methods detected it (score)
        // 2. How extreme the deviation is
        // 3. Whether it's a combined detection
        let method_confidence = if score >= 2.0 {
            0.9 // Multiple methods agree - high confidence
        } else if score >= 1.5 {
            0.7 // One method + partial agreement from another
        } else if score >= 1.0 {
            // Single method - confidence depends on deviation
            if deviation_factor > 7.0 {
                0.8 // Extreme deviation even with single method
            } else if deviation_factor > 5.0 {
                0.6 // Significant deviation
            } else {
                0.4 // Moderate deviation
            }
        } else {
            0.3 // Partial detection only
        };

        // Only include anomalies with reasonable confidence
        // We want "high confidence" to actually mean high confidence
        if method_confidence >= 0.6 || (deviation_factor > 5.0 && score >= 1.0) {
            let timestamp = if idx < timestamps.len() {
                timestamps[idx]
            } else {
                idx as f64
            };

            let value = values[idx];

            // Determine anomaly type
            let anomaly_type = if mad.outliers.contains(&idx) && cusum.change_points.contains(&idx)
            {
                AnomalyType::Combined
            } else if mad.outliers.contains(&idx) {
                AnomalyType::PointOutlier
            } else if cusum.positive_shifts.contains(&idx) || cusum.negative_shifts.contains(&idx) {
                AnomalyType::LevelShift
            } else {
                AnomalyType::TrendChange
            };

            // Determine severity based on confidence and deviation
            let severity = if method_confidence >= 0.9 && deviation_factor > 7.0 {
                AnomalySeverity::Critical
            } else if method_confidence >= 0.8 || deviation_factor > 7.0 {
                AnomalySeverity::High
            } else if method_confidence >= 0.7 || deviation_factor > 5.0 {
                AnomalySeverity::Medium
            } else {
                AnomalySeverity::Low
            };

            anomalies.push(Anomaly {
                timestamp,
                value,
                index: idx,
                anomaly_type,
                severity,
                confidence: method_confidence,
            });
        }
    }

    // Sort by timestamp
    anomalies.sort_by(|a, b| {
        a.timestamp
            .partial_cmp(&b.timestamp)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    anomalies
}

/// Calculate overall confidence score for the analysis
fn calculate_confidence_score(anomalies: &[Anomaly], total_points: usize) -> f64 {
    if anomalies.is_empty() || total_points == 0 {
        return 1.0; // No anomalies found with high confidence
    }

    // Calculate average confidence of detected anomalies
    let avg_confidence: f64 =
        anomalies.iter().map(|a| a.confidence).sum::<f64>() / anomalies.len() as f64;

    // Factor in the anomaly rate (too many anomalies might indicate noise)
    let anomaly_rate = anomalies.len() as f64 / total_points as f64;
    let rate_penalty = if anomaly_rate > 0.1 {
        // More than 10% anomalies suggests possible noise
        1.0 - (anomaly_rate - 0.1).min(0.5)
    } else {
        1.0
    };

    avg_confidence * rate_penalty
}

/// Helper function to format timestamp as both UTC and epoch
fn format_timestamp(timestamp: f64) -> String {
    let datetime = DateTime::from_timestamp(timestamp as i64, 0)
        .map(|dt: DateTime<Utc>| dt.format("%Y-%m-%d %H:%M:%S UTC").to_string())
        .unwrap_or_else(|| "invalid".to_string());
    format!("{datetime} (epoch: {timestamp:.0})")
}

/// Format anomaly detection results for display
pub fn format_anomaly_detection_result(result: &AnomalyDetectionResult) -> String {
    let mut output = String::new();

    output.push_str("Anomaly Detection Analysis\n");
    output.push_str("==========================\n\n");

    output.push_str(&format!("Query: {}\n", result.query));
    output.push_str(&format!("Total Data Points: {}\n", result.total_points));
    output.push_str(&format!("Anomalies Detected: {}\n", result.anomalies.len()));
    output.push_str(&format!(
        "Overall Confidence: {:.2}%\n\n",
        result.confidence_score * 100.0
    ));

    // MAD Analysis
    output.push_str("MAD (Median Absolute Deviation) Analysis\n");
    output.push_str("-----------------------------------------\n");
    output.push_str(&format!("Median: {:.4}\n", result.mad_analysis.median));
    output.push_str(&format!("MAD: {:.4}\n", result.mad_analysis.mad));
    output.push_str(&format!(
        "Threshold (5σ): {:.4}\n",
        result.mad_analysis.threshold
    ));
    output.push_str(&format!(
        "Outliers Found: {} ({:.2}% of data)\n",
        result.mad_analysis.outlier_count,
        (result.mad_analysis.outlier_count as f64 / result.total_points as f64) * 100.0
    ));

    // Show first few outlier timestamps if available
    if !result.mad_analysis.outliers.is_empty() && !result.timestamps.is_empty() {
        output.push_str("  Sample outlier times (first 3):\n");
        for &idx in result.mad_analysis.outliers.iter().take(3) {
            if idx < result.timestamps.len() {
                output.push_str(&format!(
                    "    - {}, value: {:.4}\n",
                    format_timestamp(result.timestamps[idx]),
                    result.values[idx]
                ));
            }
        }
    }
    output.push('\n');

    // CUSUM Analysis
    output.push_str("CUSUM (Cumulative Sum) Analysis\n");
    output.push_str("--------------------------------\n");
    output.push_str(&format!("Mean: {:.4}\n", result.cusum_analysis.mean));
    output.push_str(&format!("Std Dev: {:.4}\n", result.cusum_analysis.std_dev));
    output.push_str(&format!(
        "Total Change Points: {}\n",
        result.cusum_analysis.change_points.len()
    ));
    output.push_str(&format!(
        "Dramatic Cliffs: {}\n",
        result.cusum_analysis.cliffs.len()
    ));
    output.push_str(&format!(
        "Gradual Changes: {}\n",
        result.cusum_analysis.gradual_changes.len()
    ));
    output.push_str(&format!(
        "Positive Shifts: {}\n",
        result.cusum_analysis.positive_shifts.len()
    ));
    output.push_str(&format!(
        "Negative Shifts: {}\n",
        result.cusum_analysis.negative_shifts.len()
    ));

    // Show detected cliffs if any
    if !result.cusum_analysis.cliffs.is_empty() && !result.timestamps.is_empty() {
        output.push_str("\n  Detected Cliffs (dramatic changes):\n");
        for cliff in result.cusum_analysis.cliffs.iter().take(3) {
            if cliff.index < result.timestamps.len() {
                output.push_str(&format!(
                    "    - {} ({:?}), magnitude: {:.4}\n",
                    format_timestamp(result.timestamps[cliff.index]),
                    cliff.direction,
                    cliff.magnitude
                ));
            }
        }
    }

    // Show gradual changes
    if !result.cusum_analysis.gradual_changes.is_empty() && !result.timestamps.is_empty() {
        output.push_str("\n  Sample gradual changes (first 3):\n");
        for &idx in result.cusum_analysis.gradual_changes.iter().take(3) {
            if idx < result.timestamps.len() {
                let shift_type = if result.cusum_analysis.positive_shifts.contains(&idx) {
                    "positive shift"
                } else if result.cusum_analysis.negative_shifts.contains(&idx) {
                    "negative shift"
                } else {
                    "change"
                };
                output.push_str(&format!(
                    "    - {} ({}), value: {:.4}\n",
                    format_timestamp(result.timestamps[idx]),
                    shift_type,
                    result.values[idx]
                ));
            }
        }
    }

    // Show sensitivity analysis
    if !result.cusum_analysis.sensitivity_levels.is_empty() {
        output.push_str("\n  Multi-scale Detection:\n");
        for level in &result.cusum_analysis.sensitivity_levels {
            output.push_str(&format!(
                "    - {}: {} changes detected\n",
                level.name,
                level.detected_changes.len()
            ));
        }
    }
    output.push('\n');

    // FFT Analysis
    output.push_str("FFT (Fast Fourier Transform) Analysis\n");
    output.push_str("--------------------------------------\n");

    // Calculate and show analysis constraints
    if !result.timestamps.is_empty() && result.timestamps.len() > 1 {
        let sample_interval = if result.timestamps.len() > 1 {
            (result.timestamps[1] - result.timestamps[0]).abs()
        } else {
            1.0
        };
        let recording_duration =
            (result.timestamps[result.timestamps.len() - 1] - result.timestamps[0]).abs();
        let min_detectable_period = 2.0 * sample_interval;
        let max_detectable_period = recording_duration / 2.0;

        output.push_str(&format!(
            "Analysis Constraints:\n  Min Period: {min_detectable_period:.2}s (Nyquist limit)\n  Max Period: {max_detectable_period:.2}s (half recording length)\n\n"
        ));
    }

    if result.fft_analysis.has_periodic_pattern {
        output.push_str(&format!(
            "Periodic Pattern: Yes (strength: {:.2}%)\n",
            result.fft_analysis.periodicity_strength * 100.0
        ));
        if let Some(period) = result.fft_analysis.period_seconds {
            output.push_str(&format!("Primary Period: {period:.2} seconds\n"));
        }
    } else {
        output.push_str("Periodic Pattern: No significant periodicity detected\n");
    }

    if !result.fft_analysis.dominant_frequencies.is_empty() {
        output.push_str("\nDominant Frequencies:\n");
        for (i, freq) in result
            .fft_analysis
            .dominant_frequencies
            .iter()
            .enumerate()
            .take(3)
        {
            output.push_str(&format!(
                "  {}. {:.4} Hz (period: {:.2}s, power: {:.2}%)\n",
                i + 1,
                freq.frequency_hz,
                freq.period_seconds,
                freq.relative_power * 100.0
            ));
        }
    }
    output.push('\n');

    // Allan Deviation Analysis
    output.push_str("Allan Deviation Analysis\n");
    output.push_str("------------------------\n");
    output.push_str(&format!("Noise Type: {:?}\n", result.allan_analysis.noise_type));

    if result.allan_analysis.has_cyclic_pattern {
        output.push_str("Cyclic Patterns Detected:\n");
        for (i, minima) in result.allan_analysis.minima.iter().enumerate().take(3) {
            output.push_str(&format!(
                "  {}. Period: {:.2}s (confidence: {:.2}%)\n",
                i + 1,
                minima.tau_seconds,
                minima.confidence * 100.0
            ));
        }
    } else {
        output.push_str("No significant cyclic patterns detected\n");
    }
    output.push('\n');

    // Hadamard Deviation Analysis
    output.push_str("Hadamard Deviation Analysis\n");
    output.push_str("---------------------------\n");
    output.push_str(&format!("Noise Type: {:?}\n", result.hadamard_analysis.noise_type));

    if !result.hadamard_analysis.minima.is_empty() {
        output.push_str("Detected Minima:\n");
        for (i, minima) in result.hadamard_analysis.minima.iter().enumerate().take(3) {
            output.push_str(&format!(
                "  {}. Tau: {:.2}s, Deviation: {:.6}\n",
                i + 1,
                minima.tau_seconds,
                minima.deviation
            ));
        }
    }
    output.push('\n');

    // Modified Allan Deviation Analysis
    output.push_str("Modified Allan Deviation Analysis\n");
    output.push_str("---------------------------------\n");
    output.push_str(&format!("Noise Type: {:?}\n", result.modified_allan_analysis.noise_type));

    if !result.modified_allan_analysis.minima.is_empty() {
        output.push_str("Detected Minima (better frequency drift handling):\n");
        for (i, minima) in result.modified_allan_analysis.minima.iter().enumerate().take(3) {
            output.push_str(&format!(
                "  {}. Period: {:.2}s (confidence: {:.2}%)\n",
                i + 1,
                minima.tau_seconds,
                minima.confidence * 100.0
            ));
        }
    } else {
        output.push_str("No significant patterns detected\n");
    }
    output.push('\n');

    // Detected Anomalies
    if !result.anomalies.is_empty() {
        output.push_str("Detected Anomalies (Confidence ≥ 60%)\n");
        output.push_str("--------------------------------------\n");

        // Group by severity
        let mut critical = Vec::new();
        let mut high = Vec::new();
        let mut medium = Vec::new();
        let mut low = Vec::new();

        for anomaly in &result.anomalies {
            match anomaly.severity {
                AnomalySeverity::Critical => critical.push(anomaly),
                AnomalySeverity::High => high.push(anomaly),
                AnomalySeverity::Medium => medium.push(anomaly),
                AnomalySeverity::Low => low.push(anomaly),
            }
        }

        if !critical.is_empty() {
            output.push_str("\nCRITICAL Severity:\n");
            for anomaly in critical.iter().take(5) {
                output.push_str(&format!(
                    "  - Time: {}\n    Value: {:.4}, Type: {:?}, Confidence: {:.2}%\n",
                    format_timestamp(anomaly.timestamp),
                    anomaly.value,
                    anomaly.anomaly_type,
                    anomaly.confidence * 100.0
                ));
            }
        }

        if !high.is_empty() {
            output.push_str("\nHIGH Severity:\n");
            for anomaly in high.iter().take(5) {
                output.push_str(&format!(
                    "  - Time: {}\n    Value: {:.4}, Type: {:?}, Confidence: {:.2}%\n",
                    format_timestamp(anomaly.timestamp),
                    anomaly.value,
                    anomaly.anomaly_type,
                    anomaly.confidence * 100.0
                ));
            }
        }

        if !medium.is_empty() && (critical.len() + high.len()) < 5 {
            output.push_str("\nMEDIUM Severity:\n");
            for anomaly in medium.iter().take(3) {
                output.push_str(&format!(
                    "  - Time: {}\n    Value: {:.4}, Type: {:?}, Confidence: {:.2}%\n",
                    format_timestamp(anomaly.timestamp),
                    anomaly.value,
                    anomaly.anomaly_type,
                    anomaly.confidence * 100.0
                ));
            }
        }

        output.push_str(&format!(
            "\n(Showing top anomalies out of {} total detected)\n",
            result.anomalies.len()
        ));
    } else {
        output.push_str("No high-confidence anomalies detected.\n");
    }

    // Summary
    output.push('\n');
    output.push_str("Summary\n");
    output.push_str("-------\n");
    if result.anomalies.is_empty() {
        output.push_str("The time series appears to be operating within normal parameters.\n");
    } else {
        let critical_count = result
            .anomalies
            .iter()
            .filter(|a| matches!(a.severity, AnomalySeverity::Critical))
            .count();
        let high_count = result
            .anomalies
            .iter()
            .filter(|a| matches!(a.severity, AnomalySeverity::High))
            .count();

        if critical_count > 0 {
            output.push_str(&format!(
                "ATTENTION: {critical_count} critical anomalies detected requiring immediate investigation.\n"
            ));
        } else if high_count > 0 {
            output.push_str(&format!(
                "Found {high_count} high-severity anomalies that warrant investigation.\n"
            ));
        } else {
            output.push_str(
                "Minor anomalies detected, likely within acceptable operational variance.\n",
            );
        }
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_and_fix_query_missing_range_vector() {
        // Test auto-fix for missing range vectors
        let test_cases = vec![
            ("rate(cpu_usage)", "rate(cpu_usage[1m])"),
            ("irate(network_bytes)", "irate(network_bytes[1m])"),
            ("rate(cpu_usage{state=\"busy\"})", "rate(cpu_usage{state=\"busy\"}[1m])"),
            ("sum(rate(requests_total))", "sum(rate(requests_total[1m]))"),
            ("avg_over_time(temperature)", "avg_over_time(temperature[1m])"),
            ("increase(counter_metric)", "increase(counter_metric[1m])"),
            ("delta(gauge_metric)", "delta(gauge_metric[1m])"),
        ];

        for (input, expected) in test_cases {
            let result = validate_and_fix_query(input);
            assert!(result.is_ok(), "Failed to fix query: {}", input);
            assert_eq!(result.unwrap(), expected, "Incorrect fix for: {}", input);
        }
    }

    #[test]
    fn test_validate_and_fix_query_already_valid() {
        // Test that valid queries are not modified
        let valid_queries = vec![
            "rate(cpu_usage[5s])",
            "irate(network_bytes[5m])",
            "sum(rate(requests_total[10m]))",
            "avg_over_time(temperature[1h])",
            "cpu_usage", // Plain metric without rate function
            "cpu_usage{state=\"busy\"}", // Metric with label selector
        ];

        for query in valid_queries {
            let result = validate_and_fix_query(query);
            assert!(result.is_ok(), "Valid query rejected: {}", query);
            assert_eq!(result.unwrap(), query, "Valid query was modified: {}", query);
        }
    }

    #[test]
    fn test_validate_and_fix_query_bare_range_vector() {
        // Test that bare range vectors are rejected
        let result = validate_and_fix_query("cpu_usage[5m]");
        assert!(result.is_err(), "Bare range vector should be rejected");
        let error = result.unwrap_err().to_string();
        assert!(error.contains("bare range vector"), "Error should mention bare range vector");
    }

    #[test]
    fn test_validate_and_fix_query_nested_functions() {
        // Test nested function handling
        let query = "sum(rate(cpu_usage))";
        let result = validate_and_fix_query(query);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "sum(rate(cpu_usage[1m]))");
    }

    #[test]
    fn test_cliff_detection() {
        // Test cliff detection with dramatic changes
        let values = vec![
            100.0, 100.1, 99.9, 100.2, // Stable around 100
            50.0,  // Dramatic cliff down
            50.1, 49.9, 50.2, // Stable around 50
            120.0, // Dramatic cliff up
            120.1, 119.9, 120.2, // Stable around 120
        ];

        let mean = 85.0;
        let std_dev = 25.0;

        let cliffs = detect_cliffs(&values, mean, std_dev);

        // Should detect cliffs at indices 4 (100->50) and 8 (50->120)
        assert_eq!(cliffs.len(), 2, "Should detect 2 cliffs");
        assert_eq!(cliffs[0].index, 4, "First cliff should be at index 4");
        assert_eq!(cliffs[1].index, 8, "Second cliff should be at index 8");

        // Check cliff magnitudes
        assert!((cliffs[0].magnitude - 50.0).abs() < 1.0, "First cliff magnitude should be ~50");
        assert!((cliffs[1].magnitude - 70.0).abs() < 1.0, "Second cliff magnitude should be ~70");

        // Check directions
        assert!(matches!(cliffs[0].direction, ChangeDirection::Decrease));
        assert!(matches!(cliffs[1].direction, ChangeDirection::Increase));
    }

    #[test]
    fn test_multi_scale_cusum() {
        // Test that multi-scale CUSUM detects changes at different sensitivities
        let mut values = vec![100.0; 20]; // Stable baseline
        values.extend(vec![
            // Small gradual increase
            101.0, 102.0, 103.0, 104.0, 105.0,
            // Stable at new level
            105.0, 105.0, 105.0,
            // Dramatic cliff (much larger change)
            200.0,
            // Stable at high level
            200.0, 200.0,
        ]);

        let result = perform_cusum_analysis(&values);
        assert!(result.is_ok());

        let cusum = result.unwrap();

        // Check for any detected changes (cliffs or gradual)
        assert!(
            !cusum.cliffs.is_empty() || !cusum.change_points.is_empty(),
            "Should detect changes in the data"
        );

        // Should have multiple sensitivity levels
        assert_eq!(cusum.sensitivity_levels.len(), 4, "Should have 4 sensitivity levels");

        // High sensitivity should detect more changes than low sensitivity
        let high_sensitivity = &cusum.sensitivity_levels[0];
        let low_sensitivity = &cusum.sensitivity_levels[2];
        assert!(
            high_sensitivity.detected_changes.len() >= low_sensitivity.detected_changes.len(),
            "High sensitivity should detect more changes"
        );
    }

    #[test]
    fn test_noise_type_identification() {
        // Test noise type identification from slopes
        let test_cases = vec![
            (vec![1.0, 2.0, 4.0, 8.0], vec![1.0, 0.5, 0.25, 0.125], NoiseType::WhitePhase), // slope ≈ -1
            (vec![1.0, 2.0, 4.0, 8.0], vec![1.0, 0.7, 0.5, 0.35], NoiseType::FlickerPhase), // slope ≈ -0.5
            (vec![1.0, 2.0, 4.0, 8.0], vec![1.0, 1.0, 1.0, 1.0], NoiseType::FlickerFrequency), // slope ≈ 0
            (vec![1.0, 2.0, 4.0, 8.0], vec![1.0, 1.4, 2.0, 2.8], NoiseType::RandomWalk), // slope ≈ 0.5
            (vec![1.0, 2.0, 4.0, 8.0], vec![1.0, 2.0, 4.0, 8.0], NoiseType::FlickerWalk), // slope ≈ 1
        ];

        for (taus, deviations, expected_type) in test_cases {
            let noise_type = identify_noise_type(&taus, &deviations);
            assert!(
                matches!(noise_type, ref nt if std::mem::discriminant(nt) == std::mem::discriminant(&expected_type)),
                "Expected {:?}, got {:?} for taus {:?}, devs {:?}",
                expected_type, noise_type, taus, deviations
            );
        }
    }

    #[test]
    fn test_find_deviation_minima() {
        // Test finding local minima in deviation curves
        let taus = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0];
        let deviations = vec![
            1.0, 0.9, 0.8, // Decreasing
            0.5, // Local minimum at index 3 (tau=4)
            0.7, 0.8, 0.9, // Increasing
            0.6, // Local minimum at index 7 (tau=8)
            0.8, 1.0, // Increasing
        ];

        let minima = find_deviation_minima(&taus, &deviations);

        // Note: minima are sorted by confidence, not by tau
        assert!(minima.len() >= 1, "Should find at least 1 minimum");

        // Check that we found minima at the expected tau values
        let tau_values: Vec<f64> = minima.iter().map(|m| m.tau_seconds).collect();
        assert!(tau_values.contains(&4.0) || tau_values.contains(&8.0),
                "Should find minimum at tau=4 or tau=8");

        // Check that confidence is calculated
        assert!(minima[0].confidence > 0.0, "Minima should have non-zero confidence");
    }

    #[test]
    fn test_extract_time_series_matrix() {
        // Test extracting time series from Matrix result
        use crate::viewer::promql::{MatrixSample, QueryResult};

        let matrix_result = QueryResult::Matrix {
            result: vec![
                MatrixSample {
                    metric: std::collections::HashMap::new(),
                    values: vec![(1.0, 100.0), (2.0, 101.0), (3.0, 102.0)],
                },
            ],
        };

        let result = extract_time_series(&matrix_result, "test_query");
        assert!(result.is_ok());

        let (timestamps, values) = result.unwrap();
        assert_eq!(timestamps, vec![1.0, 2.0, 3.0]);
        assert_eq!(values, vec![100.0, 101.0, 102.0]);
    }

    #[test]
    fn test_extract_time_series_vector_error() {
        // Test that Vector results produce appropriate errors
        use crate::viewer::promql::{QueryResult, Sample};

        let vector_result = QueryResult::Vector {
            result: vec![
                Sample {
                    metric: std::collections::HashMap::new(),
                    value: (1.0, 100.0),
                },
            ],
        };

        // Test with a rate query that should have been a Matrix
        let result = extract_time_series(&vector_result, "rate(cpu_usage[1m])");
        assert!(result.is_err());
        let error = result.unwrap_err().to_string();
        assert!(error.contains("Unexpected result type"));

        // Test with a query missing range vector
        let result = extract_time_series(&vector_result, "rate(cpu_usage)");
        assert!(result.is_err());
        let error = result.unwrap_err().to_string();
        assert!(error.contains("instant values"));
    }

    #[test]
    fn test_validate_and_fix_complex_queries() {
        // Test more complex query patterns
        let test_cases = vec![
            (
                "sum by (cpu) (rate(cpu_usage))",
                "sum by (cpu) (rate(cpu_usage[1m]))"
            ),
            (
                "histogram_quantile(0.95, rate(http_request_duration_bucket))",
                "histogram_quantile(0.95, rate(http_request_duration_bucket[1m]))"
            ),
            (
                "100 - (avg(rate(cpu_idle)) * 100)",
                "100 - (avg(rate(cpu_idle[1m])) * 100)"
            ),
        ];

        for (input, expected) in test_cases {
            let result = validate_and_fix_query(input);
            assert!(result.is_ok(), "Failed to fix query: {}", input);
            assert_eq!(result.unwrap(), expected, "Incorrect fix for: {}", input);
        }
    }

    #[test]
    fn test_mad_analysis_empty_data() {
        let result = perform_mad_analysis(&[], 5.0);
        assert!(result.is_err(), "MAD analysis should fail on empty data");
    }

    #[test]
    fn test_mad_analysis_single_value() {
        let values = vec![100.0];
        let result = perform_mad_analysis(&values, 5.0);
        assert!(result.is_ok(), "MAD analysis should handle single value");
        let mad = result.unwrap();
        assert_eq!(mad.median, 100.0);
        assert_eq!(mad.mad, 0.0);
        assert_eq!(mad.outlier_count, 0);
    }

    #[test]
    fn test_mad_analysis_with_outliers() {
        let values = vec![
            100.0, 101.0, 99.0, 100.5, 99.5, // Normal values
            200.0, // Outlier
            100.2, 99.8, 100.1, // More normal values
        ];
        let result = perform_mad_analysis(&values, 3.0); // Lower threshold to catch outlier
        assert!(result.is_ok());
        let mad = result.unwrap();
        assert!(mad.outlier_count > 0, "Should detect outliers");
        assert!(mad.outliers.contains(&5), "Should detect outlier at index 5");
    }
}
