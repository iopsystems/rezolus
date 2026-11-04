pub use metriken_charts::plot::{ChartStyle, HistogramView, Plot, PlotBuilder, Unit};
use metriken_query::MetricsSource;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};

/// Count distinct values of `key` across `labels`. Series missing the
/// key are skipped. Used by section generators to decide whether
/// per-device charts are worth showing (a single GPU/CPU makes the
/// per-device variant identical to the aggregate).
pub fn unique_label_count(labels: &[BTreeMap<String, String>], key: &str) -> usize {
    let mut seen: HashSet<&str> = HashSet::new();
    for l in labels {
        if let Some(v) = l.get(key) {
            seen.insert(v.as_str());
        }
    }
    seen.len()
}

/// Convenience: how many distinct values of `key` exist for `metric`
/// in `data`, looking across counter/gauge/histogram collections.
/// Returns 0 if the metric is unknown.
pub fn metric_unique_label_count(data: &dyn MetricsSource, metric: &str, key: &str) -> usize {
    data.label_values(metric, key).len()
}

/// True if `metric` exists in `data` under any metric type. Unlike
/// `metric_unique_label_count`, this does not require the metric to
/// carry any particular label key.
pub fn has_metric(data: &dyn MetricsSource, metric: &str) -> bool {
    !data.counter_labels(metric).is_empty()
        || !data.gauge_labels(metric).is_empty()
        || !data.histogram_labels(metric).is_empty()
}

/// Pre-rename recordings carry `blockio_latency`; post-rename recordings carry
/// `blockio_device_latency`. Pick whichever this recording actually has so old
/// captures keep rendering. Selecting the name from the data (rather than
/// emitting a PromQL `x or y` fallback) keeps generated queries readable and
/// does not depend on `or` support in the query engine.
///
/// The two are the same measurement: before the phases were split out, the
/// insert and issue hooks shared one timestamp and issue overwrote it, so
/// `blockio_latency` always reported the issue-to-complete span.
///
/// If neither is present (e.g. a recording taken with the `blockio_latency`
/// sampler disabled), name the metric a current agent would emit rather than
/// the retired pre-rename name — the chart is empty either way, but the query
/// text is user-visible (Query Explorer, copy-paste, TUI/MCP consumers).
///
/// This is the single source of truth for the alias: every consumer that needs
/// to pick between the two names calls this rather than re-deriving it, so they
/// cannot disagree about which name a given recording uses.
pub fn blockio_device_latency_metric(data: &dyn MetricsSource) -> &'static str {
    if has_metric(data, "blockio_device_latency") {
        "blockio_device_latency"
    } else if has_metric(data, "blockio_latency") {
        "blockio_latency"
    } else {
        "blockio_device_latency"
    }
}

/// Whether the recording carries the host-boundary traffic counters
/// (`network_host_bytes` / `network_host_packets`) added in 5.20.1.
///
/// Consumers plot the host-boundary series ABOVE the older per-interface
/// `network_*` panels, but only when the recording actually has them: a query
/// for an absent metric renders an empty plot, and a recording made before
/// those counters existed would otherwise get blank panels at the top of its
/// network view.
///
/// Checked on `network_host_bytes` alone — all four counters come from one
/// sampler reading one BPF map, so a recording has all of them or none.
///
/// Single source of truth, for the same reason as
/// [`blockio_device_latency_metric`]: the overview dashboard, the network
/// dashboard and the TUI all need this decision and must not disagree.
pub fn has_host_traffic_counters(data: &dyn MetricsSource) -> bool {
    has_metric(data, "network_host_bytes")
}

#[derive(Default, Serialize)]
pub struct View {
    // interval between consecutive datapoints as fractional seconds
    interval: f64,
    source: String,
    version: String,
    filename: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    filesize: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    start_time: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    end_time: Option<f64>,
    groups: Vec<Group>,
    sections: Vec<Section>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    #[serde(default)]
    pub metadata: HashMap<String, serde_json::Value>,
}

