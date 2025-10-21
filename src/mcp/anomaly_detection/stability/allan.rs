use super::common::{find_deviation_minima, identify_noise_type};
use super::common::{CycleMinima, NoiseType};
use allan::Allan;
use serde::{Deserialize, Serialize};

/// Allan Deviation analysis results
#[derive(Debug, Serialize, Deserialize)]
pub struct AllanAnalysis {
    pub taus: Vec<f64>,
    pub deviations: Vec<f64>,
    pub noise_type: NoiseType,
    pub minima: Vec<CycleMinima>,
    pub has_cyclic_pattern: bool,
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
