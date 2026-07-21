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
}
