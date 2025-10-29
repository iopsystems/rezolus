use super::stability::AllanAnalysis;
use serde::{Deserialize, Serialize};

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
    pub window_change_points: Vec<WindowChangePoint>,
}

/// Detected cliff (dramatic change)
#[derive(Debug, Serialize, Deserialize)]
pub struct CliffPoint {
    pub index: usize,
    pub magnitude: f64,
    pub direction: ChangeDirection,
}

/// Window-based change point (sustained regime shift)
#[derive(Debug, Serialize, Deserialize)]
pub struct WindowChangePoint {
    pub index: usize,
    pub before_mean: f64,
    pub after_mean: f64,
    pub mean_change_pct: f64,
    pub confidence: f64,
    pub allan_significance: f64, // How many times larger than expected variance
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

pub(super) fn perform_cusum_analysis_with_allan(
    values: &[f64],
    sample_interval: f64,
    allan_window: f64,
    allan_analysis: &AllanAnalysis,
) -> Result<CusumAnalysis, Box<dyn std::error::Error>> {
    if values.is_empty() {
        return Err("Cannot perform CUSUM analysis on empty dataset".into());
    }

    // Calculate mean and standard deviation
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    let variance = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / values.len() as f64;
    let std_dev = variance.sqrt();

    // Use Allan-determined window for change-point detection with Allan-based significance
    let window_change_points =
        detect_window_change_points(values, sample_interval, allan_window, allan_analysis);

    // Detect cliffs using simple differencing
    let cliffs = detect_cliffs(values, mean, std_dev);

    // Run multi-scale CUSUM with different sensitivities
    // Adjusted thresholds to reduce false positives
    let sensitivity_configs = vec![
        ("High Sensitivity", 0.5, 5.0), // Detect small changes (was 0.25, 2.0)
        ("Medium Sensitivity", 1.0, 6.0), // Standard detection (was 0.5, 4.0)
        ("Low Sensitivity", 1.5, 8.0),  // Only major changes (was 1.0, 6.0)
        ("Cliff Detection", 2.5, 10.0), // Dramatic changes (was 2.0, 8.0)
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

    // Run standard CUSUM for compatibility with increased thresholds
    let k = 1.0 * std_dev; // Increased from 0.5
    let h = 6.0 * std_dev; // Increased from 4.0
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
        window_change_points,
    })
}

/// Interpolate Allan deviation at a specific tau
fn interpolate_allan_dev(allan_analysis: &AllanAnalysis, target_tau: f64) -> f64 {
    if allan_analysis.taus.is_empty() {
        return 0.0;
    }

    // Find the last non-zero deviation (workaround for zeros at large taus)
    let last_nonzero_idx = allan_analysis
        .deviations
        .iter()
        .enumerate()
        .rev()
        .find(|(_, &dev)| dev > 0.0)
        .map(|(i, _)| i);

    // Find the two points that bracket target_tau
    let mut below_idx = None;
    let mut above_idx = None;

    for (i, &tau) in allan_analysis.taus.iter().enumerate() {
        // Skip zero deviations
        if allan_analysis.deviations[i] <= 0.0 {
            continue;
        }

        if tau <= target_tau {
            below_idx = Some(i);
        }
        if tau >= target_tau && above_idx.is_none() {
            above_idx = Some(i);
            break;
        }
    }

    match (below_idx, above_idx) {
        (Some(b_idx), Some(a_idx)) if b_idx != a_idx => {
            // Linear interpolation in log-log space
            let tau_b = allan_analysis.taus[b_idx];
            let tau_a = allan_analysis.taus[a_idx];
            let dev_b = allan_analysis.deviations[b_idx];
            let dev_a = allan_analysis.deviations[a_idx];

            // Check for invalid values before logging
            if tau_b <= 0.0 || tau_a <= 0.0 || dev_b <= 0.0 || dev_a <= 0.0 || target_tau <= 0.0 {
                // Fallback to linear interpolation if we can't use log-log
                let t = (target_tau - tau_b) / (tau_a - tau_b);
                dev_b + t * (dev_a - dev_b)
            } else {
                let log_tau = target_tau.ln();
                let log_tau_b = tau_b.ln();
                let log_tau_a = tau_a.ln();
                let log_dev_b = dev_b.ln();
                let log_dev_a = dev_a.ln();

                let t = (log_tau - log_tau_b) / (log_tau_a - log_tau_b);
                let log_dev = log_dev_b + t * (log_dev_a - log_dev_b);
                log_dev.exp()
            }
        }
        (Some(idx), None) => {
            // Extrapolation beyond the range - use the last valid deviation
            // (This handles the case where we're asking for tau beyond the data)
            allan_analysis.deviations[idx]
        }
        (None, Some(idx)) => allan_analysis.deviations[idx],
        (Some(idx), Some(_)) => allan_analysis.deviations[idx], // Same tau
        (None, None) => {
            // No valid (non-zero) data points found
            // Use the last non-zero deviation if available
            last_nonzero_idx
                .map(|i| allan_analysis.deviations[i])
                .unwrap_or(0.0)
        }
    }
}