impl View {
    pub fn new(data: &dyn MetricsSource, sections: Vec<Section>) -> Self {
        let interval = data.interval();
        let source = data.source();
        let version = data.version();

        // Epoch milliseconds (source is seconds).
        let (start_time, end_time) = match data.time_range() {
            Some((min, max)) => (Some(min * 1000.0), Some(max * 1000.0)),
            None => (None, None),
        };

        Self {
            interval,
            source,
            version,
            filename: String::new(),
            filesize: None,
            start_time,
            end_time,
            groups: Vec::new(),
            sections,
            metadata: HashMap::new(),
        }
    }

    pub fn set_filename(&mut self, filename: String) {
        self.filename = filename;
    }

    pub fn set_filesize(&mut self, size: u64) {
        self.filesize = Some(size);
    }

    pub fn group(&mut self, group: Group) -> &Self {
        self.groups.push(group);
        self
    }

    /// Replace every query of every plot (`promql_query` and its
    /// experiment and total alternatives) with `f` of it.
    pub fn map_queries(&mut self, mut f: impl FnMut(&str) -> String) {
        for plot in self
            .groups
            .iter_mut()
            .flat_map(|g| g.subgroups.iter_mut())
            .flat_map(|sg| sg.plots.iter_mut())
        {
            for query in plot.queries_mut() {
                *query = f(query);
            }
        }
    }

    /// Set each plot's scope to `f` of its queries (`promql_query` and its
    /// experiment and total alternatives).
    pub fn scope_plots(&mut self, mut f: impl FnMut(&[&str]) -> Vec<String>) {
        for plot in self
            .groups
            .iter_mut()
            .flat_map(|g| g.subgroups.iter_mut())
            .flat_map(|sg| sg.plots.iter_mut())
        {
            let scope = f(&plot.queries());
            plot.plot.set_scope(Some(scope));
        }
    }

    /// Every query of every plot, in order: what [`map_queries`] visits.
    ///
    /// [`map_queries`]: Self::map_queries
    pub fn queries(&self) -> Vec<&str> {
        self.groups
            .iter()
            .flat_map(|g| g.subgroups.iter())
            .flat_map(|sg| sg.plots.iter())
            .flat_map(PagePlot::queries)
            .collect()
    }
}

#[derive(Clone, Default, Serialize)]
pub struct Section {
    pub name: String,
    pub route: String,
    /// The recording this entry renders for, when a template renders once
    /// per recording of a combined source and more than once: the
    /// sidebar groups entries by it. Absent otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    /// The section the page opens on, when it is not the usual one: set by
    /// [`crate::dashboard::add_beside_sections`] for a comparison whose
    /// baseline has no system metrics. Absent otherwise.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub landing: bool,
}

#[derive(Serialize, Default)]
pub struct SubGroup {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub plots: Vec<PagePlot>,
}

#[derive(Serialize)]
pub struct Group {
    name: String,
    id: String,
    subgroups: Vec<SubGroup>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    #[serde(default)]
    pub metadata: HashMap<String, serde_json::Value>,
}

impl Group {
    pub fn new<T: Into<String>, U: Into<String>>(name: T, id: U) -> Self {
        Self {
            name: name.into(),
            id: id.into(),
            subgroups: Vec::new(),
            metadata: HashMap::new(),
        }
    }

    /// Ensures a trailing subgroup exists to append plots to. Used by the
    /// legacy `Group::plot_promql*` call sites so they keep working
    /// without conversion — the first legacy call opens a default
    /// unnamed subgroup, subsequent legacy calls append to the most
    /// recently opened subgroup.
    ///
    /// NOTE: if the caller has already opened a subgroup via `subgroup()`
    /// or `subgroup_unnamed()`, a subsequent legacy `plot_promql*` call
    /// appends to THAT subgroup — even if it is named. Do not mix the
    /// legacy flat-plot API with the subgroup API on the same `Group`
    /// unless you intend that behavior.
    fn tail_subgroup_mut(&mut self) -> &mut SubGroup {
        if self.subgroups.is_empty() {
            self.subgroups.push(SubGroup::default());
        }
        self.subgroups.last_mut().unwrap()
    }

