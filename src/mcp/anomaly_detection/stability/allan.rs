use super::common::{find_deviation_minima, identify_noise_type};
use super::common::{CycleMinima, NoiseType};
use allan::Allan;
use serde::{Deserialize, Serialize};

/// Detected noise characteristic transition
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct NoiseTransition {
    pub index: usize,
    pub timestamp_offset: f64,
    pub from_noise_type: NoiseType,
    pub to_noise_type: NoiseType,
    pub deviation_change_factor: f64, // How much the Allan deviation changed
    pub confidence: f64,
}

/// Allan Deviation analysis results
#[derive(Debug, Serialize, Deserialize)]
pub struct AllanAnalysis {
    pub taus: Vec<f64>,
    pub deviations: Vec<f64>,
    pub noise_type: NoiseType,
    pub minima: Vec<CycleMinima>,
    pub has_cyclic_pattern: bool,
    pub noise_transitions: Vec<NoiseTransition>, // Detected changes in noise characteristics
}

pub(in crate::mcp::anomaly_detection) fn perform_allan_analysis(
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
            noise_transitions: vec![],
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

    // Detect noise characteristic transitions via sliding window analysis
    let noise_transitions = detect_noise_transitions(values, sample_interval)?;

    Ok(AllanAnalysis {
        taus: taus_seconds,
        deviations,
        noise_type,
        minima,
        has_cyclic_pattern,
        noise_transitions,
    })
}

/// Detect changes in noise characteristics using sliding window Allan analysis
fn detect_noise_transitions(
    values: &[f64],
    sample_interval: f64,
) -> Result<Vec<NoiseTransition>, Box<dyn std::error::Error>> {
    let mut transitions = Vec::new();

    // Need sufficient data for meaningful windows
    let min_window_size = 50;
    if values.len() < min_window_size * 2 {
        return Ok(transitions);
    }

    // Window size: ~25% of data, minimum 50 points
    let window_size = (values.len() / 4).max(min_window_size);
    // Step size: 50% overlap for better transition detection
    let step_size = window_size / 2;

    let mut prev_noise_type: Option<NoiseType> = None;
    let mut prev_deviation: Option<f64> = None;

    // Slide window through the data
    let mut start = 0;
    while start + window_size <= values.len() {
        let window = &values[start..start + window_size];

        // Compute Allan deviation for this window
        let mut allan = Allan::new();
        for &value in window {
            allan.record(value);
        }

        // Use a characteristic tau (around 10% of window length)
        let tau_samples = (window_size / 10).max(2);
        let tau_seconds = tau_samples as f64 * sample_interval;

        if let Some(tau_result) = allan.get(tau_samples) {
            if let Some(current_deviation) = tau_result.deviation() {
                // Generate minimal tau/deviation arrays for noise type identification
                let taus = vec![tau_seconds];
                let devs = vec![current_deviation];
                let current_noise_type = identify_noise_type(&taus, &devs);

                // Check for transition
                if let (Some(prev_type), Some(prev_dev)) = (prev_noise_type, prev_deviation) {
                    let noise_type_changed = !matches!(
                        (&prev_type, &current_noise_type),
                        (NoiseType::Unknown, NoiseType::Unknown)
                    ) && std::mem::discriminant(&prev_type)
                        != std::mem::discriminant(&current_noise_type);

                    let deviation_change_factor = if prev_dev > 0.0 {
                        current_deviation / prev_dev
                    } else {
                        1.0
                    };

                    // Detect significant change: noise type changed OR deviation changed by 2x
                    let significant_deviation_change =
                        deviation_change_factor > 2.0 || deviation_change_factor < 0.5;

                    if noise_type_changed || significant_deviation_change {
                        // Transition point is at the start of the current window
                        let transition_index = start;
                        let timestamp_offset = transition_index as f64 * sample_interval;

                        // Calculate confidence based on how dramatic the change is
                        let mut confidence: f64 = 0.5; // Base confidence
                        if noise_type_changed {
                            confidence += 0.3; // Noise type change is significant
                        }
                        if deviation_change_factor > 3.0 || deviation_change_factor < 0.33 {
                            confidence += 0.3; // Very large deviation change
                        } else if significant_deviation_change {
                            confidence += 0.2; // Moderate deviation change
                        }
                        confidence = confidence.min(1.0);

                        transitions.push(NoiseTransition {
                            index: transition_index,
                            timestamp_offset,
                            from_noise_type: prev_type,
                            to_noise_type: current_noise_type.clone(),
                            deviation_change_factor,
                            confidence,
                        });
                    }
                }

                prev_noise_type = Some(current_noise_type);
                prev_deviation = Some(current_deviation);
            }
        }

        start += step_size;
    }

    Ok(transitions)
}
