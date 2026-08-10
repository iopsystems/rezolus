//! Per-metric feature computation: summary stats and mappings from the
//! anomaly-engine result types down to the lean record features.

use crate::analysis::record::Stats;

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
}