    /// True when no subgroup has been added. Lets a caller skip a group whose
    /// contents are all gated on hardware-dependent metrics.
    pub fn is_empty(&self) -> bool {
        self.subgroups.is_empty()
    }

    /// Remove the most recently opened subgroup if it holds no plots. A
    /// section template omits a subgroup its predicates left empty.
    pub(crate) fn drop_last_subgroup_if_empty(&mut self) {
        if self.subgroups.last().is_some_and(|sg| sg.plots.is_empty()) {
            self.subgroups.pop();
        }
    }

    /// Open a named subgroup. Returns a mutable reference so the
    /// caller can chain plot calls on it.
    pub fn subgroup<T: Into<String>>(&mut self, name: T) -> &mut SubGroup {
        self.subgroups.push(SubGroup {
            name: Some(name.into()),
            ..SubGroup::default()
        });
        self.subgroups.last_mut().unwrap()
    }

    /// Open an unnamed subgroup. Use when you want the "break to a new
    /// vertical band" effect without a visible header.
    pub fn subgroup_unnamed(&mut self) -> &mut SubGroup {
        self.subgroups.push(SubGroup::default());
        self.subgroups.last_mut().unwrap()
    }

    /// Legacy: append a plot to the current (or default) subgroup.
    pub fn plot_promql(&mut self, plot: PlotBuilder, promql_query: String) {
        self.tail_subgroup_mut().plot_promql(plot, promql_query);
    }

    /// Legacy: append a plot with description-autofill support.
    pub fn plot_promql_with_descriptions(
        &mut self,
        plot: PlotBuilder,
        promql_query: String,
        descriptions: Option<&HashMap<String, String>>,
    ) {
        self.tail_subgroup_mut()
            .plot_promql_with_descriptions(plot, promql_query, descriptions);
    }

    /// Find an existing named subgroup by exact name match.
    pub fn find_subgroup(&mut self, name: &str) -> Option<&mut SubGroup> {
        self.subgroups
            .iter_mut()
            .find(|sg| sg.name.as_deref() == Some(name))
    }

    /// Lazily return the trailing or default unnamed subgroup. Use for
    /// callers that want the "land in an unnamed catch-all bucket"
    /// semantics without going through `plot_promql*` on `Group`.
    pub fn default_subgroup(&mut self) -> &mut SubGroup {
        self.tail_subgroup_mut()
    }
}

impl SubGroup {
    pub fn plot_promql(&mut self, plot: PlotBuilder, promql_query: String) {
        self.plot_promql_with_descriptions(plot, promql_query, None);
    }

    /// Mutable access to the most recently pushed plot. Used by callers
    /// that set `promql_query_experiment` (the category generator) right
    /// after `plot_promql*`.
    pub fn plots_mut_last(&mut self) -> Option<&mut PagePlot> {
        self.plots.last_mut()
    }

    pub fn plot_promql_with_descriptions(
        &mut self,
        mut plot: PlotBuilder,
        promql_query: String,
        descriptions: Option<&HashMap<String, String>>,
    ) {
        if plot.description().is_none()
            && let Some(descriptions) = descriptions
        {
            let mut best_match: Option<(usize, &str, &str)> = None;
            for (name, desc) in descriptions {
                if let Some(pos) = promql_query.find(name.as_str()) {
                    let dominated = best_match.is_some_and(|(best_pos, best_name, _)| {
                        name.len() < best_name.len()
                            || (name.len() == best_name.len()
                                && (pos > best_pos
                                    || (pos == best_pos && name.as_str() > best_name)))
                    });
                    if !dominated {
                        best_match = Some((pos, name.as_str(), desc.as_str()));
                    }
                }
            }
            if let Some((_, _, desc)) = best_match {
                plot = plot.with_description(desc);
            }
        }

        self.plots.push(PagePlot {
            plot: plot.build(promql_query),
            promql_query_experiment: None,
            width: PlotWidth::default(),
        });
    }

