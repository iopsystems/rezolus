use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

mod cusum;
mod mad;
mod stability;

pub use cusum::CusumAnalysis;
pub use mad::MadAnalysis;
pub use stability::{AllanAnalysis, HadamardAnalysis, ModifiedAllanAnalysis, NoiseType};

/// Result of anomaly detection analysis
#[derive(Debug, Serialize, Deserialize)]
pub struct AnomalyDetectionResult {
    pub query: String,
    pub total_points: usize,
    pub timestamps: Vec<f64>,
    pub values: Vec<f64>,
    pub smoothed_values: Option<Vec<f64>>,
    pub smoothing_window: Option<f64>,
    pub mad_analysis: MadAnalysis,
    pub cusum_analysis: CusumAnalysis,
    pub allan_analysis: AllanAnalysis,
    pub hadamard_analysis: HadamardAnalysis,
    pub modified_allan_analysis: ModifiedAllanAnalysis,
    pub anomalies: Vec<Anomaly>,
    pub confidence_score: f64,
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

/// Run the statistical analyses (Allan/Hadamard/MAD/CUSUM) on
/// `(timestamps, values)` and assemble the [`AnomalyDetectionResult`].
/// `query_label` is recorded on the result for user-facing display;
/// `step` is the sampling interval in seconds.
pub fn analyze_time_series(
    query_label: String,
    timestamps: Vec<f64>,
    values: Vec<f64>,
    step: f64,
) -> Result<AnomalyDetectionResult, Box<dyn std::error::Error>> {
    if values.is_empty() {
        return Err("No data points to analyze. The query returned an empty time series.".into());
    }

    // Allan/Hadamard/Modified Allan determine the noise type + optimal
    // smoothing window.
    let allan_analysis = stability::perform_allan_analysis(&values, step)?;
    let hadamard_analysis = stability::perform_hadamard_analysis(&values, step)?;
    let modified_allan_analysis = stability::perform_modified_allan_analysis(&values, step)?;

    let allan_window = if !allan_analysis.minima.is_empty() {
        allan_analysis.minima[0].tau_seconds
    } else {
        match allan_analysis.noise_type {
            NoiseType::WhitePhase | NoiseType::FlickerPhase => 15.0 * step,
            NoiseType::WhiteFrequency | NoiseType::FlickerFrequency => 30.0 * step,
            NoiseType::RandomWalk | NoiseType::FlickerWalk => 60.0 * step,
            NoiseType::Unknown => 30.0,
        }
    };

    let (smoothed_values, smoothing_window) = apply_allan_smoothing(&values, &allan_analysis, step);
    let use_smoothed = smoothing_window > 0.0;
    let analysis_values = if use_smoothed {
        &smoothed_values
    } else {
        &values
    };

    let mad_threshold = match allan_analysis.noise_type {
        NoiseType::WhitePhase | NoiseType::FlickerPhase => 4.0,
        NoiseType::WhiteFrequency | NoiseType::FlickerFrequency => 5.0,
        NoiseType::RandomWalk | NoiseType::FlickerWalk => 6.5,
        NoiseType::Unknown => 5.0,
    };
    let mad_analysis = mad::perform_mad_analysis(analysis_values, mad_threshold)?;
    let cusum_analysis =
        cusum::perform_cusum_analysis_with_allan(&values, step, allan_window, &allan_analysis)?;

    let anomalies = identify_anomalies(
        &timestamps,
        analysis_values,
        &mad_analysis,
        &cusum_analysis,
        &allan_analysis,
        &hadamard_analysis,
        &modified_allan_analysis,
    );
    let confidence_score = calculate_confidence_score(&anomalies, values.len());
    let total_points = values.len();

    Ok(AnomalyDetectionResult {
        query: query_label,
        total_points,
        timestamps,
        values,
        smoothed_values: if use_smoothed {
            Some(smoothed_values)
        } else {
            None
        },
        smoothing_window: if use_smoothed {
            Some(smoothing_window)
        } else {
            None
        },
        mad_analysis,
        cusum_analysis,
        allan_analysis,
        hadamard_analysis,
        modified_allan_analysis,
        anomalies,
        confidence_score,
    })
}

/// Apply moving average smoothing using Allan-determined window
/// Returns (smoothed_values, window_seconds)
fn apply_allan_smoothing(
    values: &[f64],
    allan_analysis: &AllanAnalysis,
    sample_interval: f64,
) -> (Vec<f64>, f64) {
    // Determine optimal averaging window from Allan deviation
    // Use the first minimum (optimal tau) if available, otherwise use noise characteristics
    let window_seconds = if !allan_analysis.minima.is_empty() {
        // Use the primary minimum as the optimal averaging time
        allan_analysis.minima[0].tau_seconds
    } else {
        // Fallback: use heuristic based on noise type
        // Larger windows to better filter noise and detect regime shifts
        match allan_analysis.noise_type {
            NoiseType::WhitePhase | NoiseType::FlickerPhase => {
                // High frequency noise - use moderate window
                15.0 * sample_interval // Was 5.0
            }
            NoiseType::WhiteFrequency | NoiseType::FlickerFrequency => {
                // Medium frequency noise
                30.0 * sample_interval // Was 10.0
            }
            NoiseType::RandomWalk | NoiseType::FlickerWalk => {
                // Low frequency drift - use larger window
                60.0 * sample_interval // Was 20.0
            }
            NoiseType::Unknown => {
                // Default: 30 seconds for better smoothing
                30.0
            }
        }
    };

    // Convert window from seconds to number of samples
    let window_samples = ((window_seconds / sample_interval).round() as usize).max(1);

    // Don't smooth if window is too small or data is too short
    if window_samples <= 2 || values.len() < window_samples * 2 {
        return (values.to_vec(), 0.0);
    }

    // Apply simple moving average
    let mut smoothed = Vec::with_capacity(values.len());

    for i in 0..values.len() {
        // Use centered window where possible, otherwise use asymmetric window
        let half_window = window_samples / 2;

        let (start, end) = if i < half_window {
            // Near start - use forward window
            (0, window_samples.min(values.len()))
        } else if i + half_window >= values.len() {
            // Near end - use backward window
            (values.len().saturating_sub(window_samples), values.len())
        } else {
            // Middle - use centered window
            (
                i.saturating_sub(half_window),
                (i + half_window + 1).min(values.len()),
            )
        };

        let window = &values[start..end];
        let avg = window.iter().sum::<f64>() / window.len() as f64;
        smoothed.push(avg);
    }

    (smoothed, window_seconds)
}

/// Combine analyses to identify high-confidence anomalies
fn identify_anomalies(
    timestamps: &[f64],
    values: &[f64],
    mad: &MadAnalysis,
    cusum: &CusumAnalysis,
    allan: &AllanAnalysis,
    hadamard: &HadamardAnalysis,
    modified: &ModifiedAllanAnalysis,
) -> Vec<Anomaly> {
    let mut anomalies = Vec::new();
    let mut anomaly_scores: HashMap<usize, f64> = HashMap::new();

    for &idx in &mad.outliers {
        *anomaly_scores.entry(idx).or_insert(0.0) += 1.0;
    }

    // Score window-based change points with VERY HIGH weight (sustained regime shifts)
    // These are the most important detections for experiments
    for wcp in &cusum.window_change_points {
        let weight = 4.0 * wcp.confidence; // Scale by confidence, max 4.0
        *anomaly_scores.entry(wcp.index).or_insert(0.0) += weight;
        // Mark surrounding region as potentially anomalous
        for offset in 1..=5 {
            if wcp.index >= offset {
                *anomaly_scores.entry(wcp.index - offset).or_insert(0.0) += weight * 0.3;
            }
            if wcp.index + offset < values.len() {
                *anomaly_scores.entry(wcp.index + offset).or_insert(0.0) += weight * 0.3;
            }
        }
    }

    // Score CUSUM cliffs with highest weight (dramatic changes)
    for cliff in &cusum.cliffs {
        *anomaly_scores.entry(cliff.index).or_insert(0.0) += 3.0; // Highest weight for cliffs
                                                                  // Mark surrounding points with lower weight
        if cliff.index > 0 {
            *anomaly_scores.entry(cliff.index - 1).or_insert(0.0) += 0.8;
        }
        if cliff.index < values.len() - 1 {
            *anomaly_scores.entry(cliff.index + 1).or_insert(0.0) += 0.8;
        }
    }

    // Score CUSUM positive/negative shifts with high weight (regime changes)
    for &idx in &cusum.positive_shifts {
        *anomaly_scores.entry(idx).or_insert(0.0) += 2.0; // Regime shift up (was 1.5)
    }

    for &idx in &cusum.negative_shifts {
        *anomaly_scores.entry(idx).or_insert(0.0) += 2.0; // Regime shift down (was 1.5)
    }

    // Score gradual CUSUM change points with lower weight to reduce false positives
    for &idx in &cusum.gradual_changes {
        // Only add score if not already counted as positive/negative shift
        if !cusum.positive_shifts.contains(&idx) && !cusum.negative_shifts.contains(&idx) {
            *anomaly_scores.entry(idx).or_insert(0.0) += 0.5; // Reduced from 0.8
        }
        // Don't mark nearby points for gradual changes - too noisy
    }

    // Score noise characteristic transitions (fundamental system behavior changes)
    // These are VERY important - they indicate the system's dynamics have changed
    for transition in &allan.noise_transitions {
        // Weight based on confidence and severity of change
        let base_weight = 3.5 * transition.confidence; // High base weight for noise transitions

        // Extra weight for dramatic deviation changes
        let deviation_weight = if transition.deviation_change_factor > 3.0
            || transition.deviation_change_factor < 0.33
        {
            1.0 // Very dramatic change
        } else if transition.deviation_change_factor > 2.0
            || transition.deviation_change_factor < 0.5
        {
            0.5 // Moderate change
        } else {
            0.0
        };

        let total_weight = base_weight + deviation_weight;
        *anomaly_scores.entry(transition.index).or_insert(0.0) += total_weight;

        // Mark surrounding region - noise transitions affect a window
        for offset in 1..=10 {
            if transition.index >= offset {
                *anomaly_scores
                    .entry(transition.index - offset)
                    .or_insert(0.0) += total_weight * 0.2;
            }
            if transition.index + offset < values.len() {
                *anomaly_scores
                    .entry(transition.index + offset)
                    .or_insert(0.0) += total_weight * 0.2;
            }
        }
    }

    // Score Hadamard noise transitions (complementary view to Allan)
    // Hadamard is better for frequency noise and less sensitive to linear drift
    for transition in &hadamard.noise_transitions {
        let base_weight = 3.0 * transition.confidence; // Slightly lower than Allan

        let deviation_weight = if transition.deviation_change_factor > 3.0
            || transition.deviation_change_factor < 0.33
        {
            0.8
        } else if transition.deviation_change_factor > 2.0
            || transition.deviation_change_factor < 0.5
        {
            0.4
        } else {
            0.0
        };

        let total_weight = base_weight + deviation_weight;
        *anomaly_scores.entry(transition.index).or_insert(0.0) += total_weight;

        // Mark surrounding region
        for offset in 1..=10 {
            if transition.index >= offset {
                *anomaly_scores
                    .entry(transition.index - offset)
                    .or_insert(0.0) += total_weight * 0.2;
            }
            if transition.index + offset < values.len() {
                *anomaly_scores
                    .entry(transition.index + offset)
                    .or_insert(0.0) += total_weight * 0.2;
            }
        }
    }

    // Score Modified Allan noise transitions (best for frequency drift)
    // Modified Allan handles frequency drift better than standard Allan
    for transition in &modified.noise_transitions {
        let base_weight = 3.2 * transition.confidence; // Between Allan and Hadamard

        let deviation_weight = if transition.deviation_change_factor > 3.0
            || transition.deviation_change_factor < 0.33
        {
            0.9
        } else if transition.deviation_change_factor > 2.0
            || transition.deviation_change_factor < 0.5
        {
            0.45
        } else {
            0.0
        };

        let total_weight = base_weight + deviation_weight;
        *anomaly_scores.entry(transition.index).or_insert(0.0) += total_weight;

        // Mark surrounding region
        for offset in 1..=10 {
            if transition.index >= offset {
                *anomaly_scores
                    .entry(transition.index - offset)
                    .or_insert(0.0) += total_weight * 0.2;
            }
            if transition.index + offset < values.len() {
                *anomaly_scores
                    .entry(transition.index + offset)
                    .or_insert(0.0) += total_weight * 0.2;
            }
        }
    }

    // Allan/Hadamard/Modified Allan deviations are used to:
    // 1. Determine optimal smoothing window (applied to analysis_values)
    // 2. Provide significance testing to CUSUM (allan_window and allan_significance)
    // 3. Adapt MAD thresholds based on noise characteristics
    // 4. Detect fundamental changes in system dynamics (noise transitions from all three methods)

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
        // Stricter thresholds to reduce false positives
        let method_confidence = if score >= 3.0 {
            0.95 // Very strong evidence (cliff + shift + MAD)
        } else if score >= 2.5 {
            0.9 // Strong evidence (cliff + shift or MAD)
        } else if score >= 2.0 {
            0.85 // Multiple methods agree
        } else if score >= 1.5 {
            0.7 // One strong method + partial agreement
        } else if score >= 1.0 {
            // Single method - confidence depends on deviation
            if deviation_factor > 10.0 {
                0.8 // Very extreme deviation
            } else if deviation_factor > 7.0 {
                0.7 // Extreme deviation
            } else if deviation_factor > 5.0 {
                0.6 // Significant deviation
            } else {
                0.4 // Moderate deviation - likely not anomalous
            }
        } else {
            0.3 // Weak detection - likely false positive
        };

        // Only include anomalies with high confidence
        // Require stronger evidence to reduce false positive rate
        if method_confidence >= 0.7 || (deviation_factor > 7.0 && score >= 1.5) {
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
            // Stricter criteria for higher severity levels
            let severity = if method_confidence >= 0.95 && deviation_factor > 10.0 {
                AnomalySeverity::Critical
            } else if method_confidence >= 0.9
                || (method_confidence >= 0.85 && deviation_factor > 7.0)
            {
                AnomalySeverity::High
            } else if method_confidence >= 0.8 || deviation_factor > 7.0 {
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

    output.push_str("MAD (Median Absolute Deviation) Analysis\n");
    output.push_str("-----------------------------------------\n");
    output.push_str(&format!("Median: {:.4}\n", result.mad_analysis.median));
    output.push_str(&format!("MAD: {:.4}\n", result.mad_analysis.mad));
    output.push_str(&format!(
        "Threshold ({:.1}σ, Allan-adapted): {:.4}\n",
        result.mad_analysis.threshold_sigma, result.mad_analysis.threshold
    ));
    output.push_str(&format!(
        "Outliers Found: {} ({:.2}% of data)\n",
        result.mad_analysis.outlier_count,
        (result.mad_analysis.outlier_count as f64 / result.total_points as f64) * 100.0
    ));

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
    output.push_str(&format!(
        "Sustained Regime Shifts: {}\n",
        result.cusum_analysis.window_change_points.len()
    ));

    if let Some(window) = result.smoothing_window {
        output.push_str(&format!(
            "Change-point window (Allan-based): {:.1}s\n",
            window * 2.0 // 2x the smoothing window
        ));
    }

    // Show window-based regime shifts (most important for detecting experiments)
    if !result.cusum_analysis.window_change_points.is_empty() && !result.timestamps.is_empty() {
        output.push_str("\n  SUSTAINED REGIME SHIFTS (experimental changes):\n");
        for (i, wcp) in result
            .cusum_analysis
            .window_change_points
            .iter()
            .enumerate()
            .take(5)
        {
            if wcp.index < result.timestamps.len() {
                let direction = if wcp.after_mean > wcp.before_mean {
                    "INCREASE"
                } else {
                    "DECREASE"
                };
                output.push_str(&format!(
                    "    {}. {} - {} change of {:.1}%\n       Before: {:.4}, After: {:.4}\n       Allan Significance: {:.1}x expected variance, Confidence: {:.1}%\n",
                    i + 1,
                    format_timestamp(result.timestamps[wcp.index]),
                    direction,
                    wcp.mean_change_pct * 100.0,
                    wcp.before_mean,
                    wcp.after_mean,
                    wcp.allan_significance,
                    wcp.confidence * 100.0
                ));
            }
        }
    }

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

    output.push_str("Allan Deviation Analysis\n");
    output.push_str("------------------------\n");
    output.push_str(&format!(
        "Noise Type: {:?}\n",
        result.allan_analysis.noise_type
    ));

    if let Some(window) = result.smoothing_window {
        output.push_str("\nData Smoothing Applied:\n");
        output.push_str(&format!(
            "  Allan-determined averaging window: {:.2}s\n",
            window
        ));
        output.push_str("  Purpose: Filter spike noise to detect regime shifts\n");
        output.push_str("  Anomaly detection performed on smoothed data\n");
    } else {
        output.push_str("\nNo smoothing applied (insufficient data or optimal window too small)\n");
    }

    if result.allan_analysis.has_cyclic_pattern {
        output.push_str("\nCyclic Patterns Detected:\n");
        for (i, minima) in result.allan_analysis.minima.iter().enumerate().take(3) {
            output.push_str(&format!(
                "  {}. Period: {:.2}s (confidence: {:.2}%)\n",
                i + 1,
                minima.tau_seconds,
                minima.confidence * 100.0
            ));
        }
    } else {
        output.push_str("\nNo significant cyclic patterns detected\n");
    }

    if !result.allan_analysis.noise_transitions.is_empty() {
        output.push_str("\nNoise Characteristic Transitions Detected:\n");
        output.push_str("  (Fundamental changes in system dynamics)\n");
        for (i, transition) in result
            .allan_analysis
            .noise_transitions
            .iter()
            .enumerate()
            .take(5)
        {
            if let Some(timestamp) = result.timestamps.get(transition.index) {
                let change_direction = if transition.deviation_change_factor > 1.0 {
                    format!("increased {:.1}x", transition.deviation_change_factor)
                } else {
                    format!("decreased {:.1}x", 1.0 / transition.deviation_change_factor)
                };

                output.push_str(&format!(
                    "  {}. {} - {:?} → {:?}\n     Allan deviation {} (confidence: {:.1}%)\n",
                    i + 1,
                    format_timestamp(*timestamp),
                    transition.from_noise_type,
                    transition.to_noise_type,
                    change_direction,
                    transition.confidence * 100.0
                ));
            }
        }
        if result.allan_analysis.noise_transitions.len() > 5 {
            output.push_str(&format!(
                "  ... and {} more transitions\n",
                result.allan_analysis.noise_transitions.len() - 5
            ));
        }
    }

    output.push('\n');

    output.push_str("Hadamard Deviation Analysis\n");
    output.push_str("---------------------------\n");
    output.push_str(&format!(
        "Noise Type: {:?}\n",
        result.hadamard_analysis.noise_type
    ));

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

    if !result.hadamard_analysis.noise_transitions.is_empty() {
        output.push_str("\nNoise Transitions (Hadamard view):\n");
        for (i, transition) in result
            .hadamard_analysis
            .noise_transitions
            .iter()
            .enumerate()
            .take(5)
        {
            if let Some(timestamp) = result.timestamps.get(transition.index) {
                let change_direction = if transition.deviation_change_factor > 1.0 {
                    format!("increased {:.1}x", transition.deviation_change_factor)
                } else {
                    format!("decreased {:.1}x", 1.0 / transition.deviation_change_factor)
                };

                output.push_str(&format!(
                    "  {}. {} - {:?} → {:?}\n     HDEV {} (conf: {:.0}%)\n",
                    i + 1,
                    format_timestamp(*timestamp),
                    transition.from_noise_type,
                    transition.to_noise_type,
                    change_direction,
                    transition.confidence * 100.0
                ));
            }
        }
        if result.hadamard_analysis.noise_transitions.len() > 5 {
            output.push_str(&format!(
                "  ... and {} more transitions\n",
                result.hadamard_analysis.noise_transitions.len() - 5
            ));
        }
    }

    output.push('\n');

    output.push_str("Modified Allan Deviation Analysis\n");
    output.push_str("---------------------------------\n");
    output.push_str(&format!(
        "Noise Type: {:?}\n",
        result.modified_allan_analysis.noise_type
    ));

    if !result.modified_allan_analysis.minima.is_empty() {
        output.push_str("Detected Minima (better frequency drift handling):\n");
        for (i, minima) in result
            .modified_allan_analysis
            .minima
            .iter()
            .enumerate()
            .take(3)
        {
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

    if !result.modified_allan_analysis.noise_transitions.is_empty() {
        output.push_str("\nNoise Transitions (Modified Allan view):\n");
        for (i, transition) in result
            .modified_allan_analysis
            .noise_transitions
            .iter()
            .enumerate()
            .take(5)
        {
            if let Some(timestamp) = result.timestamps.get(transition.index) {
                let change_direction = if transition.deviation_change_factor > 1.0 {
                    format!("increased {:.1}x", transition.deviation_change_factor)
                } else {
                    format!("decreased {:.1}x", 1.0 / transition.deviation_change_factor)
                };

                output.push_str(&format!(
                    "  {}. {} - {:?} → {:?}\n     MDEV {} (conf: {:.0}%)\n",
                    i + 1,
                    format_timestamp(*timestamp),
                    transition.from_noise_type,
                    transition.to_noise_type,
                    change_direction,
                    transition.confidence * 100.0
                ));
            }
        }
        if result.modified_allan_analysis.noise_transitions.len() > 5 {
            output.push_str(&format!(
                "  ... and {} more transitions\n",
                result.modified_allan_analysis.noise_transitions.len() - 5
            ));
        }
    }

    output.push('\n');

    if !result.anomalies.is_empty() {
        output.push_str("Detected Anomalies (Confidence ≥ 70%)\n");
        output.push_str("--------------------------------------\n");

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
