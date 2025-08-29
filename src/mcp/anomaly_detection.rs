use crate::viewer::promql::{QueryEngine, QueryResult};
use crate::viewer::tsdb::Tsdb;
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

/// Perform anomaly detection on a time series
pub fn detect_anomalies(
    engine: &Arc<QueryEngine>,
    _tsdb: &Arc<Tsdb>,
    query: &str,
) -> Result<AnomalyDetectionResult, Box<dyn std::error::Error>> {
    // Execute the query to get time series data
    let (start_time, end_time) = engine.get_time_range();
    let step = 1.0; // 1 second resolution

    let query_result = engine.query_range(query, start_time, end_time, step)?;

    // Extract time series data
    let (timestamps, values) = extract_time_series(&query_result)?;

    if values.is_empty() {
        return Err("No data points found for the given query".into());
    }

    // Perform MAD analysis with conservative threshold
    let mad_analysis = perform_mad_analysis(&values, 5.0)?;

    // Perform CUSUM analysis
    let cusum_analysis = perform_cusum_analysis(&values)?;

    // Perform FFT analysis (step is the sample interval in seconds)
    let fft_analysis = perform_fft_analysis(&values, step)?;

    // Combine analyses to identify high-confidence anomalies
    let anomalies = identify_anomalies(
        &timestamps,
        &values,
        &mad_analysis,
        &cusum_analysis,
        &fft_analysis,
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
        anomalies,
        confidence_score,
    })
}

/// Extract time series data from query result
fn extract_time_series(
    result: &QueryResult,
) -> Result<(Vec<f64>, Vec<f64>), Box<dyn std::error::Error>> {
    match result {
        QueryResult::Vector { result } => {
            // For vector results, we have a single point per series
            // This is less useful for anomaly detection
            if result.is_empty() {
                return Ok((vec![], vec![]));
            }

            // Just return the current values as a single point
            let timestamps: Vec<f64> = result.iter().map(|sample| sample.value.0).collect();
            let values: Vec<f64> = result.iter().map(|sample| sample.value.1).collect();
            Ok((timestamps, values))
        }
        QueryResult::Matrix { result } => {
            // For matrix results, combine all series or pick the first one
            if result.is_empty() {
                return Ok((vec![], vec![]));
            }

            // Use the first series for now
            let series = &result[0];
            let timestamps: Vec<f64> = series.values.iter().map(|(ts, _)| *ts).collect();
            let values: Vec<f64> = series.values.iter().map(|(_, val)| *val).collect();
            Ok((timestamps, values))
        }
        QueryResult::Scalar { result } => {
            // Single scalar value (timestamp, value)
            Ok((vec![result.0], vec![result.1]))
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

/// Perform CUSUM (Cumulative Sum) analysis
fn perform_cusum_analysis(values: &[f64]) -> Result<CusumAnalysis, Box<dyn std::error::Error>> {
    if values.is_empty() {
        return Err("Cannot perform CUSUM analysis on empty dataset".into());
    }

    // Calculate mean and standard deviation
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    let variance = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / values.len() as f64;
    let std_dev = variance.sqrt();

    // CUSUM parameters
    let k = 0.5 * std_dev; // Allowance parameter
    let h = 4.0 * std_dev; // Decision threshold (conservative)

    // Initialize CUSUM statistics
    let mut s_high = 0.0;
    let mut s_low = 0.0;
    let mut change_points = Vec::new();
    let mut positive_shifts = Vec::new();
    let mut negative_shifts = Vec::new();

    for (i, &value) in values.iter().enumerate() {
        // Update CUSUM statistics
        s_high = f64::max(0.0, s_high + value - mean - k);
        s_low = f64::max(0.0, s_low + mean - k - value);

        // Check for change points
        if s_high > h {
            positive_shifts.push(i);
            change_points.push(i);
            s_high = 0.0; // Reset after detection
        }

        if s_low > h {
            negative_shifts.push(i);
            change_points.push(i);
            s_low = 0.0; // Reset after detection
        }
    }

    // Remove duplicate change points and sort
    change_points.sort_unstable();
    change_points.dedup();

    Ok(CusumAnalysis {
        mean,
        std_dev,
        threshold: h,
        change_points,
        positive_shifts,
        negative_shifts,
    })
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

/// Combine analyses to identify high-confidence anomalies
fn identify_anomalies(
    timestamps: &[f64],
    values: &[f64],
    mad: &MadAnalysis,
    cusum: &CusumAnalysis,
    _fft: &FftAnalysis,
) -> Vec<Anomaly> {
    let mut anomalies = Vec::new();
    let mut anomaly_scores: HashMap<usize, f64> = HashMap::new();

    // Score MAD outliers
    for &idx in &mad.outliers {
        *anomaly_scores.entry(idx).or_insert(0.0) += 1.0;
    }

    // Score CUSUM change points
    for &idx in &cusum.change_points {
        *anomaly_scores.entry(idx).or_insert(0.0) += 1.0;
        // Also mark nearby points as potentially anomalous
        if idx > 0 {
            *anomaly_scores.entry(idx - 1).or_insert(0.0) += 0.5;
        }
        if idx < values.len() - 1 {
            *anomaly_scores.entry(idx + 1).or_insert(0.0) += 0.5;
        }
    }

    // Note: We don't check for periodicity breaks since patterns in these systems
    // are likely to change dynamically. The FFT analysis identifies dominant
    // frequencies but doesn't flag deviations from them as anomalies.

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
        "Change Points: {}\n",
        result.cusum_analysis.change_points.len()
    ));
    output.push_str(&format!(
        "Positive Shifts: {}\n",
        result.cusum_analysis.positive_shifts.len()
    ));
    output.push_str(&format!(
        "Negative Shifts: {}\n",
        result.cusum_analysis.negative_shifts.len()
    ));

    // Show first few change point timestamps if available
    if !result.cusum_analysis.change_points.is_empty() && !result.timestamps.is_empty() {
        output.push_str("  Sample change point times (first 3):\n");
        for &idx in result.cusum_analysis.change_points.iter().take(3) {
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