    /// Set the optional description text rendered below the subgroup header.
    pub fn describe<T: Into<String>>(&mut self, text: T) -> &mut Self {
        self.description = Some(text.into());
        self
    }

    /// Append a plot that spans the full width of the group's grid.
    pub fn plot_promql_full(&mut self, plot: PlotBuilder, promql_query: String) {
        self.plot_promql_full_with_descriptions(plot, promql_query, None);
    }

    /// Full-width variant with description autofill.
    pub fn plot_promql_full_with_descriptions(
        &mut self,
        plot: PlotBuilder,
        promql_query: String,
        descriptions: Option<&HashMap<String, String>>,
    ) {
        self.plot_promql_with_descriptions(plot, promql_query, descriptions);
        if let Some(plot) = self.plots.last_mut() {
            plot.width = PlotWidth::Full;
        }
    }

    /// Append a half-width **rate** card then a half-width **mean**
    /// card for a percentile histogram `selector`; they render side by
    /// side. Call immediately before the percentile plot so the
    /// subgroup reads rate | mean | <percentiles>.
    ///
    /// `mean_unit` is the mean card's `Unit` (`Time` for latency,
    /// `Bytes` for size); its formatter unit-system is derived from it.
    /// The rate card mirrors existing counter rate cards (`Unit::Rate`,
    /// no unit system).
    pub fn histogram_rate_mean(
        &mut self,
        title_stem: &str,
        id_stem: &str,
        selector: &str,
        rate: RateSource,
        mean_unit: Unit,
    ) {
        let unit_system = match mean_unit {
            Unit::Time => Unit::Time,
            Unit::Bytes => Unit::Bytes,
            // This helper is only used for latency/size means; guard
            // anything else loudly rather than mis-format silently.
            other => {
                panic!("histogram_rate_mean: unsupported mean unit {other:?}; only Time/Bytes")
            }
        };
        let rate_query = match rate {
            RateSource::Counter(q) => q,
            RateSource::FromHistogram => {
                format!("sum(histogram_irate({selector}))")
            }
        };
        self.plot_promql(
            Plot::counter(
                format!("{title_stem} Rate"),
                format!("{id_stem}-rate"),
                Unit::Rate,
            ),
            rate_query,
        );
        self.plot_promql(
            Plot::gauge(
                format!("{title_stem} Mean/Total"),
                format!("{id_stem}-mean"),
                mean_unit,
            )
            .with_unit_system(unit_system)
            .with_axis_label("Mean")
            .with_total_query(format!("histogram_sum({selector})")),
            format!("histogram_mean({selector})"),
        );
    }
}

/// A plot as a page places it: the [`Plot`], and the fields the viewer's
/// page layout and A/B compare page read, which no chart module does.
#[derive(Serialize, Clone)]
pub struct PagePlot {
    #[serde(flatten)]
    pub plot: Plot,
    /// The query the A/B compare page draws for the experiment recording,
    /// when it differs from the baseline's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub promql_query_experiment: Option<String>,
    #[serde(skip_serializing_if = "plot_width_is_half", default)]
    pub width: PlotWidth,
}

