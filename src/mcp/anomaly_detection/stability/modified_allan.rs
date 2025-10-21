use super::common::{find_deviation_minima, identify_noise_type};
use super::common::{CycleMinima, NoiseType};
use allan::ModifiedAllan;
use serde::{Deserialize, Serialize};

/// Modified Allan Deviation analysis results
#[derive(Debug, Serialize, Deserialize)]
pub struct ModifiedAllanAnalysis {
    pub taus: Vec<f64>,
    pub deviations: Vec<f64>,
    pub noise_type: NoiseType,
    pub minima: Vec<CycleMinima>,
}

pub(in crate::mcp::anomaly_detection) fn perform_modified_allan_analysis(
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
