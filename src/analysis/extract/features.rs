//! Per-metric feature computation: summary stats and mappings from the
//! anomaly-engine result types down to the lean record features.

use crate::analysis::record::{
    AnomalyFeature, NoiseSummary, RegimeShiftFeature, Stats, UncertaintySummary,
};
use crate::mcp::anomaly_detection::{AllanAnalysis, WindowChangePoint};

/// Replace non-finite values with 0.0 — the record's finite-float invariant
/// (NaN/Inf serialize to JSON null and can never be read back).
pub(crate) fn finite_or_zero(v: f64) -> f64 {
    if v.is_finite() {
        v
    } else {
        0.0
    }
}

/// Nearest-rank percentile over a sorted, non-empty slice.
pub(crate) fn percentile(sorted: &[f64], q: f64) -> f64 {
    debug_assert!(!sorted.is_empty(), "percentile over empty slice");
    let n = sorted.len();
    let rank = ((q * n as f64).ceil() as usize).clamp(1, n);
    sorted[rank - 1]
}

/// Summary stats over the finite subset of `values`. An empty (or all
/// non-finite) series yields all-zero stats — the metric still appears in
/// the record (exhaustive coverage), it just carries no signal.
pub(crate) fn compute_stats(values: &[f64]) -> Stats {
    let finite: Vec<f64> = values.iter().copied().filter(|v| v.is_finite()).collect();
    if finite.is_empty() {
        return Stats {
            min: 0.0,
            max: 0.0,
            mean: 0.0,
            last: 0.0,
            p50: 0.0,
            p99: 0.0,
        };
    }
    let mut sorted = finite.clone();
    sorted.sort_by(|a, b| a.total_cmp(b));
    Stats {
        min: sorted[0],
        max: sorted[sorted.len() - 1],
        mean: finite.iter().sum::<f64>() / finite.len() as f64,
        last: finite[finite.len() - 1],
        p50: percentile(&sorted, 0.50),
        p99: percentile(&sorted, 0.99),
    }
}

/// Map the Allan analysis down to the record's noise summary.
/// `optimal_tau_s` is the highest-confidence deviation minimum (the engine
/// sorts `minima` by confidence descending).
pub(crate) fn noise_summary(allan: &AllanAnalysis) -> NoiseSummary {
    NoiseSummary {
        noise_type: format!("{:?}", allan.noise_type),
        optimal_tau_s: allan
            .minima
            .first()
            .map(|m| m.tau_seconds)
            .filter(|t| t.is_finite() && *t > 0.0),
    }
}

/// Map window change points to record regime shifts: direction derived from
/// the means (the engine type has no direction field), fraction -> percent
/// (`0.75` -> `75.0`), sorted by index (the engine emits confidence-desc).
pub(crate) fn regime_shift_features(points: &[WindowChangePoint]) -> Vec<RegimeShiftFeature> {
    let mut shifts: Vec<RegimeShiftFeature> = points
        .iter()
        .map(|w| RegimeShiftFeature {
            index: w.index,
            direction: if w.after_mean > w.before_mean {
                "Increase".to_string()
            } else {
                "Decrease".to_string()
            },
            before_mean: finite_or_zero(w.before_mean),
            after_mean: finite_or_zero(w.after_mean),
            mean_change_pct: finite_or_zero(w.mean_change_pct * 100.0),
            confidence: finite_or_zero(w.confidence),
            // NaN when the engine's before_mean == 0 (0/0) — guard per the
            // record's finite-float invariant.
            allan_significance: finite_or_zero(w.allan_significance),
        })
        .collect();
    shifts.sort_by_key(|s| s.index);
    shifts
}

