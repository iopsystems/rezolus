use serde::{Deserialize, Serialize};

/// MAD (Median Absolute Deviation) analysis results
#[derive(Debug, Serialize, Deserialize)]
pub struct MadAnalysis {
    pub median: f64,
    pub mad: f64,
    pub threshold: f64,
    pub threshold_sigma: f64, // The sigma multiplier used (Allan-adapted)
    pub outliers: Vec<usize>,
    pub outlier_count: usize,
}

pub(super) fn perform_mad_analysis(
    values: &[f64],
    threshold_multiplier: f64,
) -> Result<MadAnalysis, Box<dyn std::error::Error>> {
    if values.is_empty() {
        return Err("Cannot perform MAD analysis on empty dataset".into());
    }

    // Calculate median
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = if sorted.len().is_multiple_of(2) {
        (sorted[sorted.len() / 2 - 1] + sorted[sorted.len() / 2]) / 2.0
    } else {
        sorted[sorted.len() / 2]
    };

    // Calculate absolute deviations from median
    let deviations: Vec<f64> = values.iter().map(|v| (v - median).abs()).collect();

    // Calculate MAD (median of absolute deviations)
    let mut sorted_deviations = deviations.clone();
    sorted_deviations.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mad = if sorted_deviations.len().is_multiple_of(2) {
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
        threshold_sigma: threshold_multiplier,
        outlier_count: outliers.len(),
        outliers,
    })
}
