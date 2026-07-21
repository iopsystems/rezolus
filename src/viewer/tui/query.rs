//! Bridges `PlotDef`s to `MetricsSource::query_range` and shapes results
//! into chart-ready series.

use metriken_query::{MetricsSource, QueryResult};

use super::model::{PlotDef, PlotKind};
use super::window::TimeWindow;

/// One line to draw: a label plus (timestamp_s, value) points.
#[derive(Clone, Debug, PartialEq)]
pub struct Series {
    pub label: String,
    pub points: Vec<(f64, f64)>,
}

/// Result of loading a plot for the current window.
#[derive(Clone, Debug, PartialEq)]
pub enum ChartData {
    Lines(Vec<Series>),
    /// A heatmap-only plot we do not render in v1.
    Unsupported,
    /// The query failed; message is shown inline.
    Error(String),
    /// No data in range.
    Empty,
}

/// Build the PromQL expression for a plot, applying the histogram
/// percentile wrapping rule (mirrors `metric_types.js::buildHistogramQuery`).
pub fn build_query(def: &PlotDef) -> Option<String> {
    match def.kind {
        PlotKind::Line => Some(def.base_query.clone()),
        PlotKind::Percentiles => {
            let qs = def
                .percentiles
                .iter()
                .map(|p| format!("{p}"))
                .collect::<Vec<_>>()
                .join(", ");
            Some(format!("histogram_quantiles([{qs}], {})", def.base_query))
        }
        PlotKind::HeatmapUnsupported => None,
    }
}

/// Turn a percentile quantile (e.g. 0.999) into a short label ("p99.9").
pub fn percentile_label(q: f64) -> String {
    let pct = q * 100.0;
    if (pct - pct.round()).abs() < 1e-9 {
        format!("p{}", pct.round() as i64)
    } else {
        // Trim trailing zeros: 99.9, 99.99
        let s = format!("{pct}");
        format!("p{s}")
    }
}

/// Load a plot's chart data for the given window against `data`.
pub fn load_chart(data: &dyn MetricsSource, def: &PlotDef, window: TimeWindow) -> ChartData {
    if def.kind == PlotKind::HeatmapUnsupported {
        return ChartData::Unsupported;
    }
    let Some(query) = build_query(def) else {
        return ChartData::Unsupported;
    };
    let Some((start, end, step)) = window.resolve(data.time_range(), data.interval()) else {
        return ChartData::Empty;
    };
    match data.query_range(&query, start, end, step) {
        Ok(QueryResult::Matrix { result }) => {
            let series: Vec<Series> = result
                .into_iter()
                .map(|s| Series {
                    label: series_label(&s.metric),
                    points: s.values,
                })
                .filter(|s| !s.points.is_empty())
                .collect();
            if series.is_empty() {
                ChartData::Empty
            } else {
                ChartData::Lines(series)
            }
        }
        Ok(_) => ChartData::Empty,
        Err(e) => ChartData::Error(e.to_string()),
    }
}

/// Choose a display label for a matrix series from its metric labels.
/// Prefers a `percentile`/`quantile` label, then any distinguishing label,
/// else empty.
fn series_label(metric: &std::collections::HashMap<String, String>) -> String {
    if let Some(q) = metric.get("percentile").or_else(|| metric.get("quantile")) {
        if let Ok(v) = q.parse::<f64>() {
            return percentile_label(if v > 1.0 { v / 100.0 } else { v });
        }
        return q.clone();
    }
    // Fall back to a single non-name label value if present.
    metric
        .iter()
        .find(|(k, _)| k.as_str() != "__name__")
        .map(|(_, v)| v.clone())
        .unwrap_or_default()
}