/// Detect sustained regime shifts using window-based comparison
/// This detects changes in the baseline that persist over time, not just point spikes
fn detect_window_change_points(
    values: &[f64],
    sample_interval: f64,
    allan_window_seconds: f64,
    allan_analysis: &AllanAnalysis,
) -> Vec<WindowChangePoint> {
    let mut change_points = Vec::new();

    if values.len() < 60 {
        // Need at least 60 samples for meaningful windows
        return change_points;
    }

    // Use Allan-determined window size for change-point detection
    // This adapts to the noise characteristics of the metric
    // For regime shift detection, use 2x the Allan window to ensure we capture sustained changes
    let window_duration = (allan_window_seconds * 2.0).max(30.0).min(120.0);
    let window_size = (window_duration / sample_interval).round() as usize;
    let window_size = window_size.max(10).min(values.len() / 4);

    // Get the Allan deviation at the window tau for significance testing
    let allan_dev_at_tau = interpolate_allan_dev(allan_analysis, window_duration);

    // Slide a window through the data, comparing before/after statistics
    let step_size = window_size / 2; // 50% overlap for better detection

    for i in (window_size..values.len()).step_by(step_size) {
        // Compare window before this point with window after
        let before_start = i.saturating_sub(window_size);
        let before_end = i;
        let after_start = i;
        let after_end = (i + window_size).min(values.len());

        if after_end - after_start < window_size / 2 {
            // Not enough data after this point
            break;
        }

        let before_window = &values[before_start..before_end];
        let after_window = &values[after_start..after_end];

        // Calculate means
        let before_mean = before_window.iter().sum::<f64>() / before_window.len() as f64;
        let after_mean = after_window.iter().sum::<f64>() / after_window.len() as f64;

        // Calculate standard deviations
        let before_var = before_window
            .iter()
            .map(|v| (v - before_mean).powi(2))
            .sum::<f64>()
            / before_window.len() as f64;
        let _before_std = before_var.sqrt();

        let after_var = after_window
            .iter()
            .map(|v| (v - after_mean).powi(2))
            .sum::<f64>()
            / after_window.len() as f64;
        let _after_std = after_var.sqrt();

        // Calculate mean change percentage
        let mean_change = (after_mean - before_mean).abs();
        let mean_change_pct = if before_mean.abs() > 0.0 {
            mean_change / before_mean.abs()
        } else {
            0.0
        };

        // Detect significant change using pooled standard error
        let pooled_var = (before_var + after_var) / 2.0;
        let pooled_std = pooled_var.sqrt();
        let std_error = pooled_std
            * ((1.0 / before_window.len() as f64) + (1.0 / after_window.len() as f64)).sqrt();

        // t-statistic for difference of means
        let t_stat = if std_error > 0.0 {
            (after_mean - before_mean).abs() / std_error
        } else {
            0.0
        };

        // Calculate Allan-based significance: how many times larger is the shift vs expected variability?
        let allan_significance = if allan_dev_at_tau > 0.0 {
            mean_change * before_mean.abs() / (allan_dev_at_tau * before_mean.abs())
        } else {
            0.0
        };

        // Require:
        // 1. Statistically significant (t > 3.0)
        // 2. Shift larger than expected variability (allan_significance > 2.0)
        //    This means the shift is 2x larger than normal variance at this timescale
        // 3. Meaningful percentage change (> 10%)
        if t_stat > 3.0 && allan_significance > 2.0 && mean_change_pct > 0.10 {
            // This is a sustained regime shift that exceeds normal variability

            // Confidence based on:
            // - t-statistic (statistical significance)
            // - Allan significance (how abnormal vs expected variance)
            // - Percent change magnitude
            let t_component = (t_stat / 10.0).min(1.0);
            let allan_component = (allan_significance / 10.0).min(1.0);
            let pct_component = (mean_change_pct * 2.0).min(1.0);

            // Weight Allan significance highest, then t-stat, then percent change
            let confidence =
                (allan_component * 0.5 + t_component * 0.3 + pct_component * 0.2).min(1.0);

            // Check if we already detected a change point nearby
            let too_close = change_points
                .iter()
                .any(|cp: &WindowChangePoint| cp.index.abs_diff(i) < window_size);

            if !too_close && confidence > 0.4 {
                change_points.push(WindowChangePoint {
                    index: i,
                    before_mean,
                    after_mean,
                    mean_change_pct,
                    confidence,
                    allan_significance,
                });
            }
        }
    }

    // Sort by confidence (highest first)
    change_points.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    change_points
}

/// Detect dramatic cliffs in the data
fn detect_cliffs(values: &[f64], mean: f64, std_dev: f64) -> Vec<CliffPoint> {
    let mut cliffs = Vec::new();

    if values.len() < 2 {
        return cliffs;
    }

    // Calculate coefficient of variation to adapt thresholds
    let cv = if mean.abs() > 0.0 {
        std_dev / mean.abs()
    } else {
        0.0
    };

    // For high-variance metrics (CV > 0.4), use stricter thresholds to avoid noise
    // For low-variance metrics (CV < 0.2), use standard thresholds
    let (absolute_multiplier, relative_threshold) = if cv > 0.4 {
        // High variance - very strict (e.g., blockio, network)
        (5.0, 0.50) // 5 std devs, 50% change
    } else if cv > 0.25 {
        // Moderate variance - strict
        (4.0, 0.35) // 4 std devs, 35% change
    } else {
        // Low variance - standard (e.g., cpu cycles)
        (3.0, 0.25) // 3 std devs, 25% change
    };

    let absolute_threshold = absolute_multiplier * std_dev;

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

            // Require stronger evidence for window-based cliff detection
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
