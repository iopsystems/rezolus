use serde::{Deserialize, Serialize};

/// Detected cycle/period from deviation minima
#[derive(Debug, Serialize, Deserialize)]
pub struct CycleMinima {
    pub tau_seconds: f64,
    pub deviation: f64,
    pub confidence: f64,
}

/// Noise type identified from Allan/Hadamard slope
#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum NoiseType {
    WhitePhase,       // slope = -1
    FlickerPhase,     // slope = -1/2
    WhiteFrequency,   // slope = -1/2
    FlickerFrequency, // slope = 0
    RandomWalk,       // slope = +1/2
    FlickerWalk,      // slope = +1
    Unknown,
}

/// Identify noise type from slope in log-log space
pub(super) fn identify_noise_type(taus: &[f64], deviations: &[f64]) -> NoiseType {
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
pub(super) fn find_deviation_minima(taus: &[f64], deviations: &[f64]) -> Vec<CycleMinima> {
    let mut minima = Vec::new();

    if taus.len() < 3 {
        return minima;
    }

    // Find local minima
    for i in 1..taus.len() - 1 {
        if deviations[i] < deviations[i - 1] && deviations[i] < deviations[i + 1] {
            // Calculate confidence based on how pronounced the minimum is
            let depth =
                ((deviations[i - 1] - deviations[i]) + (deviations[i + 1] - deviations[i])) / 2.0;
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
    minima.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    minima
}
