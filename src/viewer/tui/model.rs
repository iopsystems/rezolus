//! Navigation model: the section list plus per-section plot definitions,
//! parsed from the same JSON the web frontend consumes.

/// What kind of chart a plot resolves to in the TUI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlotKind {
    /// Gauge / delta_counter: query the base expression directly.
    Line,
    /// Histogram percentile scatter: wrap in `histogram_quantiles`.
    Percentiles,
    /// Bucket / quantile heatmap: not rendered in v1.
    HeatmapUnsupported,
}

/// One plot's render definition, extracted from a section View's JSON.
#[derive(Clone, Debug, PartialEq)]
pub struct PlotDef {
    pub title: String,
    pub base_query: String,
    pub kind: PlotKind,
    pub percentiles: Vec<f64>,
    pub unit_system: Option<String>,
}

/// A named group of plots within a section.
#[derive(Clone, Debug, PartialEq)]
pub struct NavGroup {
    pub name: String,
    pub plots: Vec<PlotDef>,
}

/// A top-level section (nav entry) and, once loaded, its groups.
#[derive(Clone, Debug, PartialEq)]
pub struct NavSection {
    pub name: String,
    pub route: String,
    /// `None` until the section body has been fetched/parsed.
    pub groups: Option<Vec<NavGroup>>,
}

/// Default percentiles, matching `charts/metric_types.js::DEFAULT_PERCENTILES`.
pub const DEFAULT_PERCENTILES: [f64; 5] = [0.5, 0.9, 0.99, 0.999, 0.9999];

/// Parse a single plot object from a section View's JSON.
/// Returns `None` if it has no query (nothing to render).
pub fn parse_plot(plot: &serde_json::Value) -> Option<PlotDef> {
    let base_query = plot.get("promql_query")?.as_str()?.to_string();
    let opts = plot.get("opts");
    let title = opts
        .and_then(|o| o.get("title"))
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();
    let type_ = opts
        .and_then(|o| o.get("type"))
        .and_then(|t| t.as_str())
        .unwrap_or("gauge");
    let subtype = opts.and_then(|o| o.get("subtype")).and_then(|s| s.as_str());
    let unit_system = opts
        .and_then(|o| o.get("format"))
        .and_then(|f| f.get("unit_system"))
        .and_then(|u| u.as_str())
        .map(|s| s.to_string());

    let kind = match type_ {
        "histogram" => match subtype {
            Some("buckets") | Some("quantile_heatmap") => PlotKind::HeatmapUnsupported,
            _ => PlotKind::Percentiles,
        },
        _ => PlotKind::Line,
    };

    let percentiles = opts
        .and_then(|o| o.get("percentiles"))
        .and_then(|p| p.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_f64()).collect::<Vec<_>>())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_PERCENTILES.to_vec());

    Some(PlotDef {
        title,
        base_query,
        kind,
        percentiles,
        unit_system,
    })
}

/// Parse the groups+plots out of a section View JSON (as produced by
/// `LazySectionStore::get_or_generate`).
pub fn parse_section_groups(view: &serde_json::Value) -> Vec<NavGroup> {
    let mut out = Vec::new();
    let Some(groups) = view.get("groups").and_then(|g| g.as_array()) else {
        return out;
    };
    for group in groups {
        let name = group
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or("")
            .to_string();
        let mut plots = Vec::new();
        if let Some(subgroups) = group.get("subgroups").and_then(|s| s.as_array()) {
            for sg in subgroups {
                if let Some(ps) = sg.get("plots").and_then(|p| p.as_array()) {
                    for p in ps {
                        if let Some(def) = parse_plot(p) {
                            plots.push(def);
                        }
                    }
                }
            }
        }
        out.push(NavGroup { name, plots });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn gauge_plot_is_line_with_verbatim_query() {
        let p = json!({
            "promql_query": "cpu_usage",
            "opts": { "title": "CPU", "type": "gauge",
                      "format": { "unit_system": "percentage" } }
        });
        let def = parse_plot(&p).unwrap();
        assert_eq!(def.kind, PlotKind::Line);
        assert_eq!(def.base_query, "cpu_usage");
        assert_eq!(def.title, "CPU");
        assert_eq!(def.unit_system.as_deref(), Some("percentage"));
    }

    #[test]
    fn histogram_percentiles_uses_defaults_when_absent() {
        let p = json!({
            "promql_query": "scheduler_runqueue_latency",
            "opts": { "title": "Runq", "type": "histogram" }
        });
        let def = parse_plot(&p).unwrap();
        assert_eq!(def.kind, PlotKind::Percentiles);
        assert_eq!(def.percentiles, DEFAULT_PERCENTILES.to_vec());
    }

    #[test]
    fn histogram_buckets_is_unsupported() {
        let p = json!({
            "promql_query": "x",
            "opts": { "type": "histogram", "subtype": "buckets" }
        });
        assert_eq!(parse_plot(&p).unwrap().kind, PlotKind::HeatmapUnsupported);
    }

    #[test]
    fn plot_without_query_is_skipped() {
        let p = json!({ "opts": { "type": "gauge" } });
        assert!(parse_plot(&p).is_none());
    }

    #[test]
    fn parse_groups_flattens_subgroups() {
        let view = json!({
            "groups": [
                { "name": "G1", "subgroups": [
                    { "plots": [ { "promql_query": "a", "opts": { "type": "gauge" } } ] },
                    { "plots": [ { "promql_query": "b", "opts": { "type": "gauge" } } ] }
                ] }
            ]
        });
        let groups = parse_section_groups(&view);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "G1");
        assert_eq!(groups[0].plots.len(), 2);
        assert_eq!(groups[0].plots[0].base_query, "a");
    }
}