/// Downsample a time-ordered `(timestamp, value)` series to roughly `target`
/// buckets, keeping each bucket's **min and max** so short-lived spikes
/// survive — a post-eval min/max reducer (PromQL step decimation can't do
/// this: histograms ignore step, and averaging hides spikes). Each bucket
/// emits its extremes ordered by timestamp so a line renderer sweeps the
/// bucket's full vertical range. Input is assumed ascending by timestamp
/// (as `query_range` returns it); returned unchanged when already sparse
/// enough (≤ ~2 points per target column).
pub fn min_max_decimate(points: &[(f64, f64)], target: usize) -> Vec<(f64, f64)> {
    let target = target.max(1);
    if points.len() <= target.saturating_mul(2) {
        return points.to_vec();
    }
    let x0 = points.first().map(|p| p.0).unwrap_or(0.0);
    let x1 = points.last().map(|p| p.0).unwrap_or(x0);
    let span = (x1 - x0).max(f64::EPSILON);

    let mut out: Vec<(f64, f64)> = Vec::with_capacity(target * 2);
    // Extremes of the bucket currently being accumulated: (bucket index,
    // lowest-value point so far, highest-value point so far).
    type Bucket = (usize, (f64, f64), (f64, f64));
    let mut cur: Option<Bucket> = None;

    let flush = |out: &mut Vec<(f64, f64)>, lo: (f64, f64), hi: (f64, f64)| {
        // Emit in timestamp order so the swept segment reads naturally.
        let (a, b) = if lo.0 <= hi.0 { (lo, hi) } else { (hi, lo) };
        out.push(a);
        if b != a {
            out.push(b);
        }
    };

    for &(x, y) in points {
        let bucket = ((((x - x0) / span) * target as f64) as usize).min(target - 1);
        match cur {
            Some((b, lo, hi)) if b == bucket => {
                let lo = if y < lo.1 { (x, y) } else { lo };
                let hi = if y > hi.1 { (x, y) } else { hi };
                cur = Some((b, lo, hi));
            }
            Some((_, lo, hi)) => {
                flush(&mut out, lo, hi);
                cur = Some((bucket, (x, y), (x, y)));
            }
            None => cur = Some((bucket, (x, y), (x, y))),
        }
    }
    if let Some((_, lo, hi)) = cur {
        flush(&mut out, lo, hi);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::model::PlotKind;
    use super::*;

    fn def(kind: PlotKind, pcts: Vec<f64>) -> PlotDef {
        PlotDef {
            title: "t".into(),
            base_query: "m".into(),
            kind,
            percentiles: pcts,
            unit_system: None,
        }
    }

    #[test]
    fn line_query_is_verbatim() {
        assert_eq!(build_query(&def(PlotKind::Line, vec![])).unwrap(), "m");
    }

    #[test]
    fn percentile_query_wraps_histogram_quantiles() {
        let q = build_query(&def(PlotKind::Percentiles, vec![0.5, 0.99])).unwrap();
        assert_eq!(q, "histogram_quantiles([0.5, 0.99], m)");
    }

    #[test]
    fn heatmap_has_no_query() {
        assert!(build_query(&def(PlotKind::HeatmapUnsupported, vec![])).is_none());
    }

    #[test]
    fn percentile_labels_format() {
        assert_eq!(percentile_label(0.5), "p50");
        assert_eq!(percentile_label(0.99), "p99");
        assert_eq!(percentile_label(0.999), "p99.9");
    }

    #[test]
    fn decimate_returns_input_when_already_sparse() {
        let pts = vec![(0.0, 1.0), (1.0, 2.0), (2.0, 3.0)];
        assert_eq!(min_max_decimate(&pts, 10), pts);
    }

    #[test]
    fn decimate_bounds_output_size() {
        // 1000 points into ~10 buckets -> at most 2 points per bucket.
        let pts: Vec<(f64, f64)> = (0..1000).map(|i| (i as f64, (i % 7) as f64)).collect();
        let out = min_max_decimate(&pts, 10);
        assert!(out.len() < pts.len());
        assert!(out.len() <= 10 * 2, "len {}", out.len());
    }

    #[test]
    fn decimate_preserves_a_spike() {
        // A flat baseline with one tall spike buried in the middle. The
        // spike's value must survive downsampling (this is the whole point).
        let mut pts: Vec<(f64, f64)> = (0..1000).map(|i| (i as f64, 1.0)).collect();
        pts[500].1 = 999.0;
        let out = min_max_decimate(&pts, 8);
        let max_y = out.iter().map(|p| p.1).fold(f64::MIN, f64::max);
        assert_eq!(max_y, 999.0, "spike lost: {out:?}");
        // The baseline min is also preserved.
        let min_y = out.iter().map(|p| p.1).fold(f64::MAX, f64::min);
        assert_eq!(min_y, 1.0);
    }

    #[test]
    fn decimate_output_is_time_ordered_across_buckets() {
        let pts: Vec<(f64, f64)> = (0..1000).map(|i| (i as f64, (i as f64).sin())).collect();
        let out = min_max_decimate(&pts, 20);
        // Bucket boundaries are ascending, so consecutive bucket pairs never
        // step backwards by more than one bucket's width (~50 here).
        for w in out.windows(2) {
            assert!(w[1].0 >= w[0].0 - 60.0, "big backstep: {:?}", w);
        }
    }
}
