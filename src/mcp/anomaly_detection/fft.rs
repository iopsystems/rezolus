use rustfft::{num_complex::Complex, FftPlanner};
use serde::{Deserialize, Serialize};

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

pub(super) fn perform_fft_analysis(
    values: &[f64],
    sample_interval: f64,
) -> Result<FftAnalysis, Box<dyn std::error::Error>> {
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