/// Build one record anomaly. `magnitude` is computed here — the engine's
/// `Anomaly` carries the raw analysis value, not a magnitude:
/// robust sigmas: `(value - median).abs() / (mad * 1.4826)`, 0.0 when `mad == 0` (constant-ish series).
#[allow(clippy::too_many_arguments)]
pub(crate) fn anomaly_feature_from(
    timestamp: f64,
    value: f64,
    index: usize,
    anomaly_type: &str,
    severity: &str,
    confidence: f64,
    mad_median: f64,
    mad: f64,
) -> AnomalyFeature {
    // Robust sigmas: MAD × 1.4826 approximates one standard deviation for
    // Gaussian data, matching the engine's own mad_std convention.
    let magnitude = if mad > 0.0 {
        (value - mad_median).abs() / (mad * 1.4826)
    } else {
        0.0
    };
    AnomalyFeature {
        timestamp: finite_or_zero(timestamp),
        index,
        anomaly_type: anomaly_type.to_string(),
        severity: severity.to_string(),
        confidence: finite_or_zero(confidence),
        magnitude: finite_or_zero(magnitude),
    }
}

/// Acquisition-window uncertainty: mean band width vs. observed movement.
/// `within_band` is true when the whole observed movement fits inside the
/// mean measurement band. For a flat signal the ratio is pinned to 1.0.
/// `None` when the query result carried no intervals (gauges, histograms,
/// or aggregation that strips them).
pub(crate) fn uncertainty_summary(
    values: &[f64],
    intervals: Option<&[(f64, f64)]>,
) -> Option<UncertaintySummary> {
    let intervals = intervals?;
    if intervals.is_empty() || values.is_empty() {
        return None;
    }
    let widths: Vec<f64> = intervals
        .iter()
        .map(|(lo, hi)| hi - lo)
        .filter(|w| w.is_finite() && *w >= 0.0)
        .collect();
    if widths.is_empty() {
        return None;
    }
    let band = widths.iter().sum::<f64>() / widths.len() as f64;
    let finite: Vec<f64> = values.iter().copied().filter(|v| v.is_finite()).collect();
    if finite.is_empty() {
        return None;
    }
    let mut min = finite[0];
    let mut max = finite[0];
    for v in &finite {
        min = min.min(*v);
        max = max.max(*v);
    }
    let movement = max - min;
    let ratio = if movement > 0.0 { band / movement } else { 1.0 };
    Some(UncertaintySummary {
        band_to_signal_ratio: finite_or_zero(ratio),
        within_band: movement <= band,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_computed_from_values() {
        let vals = [3.0, 1.0, 2.0, 4.0];
        let s = compute_stats(&vals);
        assert_eq!(s.min, 1.0);
        assert_eq!(s.max, 4.0);
        assert_eq!(s.mean, 2.5);
        assert_eq!(s.last, 4.0);
        assert_eq!(s.p50, 2.0);
        assert_eq!(s.p99, 4.0);
    }

    #[test]
    fn stats_ignore_non_finite_and_empty_is_zero() {
        let vals = [f64::NAN, 5.0, f64::INFINITY];
        let s = compute_stats(&vals);
        assert_eq!(s.min, 5.0);
        assert_eq!(s.max, 5.0);
        let z = compute_stats(&[]);
        assert_eq!(z.min, 0.0);
        assert_eq!(z.max, 0.0);
        assert_eq!(z.mean, 0.0);
    }

    #[test]
    fn percentile_nearest_rank() {
        let sorted = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0];
        assert_eq!(percentile(&sorted, 0.50), 5.0);
        assert_eq!(percentile(&sorted, 0.99), 10.0);
        assert_eq!(percentile(&sorted, 0.10), 1.0);
    }

    #[test]
    fn finite_or_zero_guards() {
        assert_eq!(finite_or_zero(1.5), 1.5);
        assert_eq!(finite_or_zero(f64::NAN), 0.0);
        assert_eq!(finite_or_zero(f64::NEG_INFINITY), 0.0);
    }

    use crate::mcp::anomaly_detection::WindowChangePoint;

    fn wcp(
        before: f64,
        after: f64,
        pct_fraction: f64,
        sig: f64,
        index: usize,
    ) -> WindowChangePoint {
        WindowChangePoint {
            index,
            before_mean: before,
            after_mean: after,
            mean_change_pct: pct_fraction,
            confidence: 0.9,
            allan_significance: sig,
        }
    }

    #[test]
    fn regime_shift_maps_percent_direction_and_sorts_by_index() {
        let shifts =
            regime_shift_features(&[wcp(0.4, 0.7, 0.75, 3.2, 40), wcp(0.7, 0.5, 0.28, 2.5, 10)]);
        assert_eq!(shifts.len(), 2);
        // sorted by index, not input (confidence) order
        assert_eq!(shifts[0].index, 10);
        assert_eq!(shifts[0].direction, "Decrease");
        assert_eq!(shifts[1].index, 40);
        assert_eq!(shifts[1].direction, "Increase");
        // fraction -> percent
        assert!((shifts[1].mean_change_pct - 75.0).abs() < 1e-9);
    }

    #[test]
    fn regime_shift_nan_significance_is_guarded() {
        // before_mean == 0 WOULD make the engine's allan_significance NaN (0/0); the engine's own gates make this unreachable, so this guard is defensive.
        let shifts = regime_shift_features(&[wcp(0.0, 0.5, f64::NAN, f64::NAN, 5)]);
        assert_eq!(shifts[0].allan_significance, 0.0);
        assert_eq!(shifts[0].mean_change_pct, 0.0);
    }

    #[test]
    fn anomaly_magnitude_computed_from_mad() {
        let f = anomaly_feature_from(42.0, 8.0, 3, "PointOutlier", "High", 0.9, 5.0, 1.0);
        // (8 - 5).abs() / (1.0 * 1.4826) = 3.0 / 1.4826
        assert!((f.magnitude - 3.0 / 1.4826).abs() < 1e-9);
        assert_eq!(f.anomaly_type, "PointOutlier");
        assert_eq!(f.severity, "High");
        assert_eq!(f.index, 3);
    }

    #[test]
    fn anomaly_magnitude_zero_when_mad_zero() {
        let f = anomaly_feature_from(42.0, 8.0, 3, "LevelShift", "Low", 0.6, 5.0, 0.0);
        assert_eq!(f.magnitude, 0.0);
    }

    #[test]
    fn uncertainty_from_intervals() {
        // signal moves 0..10, mean band width 1.0 -> ratio 0.1, not within band
        let values = [0.0, 5.0, 10.0];
        let intervals = [(4.5, 5.5), (4.5, 5.5), (4.5, 5.5)];
        let u = uncertainty_summary(&values, Some(&intervals[..])).expect("some");
        assert!((u.band_to_signal_ratio - 0.1).abs() < 1e-9);
        assert!(!u.within_band);
        // constant signal: movement 0 <= band -> within_band, ratio pinned to 1.0
        let c = uncertainty_summary(&[5.0, 5.0], Some(&intervals[..2])).expect("some");
        assert!(c.within_band);
        assert_eq!(c.band_to_signal_ratio, 1.0);
        // no intervals -> None
        assert!(uncertainty_summary(&values, None).is_none());
        assert!(uncertainty_summary(&values, Some(&[])).is_none());
    }

    #[test]
    fn percentile_boundaries() {
        let one = [7.0];
        assert_eq!(percentile(&one, 0.0), 7.0);
        assert_eq!(percentile(&one, 1.0), 7.0);
        let two = [1.0, 2.0];
        assert_eq!(percentile(&two, 0.0), 1.0);
        assert_eq!(percentile(&two, 1.0), 2.0);
    }

    #[test]
    fn uncertainty_filters_degenerate_inputs() {
        // all-non-finite values -> None
        assert!(uncertainty_summary(&[f64::NAN], Some(&[(0.0, 1.0)])).is_none());
        // all-invalid widths (hi < lo, NaN) -> None
        assert!(uncertainty_summary(&[1.0, 2.0], Some(&[(2.0, 1.0), (f64::NAN, 0.0)])).is_none());
    }

    #[test]
    fn regime_shift_equal_means_is_decrease() {
        let shifts = regime_shift_features(&[wcp(0.5, 0.5, 0.0, 1.0, 7)]);
        assert_eq!(shifts[0].direction, "Decrease");
    }
}