impl PagePlot {
    /// The query, the experiment query, then the total query.
    fn queries(&self) -> Vec<&str> {
        [
            Some(self.plot.query()),
            self.promql_query_experiment.as_deref(),
            self.plot.total_query(),
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    /// Mutable references to what [`queries`](Self::queries) returns, in
    /// the same order.
    fn queries_mut(&mut self) -> impl Iterator<Item = &mut String> {
        let mut plot = self.plot.queries_mut();
        let query = plot.next();
        query
            .into_iter()
            .chain(self.promql_query_experiment.as_mut())
            .chain(plot)
    }
}

/// Where the "rate" half of a histogram's rate|mean card pair gets its
/// data.
///
/// - `Counter(query)`: a verbatim rate query over a *separate, accurate*
///   counter column (e.g. `sum(irate(blockio_operations…[5m]))`) — it
///   is deliberately independent of the histogram `selector`, since the
///   accurate event count lives in a different metric.
/// - `FromHistogram`: derive an approximate rate from the histogram
///   column itself; subject to lock-free snapshot undercount — use only
///   when no accurate counter exists.
#[derive(Debug)]
pub enum RateSource {
    Counter(String),
    FromHistogram,
}

#[derive(Serialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlotWidth {
    #[default]
    Half,
    Full,
}

fn plot_width_is_half(w: &PlotWidth) -> bool {
    matches!(w, PlotWidth::Half)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Unit` names exactly the systems `units.js` scales; a system added to
    /// one and not the other would be refused, or shown verbatim, by the viewer.
    #[test]
    fn units_match_the_viewer() {
        let js = include_str!("../../../src/viewer/assets/lib/charts/util/units.js");
        let block = &js[js.find("const UNIT_SYSTEMS = {").unwrap()..];
        let block = &block[..block.find("\n};").unwrap()];
        let viewer: Vec<&str> = block
            .lines()
            .filter_map(|l| l.strip_prefix("    ")?.strip_suffix(": {"))
            .filter(|k| k.chars().all(|c| c.is_ascii_lowercase()))
            .collect();
        let ours: Vec<String> = Unit::ALL.iter().map(Unit::to_string).collect();
        assert_eq!(viewer, ours);
        for name in &ours {
            assert_eq!(name.parse::<Unit>().unwrap().to_string(), *name);
        }
    }

    #[test]
    fn maybe_unit_system_drops_an_unknown_system() {
        let unit = |plot: PlotBuilder| {
            serde_json::to_value(plot.build("q")).unwrap()["opts"]["format"]["unit_system"].clone()
        };
        let known = Plot::counter("t", "id", Unit::Count).maybe_unit_system(Some("bytes"));
        assert_eq!(unit(known), "bytes");
        let unknown = Plot::counter("t", "id", Unit::Count)
            .maybe_unit_system(Some("<img src=x onerror=alert(1)>"));
        assert_eq!(unit(unknown), "count", "the unknown system is not applied");
        let err = "watts".parse::<Unit>().unwrap_err();
        assert!(
            err.contains("\"watts\"") && err.contains("percentage"),
            "{err}"
        );
    }

    fn make_plot(width: PlotWidth) -> PagePlot {
        PagePlot {
            plot: Plot::counter("t", "id", Unit::Count).build("up"),
            promql_query_experiment: None,
            width,
        }
    }

    #[test]
    fn plot_width_half_is_elided_from_json() {
        let plot = make_plot(PlotWidth::Half);
        let json = serde_json::to_value(&plot).unwrap();
        assert!(
            json.get("width").is_none(),
            "expected `width` to be omitted when Half, got {json}"
        );
    }

    #[test]
    fn plot_width_full_is_serialized() {
        let plot = make_plot(PlotWidth::Full);
        let json = serde_json::to_value(&plot).unwrap();
        assert_eq!(json["width"], serde_json::json!("full"));
    }

    #[test]
    fn subgroup_serializes_with_optional_name_and_description() {
        let sg = SubGroup {
            name: Some("Operations".into()),
            description: Some("Summary + per-device IOPS.".into()),
            plots: vec![],
        };
        let json = serde_json::to_value(&sg).unwrap();
        assert_eq!(json["name"], "Operations");
        assert_eq!(json["description"], "Summary + per-device IOPS.");
        assert_eq!(json["plots"], serde_json::json!([]));
    }

    #[test]
    fn subgroup_elides_missing_name_and_description() {
        let sg = SubGroup {
            name: None,
            description: None,
            plots: vec![],
        };
        let json = serde_json::to_value(&sg).unwrap();
        assert!(json.get("name").is_none());
        assert!(json.get("description").is_none());
    }

    #[test]
    fn legacy_plot_promql_creates_single_unnamed_subgroup() {
        let mut g = Group::new("G", "g");
        g.plot_promql(Plot::counter("t1", "id1", Unit::Count), "up".into());
        g.plot_promql(Plot::counter("t2", "id2", Unit::Count), "up".into());
        let json = serde_json::to_value(&g).unwrap();
        let subs = json["subgroups"].as_array().expect("subgroups present");
        assert_eq!(subs.len(), 1, "legacy calls collapse to one subgroup");
        assert!(subs[0].get("name").is_none(), "default subgroup is unnamed");
        assert_eq!(
            subs[0]["plots"].as_array().unwrap().len(),
            2,
            "both legacy plots land in the default subgroup"
        );
    }

    #[test]
    fn group_no_longer_exposes_bare_plots_in_json() {
        let g = Group::new("G", "g");
        let json = serde_json::to_value(&g).unwrap();
        assert!(
            json.get("plots").is_none(),
            "Group JSON should expose subgroups, not plots"
        );
    }

    #[test]
    fn plot_promql_full_marks_plot_as_full_width() {
        let mut g = Group::new("G", "g");
        let sg = g.subgroup("Ops");
        sg.plot_promql_full(Plot::counter("Summary", "sum", Unit::Count), "up".into());
        let json = serde_json::to_value(&g).unwrap();
        assert_eq!(
            json["subgroups"][0]["plots"][0]["width"],
            serde_json::json!("full")
        );
    }

    #[test]
    fn describe_sets_the_description_field() {
        let mut g = Group::new("G", "g");
        g.subgroup("Ops")
            .describe("Shows total throughput and IOPS.");
        let json = serde_json::to_value(&g).unwrap();
        assert_eq!(
            json["subgroups"][0]["description"],
            "Shows total throughput and IOPS."
        );
    }

    #[test]
    fn unique_label_count_returns_zero_for_empty_input() {
        let labels: Vec<BTreeMap<String, String>> = vec![];
        assert_eq!(unique_label_count(&labels, "id"), 0);
    }

    fn make_labels(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn unique_label_count_counts_distinct_values() {
        let labels: Vec<BTreeMap<String, String>> = vec![
            make_labels(&[("id", "0"), ("state", "user")]),
            make_labels(&[("id", "0"), ("state", "system")]),
            make_labels(&[("id", "1"), ("state", "user")]),
            make_labels(&[("id", "1"), ("state", "system")]),
        ];
        assert_eq!(unique_label_count(&labels, "id"), 2);
    }

    #[test]
    fn unique_label_count_ignores_series_missing_the_key() {
        let labels: Vec<BTreeMap<String, String>> =
            vec![make_labels(&[("id", "0")]), make_labels(&[("other", "x")])];
        assert_eq!(unique_label_count(&labels, "id"), 1);
    }

    #[test]
    fn histogram_rate_mean_counter_source_emits_two_half_width_plots() {
        let mut g = Group::new("G", "g");
        let sg = g.subgroup("S");
        sg.histogram_rate_mean(
            "Read",
            "latency-read",
            "blockio_latency{op=\"read\"}",
            RateSource::Counter("sum(irate(blockio_operations{op=\"read\"}[5m]))".to_string()),
            Unit::Time,
        );
        let json = serde_json::to_value(&g).unwrap();
        let plots = json["subgroups"][0]["plots"].as_array().unwrap();
        assert_eq!(plots.len(), 2);

        assert_eq!(plots[0]["opts"]["title"], "Read Rate");
        assert_eq!(plots[0]["opts"]["id"], "latency-read-rate");
        assert_eq!(plots[0]["opts"]["type"], "delta_counter");
        assert!(
            plots[0].get("width").is_none(),
            "half width is the serde default (omitted)"
        );
        assert_eq!(
            plots[0]["promql_query"],
            "sum(irate(blockio_operations{op=\"read\"}[5m]))"
        );

        assert_eq!(plots[1]["opts"]["title"], "Read Mean/Total");
        assert_eq!(plots[1]["opts"]["id"], "latency-read-mean");
        assert_eq!(plots[1]["opts"]["type"], "gauge");
        assert_eq!(plots[1]["opts"]["format"]["unit_system"], "time");
        assert_eq!(
            plots[1]["promql_query"],
            "histogram_mean(blockio_latency{op=\"read\"})"
        );
        assert_eq!(
            plots[1]["promql_query_total"],
            "histogram_sum(blockio_latency{op=\"read\"})"
        );
    }

    /// `map_queries` rewrites the query, the experiment query and the total
    /// query once each, and `queries` lists them in that order.
    #[test]
    fn map_queries_visits_every_query_once() {
        let mut g = Group::new("G", "g");
        g.plot_promql(
            Plot::gauge("t", "id", Unit::Count).with_total_query("t"),
            "q".into(),
        );
        g.subgroups[0]
            .plots_mut_last()
            .unwrap()
            .promql_query_experiment = Some("e".into());
        let mut view = View::default();
        view.group(g);
        assert_eq!(view.queries(), ["q", "e", "t"]);
        view.map_queries(|q| format!("f({q})"));
        let json = serde_json::to_value(&view).unwrap();
        let plot = &json["groups"][0]["subgroups"][0]["plots"][0];
        assert_eq!(plot["promql_query"], "f(q)");
        assert_eq!(plot["promql_query_experiment"], "f(e)");
        assert_eq!(plot["promql_query_total"], "f(t)");
        let mut seen = Vec::new();
        view.scope_plots(|qs| {
            seen.extend(qs.iter().map(|q| q.to_string()));
            vec!["u".into()]
        });
        assert_eq!(seen, ["f(q)", "f(e)", "f(t)"]);
    }

    #[test]
    fn histogram_rate_mean_from_histogram_derives_rate_from_count() {
        let mut g = Group::new("G", "g");
        let sg = g.subgroup("S");
        sg.histogram_rate_mean(
            "Runqueue",
            "scheduler-runqueue-latency",
            "scheduler_runqueue_latency",
            RateSource::FromHistogram,
            Unit::Time,
        );
        let json = serde_json::to_value(&g).unwrap();
        let plots = json["subgroups"][0]["plots"].as_array().unwrap();
        assert_eq!(plots.len(), 2);
        assert_eq!(
            plots[0]["promql_query"],
            "sum(histogram_irate(scheduler_runqueue_latency))"
        );
        assert_eq!(plots[1]["opts"]["title"], "Runqueue Mean/Total");
        assert_eq!(
            plots[1]["promql_query"],
            "histogram_mean(scheduler_runqueue_latency)"
        );
        assert_eq!(
            plots[1]["promql_query_total"],
            "histogram_sum(scheduler_runqueue_latency)"
        );
    }
}

#[cfg(test)]
mod plot_serialize_tests {
    use super::*;

    #[test]
    fn plot_promql_query_experiment_round_trips() {
        let mut sg = SubGroup::default();
        sg.plot_promql(
            Plot::counter("X", "kpi-x", Unit::Count),
            "metric_a".to_string(),
        );
        // Mutate the just-pushed plot to set the experiment query, then
        // serialize and confirm it appears in the JSON.
        let plot = sg.plots.last_mut().unwrap();
        plot.promql_query_experiment = Some("metric_b".to_string());
        let json = serde_json::to_string(plot).unwrap();
        assert!(json.contains("\"promql_query_experiment\":\"metric_b\""));

        // Default (None) is omitted from the JSON.
        plot.promql_query_experiment = None;
        let json = serde_json::to_string(plot).unwrap();
        assert!(!json.contains("promql_query_experiment"), "got {json}");
    }
}
