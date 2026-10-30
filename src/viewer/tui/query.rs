//! Bridges `PlotDef`s to `MetricsSource::query_range` and shapes results
//! into chart-ready series.

use std::collections::{BTreeSet, HashMap, HashSet};

use metriken_query::{MetricsSource, QueryResult};

use metriken_storage::is_internal_label;

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
            let metrics: Vec<&Labels> = result.iter().map(|s| &s.metric).collect();
            let labels = series_labels(&metrics);
            let series: Vec<Series> = result
                .into_iter()
                .zip(labels)
                .map(|(s, label)| Series {
                    label,
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

type Labels = HashMap<String, String>;

/// Display labels for the series of one matrix result, in order, by the
/// rules of the browser viewer (`data.js` with `seriesNamer` from
/// `assets/lib/charts/util/labels.js`):
/// - a `percentile`/`quantile` label names its series `p99`-style;
/// - a series with both `id` and `vendor` is `<vendor> <id>`;
/// - otherwise a series is named by the visible labels whose values differ
///   between the result's series: the value of the one key, or `key=value`
///   pairs when there are several;
/// - when no visible label differs, by its first visible label in key order
///   (the browser takes the first in the order the server sends, which is
///   unspecified);
/// - a series none of these name is `Series <n>`, numbered from 1 (the
///   browser keeps an empty label value as the name).
fn series_labels(metrics: &[&Labels]) -> Vec<String> {
    let keys = varying_label_keys(metrics);
    metrics
        .iter()
        .enumerate()
        .map(|(n, m)| {
            if let Some(q) = m.get("percentile").or_else(|| m.get("quantile")) {
                return match q.parse::<f64>() {
                    Ok(v) => percentile_label(if v > 1.0 { v / 100.0 } else { v }),
                    Err(_) => q.clone(),
                };
            }
            if let (Some(id), Some(vendor)) = (m.get("id"), m.get("vendor")) {
                return format!("{vendor} {id}");
            }
            let name = match keys.as_slice() {
                [] => first_visible_label_value(m),
                [k] => m.get(k).cloned().unwrap_or_default(),
                _ => keys
                    .iter()
                    .filter_map(|k| m.get(k).map(|v| format!("{k}={v}")))
                    .collect::<Vec<_>>()
                    .join(", "),
            };
            if name.is_empty() {
                format!("Series {}", n + 1)
            } else {
                name
            }
        })
        .collect()
}

/// The visible label keys whose values differ between `metrics`, sorted.
/// A key a series lacks counts as a value of its own. A label every series
/// shares with one value, such as a recording's `host` or `source`, is not
/// among them.
fn varying_label_keys(metrics: &[&Labels]) -> Vec<String> {
    let keys: BTreeSet<&String> = metrics
        .iter()
        .flat_map(|m| m.keys())
        .filter(|k| !is_internal_label(k))
        .collect();
    keys.into_iter()
        .filter(|k| {
            let values: HashSet<Option<&String>> = metrics.iter().map(|m| m.get(*k)).collect();
            values.len() > 1
        })
        .cloned()
        .collect()
}

/// The value of the first visible label in key order, or empty.
fn first_visible_label_value(metric: &Labels) -> String {
    metric
        .iter()
        .filter(|(k, _)| !is_internal_label(k))
        .min_by_key(|(k, _)| *k)
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

    fn labels(pairs: &[(&str, &str)]) -> Labels {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn series_are_named_by_the_label_that_differs() {
        // `host` sorts before `id`, and `host` and `source` are shared by
        // every series; the name must come from `id`.
        let a = labels(&[
            ("__name__", "cpu"),
            ("host", "h"),
            ("source", "r"),
            ("id", "0"),
        ]);
        let b = labels(&[
            ("__name__", "cpu"),
            ("host", "h"),
            ("source", "r"),
            ("id", "1"),
        ]);
        assert_eq!(series_labels(&[&a, &b]), vec!["0", "1"]);
    }

    #[test]
    fn several_differing_labels_name_as_pairs() {
        let a = labels(&[("host", "h"), ("id", "0"), ("state", "user")]);
        let b = labels(&[("host", "h"), ("id", "1"), ("state", "system")]);
        assert_eq!(
            series_labels(&[&a, &b]),
            vec!["id=0, state=user", "id=1, state=system"]
        );
    }

    #[test]
    fn a_missing_label_counts_as_differing() {
        let a = labels(&[("host", "h"), ("op", "read")]);
        let b = labels(&[("host", "h")]);
        assert_eq!(series_labels(&[&a, &b]), vec!["read", "Series 2"]);
    }

    #[test]
    fn a_device_is_named_by_vendor_and_id() {
        let a = labels(&[("id", "0"), ("vendor", "nvidia")]);
        let b = labels(&[("id", "0"), ("vendor", "intel")]);
        assert_eq!(series_labels(&[&a, &b]), vec!["nvidia 0", "intel 0"]);
    }

    #[test]
    fn a_single_series_takes_its_first_visible_label_in_key_order() {
        let a = labels(&[
            ("__incarnation__", "1"),
            ("source", "r"),
            ("zone", "z"),
            ("node", "n"),
            ("rack", "k"),
            ("host", "h"),
            ("pod", "p"),
        ]);
        assert_eq!(series_labels(&[&a]), vec!["h"]);
    }

    #[test]
    fn percentile_label_wins() {
        let a = labels(&[("host", "h"), ("percentile", "99")]);
        let b = labels(&[("host", "h"), ("percentile", "50")]);
        assert_eq!(series_labels(&[&a, &b]), vec!["p99", "p50"]);
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
