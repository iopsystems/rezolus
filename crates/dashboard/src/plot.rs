use crate::data::DashboardData;
use serde::Serialize;
use std::collections::HashMap;

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
    #[serde(skip_serializing_if = "Option::is_none")]
    num_series: Option<usize>,
    groups: Vec<Group>,
    sections: Vec<Section>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    #[serde(default)]
    pub metadata: HashMap<String, serde_json::Value>,
}

impl View {
    pub fn new(data: &dyn DashboardData, sections: Vec<Section>) -> Self {
        let interval = data.interval();
        let source = data.source().to_string();
        let version = data.version().to_string();
        let filename = data.filename().to_string();

        // Epoch milliseconds (source is nanoseconds).
        let (start_time, end_time) = match data.time_range() {
            Some((min, max)) => (Some(min as f64 / 1e6), Some(max as f64 / 1e6)),
            None => (None, None),
        };

        // Count total time series (each metric x label combination)
        let num_series = {
            let mut count = 0usize;
            for name in data.counter_names() {
                count += data.counter_label_count(name);
            }
            for name in data.gauge_names() {
                count += data.gauge_label_count(name);
            }
            for name in data.histogram_names() {
                count += data.histogram_label_count(name);
            }
            Some(count)
        };

        Self {
            interval,
            source,
            version,
            filename,
            filesize: None,
            start_time,
            end_time,
            num_series,
            groups: Vec::new(),
            sections,
            metadata: HashMap::new(),
        }
    }

    pub fn set_filesize(&mut self, size: u64) {
        self.filesize = Some(size);
    }

    pub fn group(&mut self, group: Group) -> &Self {
        self.groups.push(group);
        self
    }
}

#[derive(Clone, Serialize)]
pub struct Section {
    pub name: String,
    pub route: String,
}

#[derive(Serialize, Default)]
pub struct SubGroup {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub plots: Vec<Plot>,
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
    /// flat `Group::plot_sql*` call sites so they keep working without
    /// constructing an explicit subgroup — the first flat call opens a
    /// default unnamed subgroup, subsequent flat calls append to the most
    /// recently opened subgroup.
    ///
    /// NOTE: if the caller has already opened a subgroup via `subgroup()`
    /// or `subgroup_unnamed()`, a subsequent flat `plot_sql*` call
    /// appends to THAT subgroup — even if it is named. Do not mix the
    /// flat-plot API with the subgroup API on the same `Group` unless
    /// you intend that behavior.
    fn tail_subgroup_mut(&mut self) -> &mut SubGroup {
        if self.subgroups.is_empty() {
            self.subgroups.push(SubGroup::default());
        }
        self.subgroups.last_mut().unwrap()
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

    /// Flat: append a SQL-bodied plot to the current (or default) subgroup.
    pub fn plot_sql(&mut self, opts: PlotOpts, sql_query: String) {
        self.tail_subgroup_mut().plot_sql(opts, sql_query);
    }

    pub fn plot_sql_with_descriptions(
        &mut self,
        opts: PlotOpts,
        sql_query: String,
        descriptions: Option<&HashMap<String, String>>,
    ) {
        self.tail_subgroup_mut()
            .plot_sql_with_descriptions(opts, sql_query, descriptions);
    }

    /// Flat full-width SQL-bodied plot.
    pub fn plot_sql_full(&mut self, opts: PlotOpts, sql_query: String) {
        self.tail_subgroup_mut().plot_sql_full(opts, sql_query);
    }

    pub fn plot_sql_full_with_descriptions(
        &mut self,
        opts: PlotOpts,
        sql_query: String,
        descriptions: Option<&HashMap<String, String>>,
    ) {
        self.tail_subgroup_mut()
            .plot_sql_full_with_descriptions(opts, sql_query, descriptions);
    }

    /// Find an existing named subgroup by exact name match.
    pub fn find_subgroup(&mut self, name: &str) -> Option<&mut SubGroup> {
        self.subgroups
            .iter_mut()
            .find(|sg| sg.name.as_deref() == Some(name))
    }

    /// Lazily return the trailing or default unnamed subgroup. Use for
    /// callers that want the "land in an unnamed catch-all bucket"
    /// semantics without going through `plot_sql*` on `Group`.
    pub fn default_subgroup(&mut self) -> &mut SubGroup {
        self.tail_subgroup_mut()
    }
}

impl SubGroup {
    /// Mutable access to the most recently pushed plot. Used by callers
    /// that mutate per-plot fields (e.g. `sql_query_experiment` on the
    /// category generator) right after `plot_sql*`.
    pub fn plots_mut_last(&mut self) -> Option<&mut Plot> {
        self.plots.last_mut()
    }

    /// Append a SQL-bodied plot.
    pub fn plot_sql(&mut self, opts: PlotOpts, sql_query: String) {
        self.plot_sql_with_descriptions(opts, sql_query, None);
    }

    pub fn plot_sql_with_descriptions(
        &mut self,
        mut opts: PlotOpts,
        sql_query: String,
        descriptions: Option<&HashMap<String, String>>,
    ) {
        // Description-autofill matches metric names that appear in the
        // SQL body — e.g. `_src."cpu_cycles/0"` will match the description
        // for `cpu_cycles`. Longest-name-wins, ties broken by later
        // position then lexicographic name (stable across HashMap
        // iteration order).
        if opts.description.is_none()
            && let Some(descriptions) = descriptions
        {
            let mut best_match: Option<(usize, &str, &str)> = None;
            for (name, desc) in descriptions {
                if let Some(pos) = sql_query.find(name.as_str()) {
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
                opts.description = Some(desc.to_string());
            }
        }

        self.plots.push(Plot {
            opts,
            data: Vec::new(),
            min_value: None,
            max_value: None,
            time_data: None,
            formatted_time_data: None,
            series_names: None,
            sql_query: Some(sql_query),
            sql_query_experiment: None,
            width: PlotWidth::default(),
        });
    }

    /// Full-width SQL-bodied plot.
    pub fn plot_sql_full(&mut self, opts: PlotOpts, sql_query: String) {
        self.plot_sql_full_with_descriptions(opts, sql_query, None);
    }

    pub fn plot_sql_full_with_descriptions(
        &mut self,
        opts: PlotOpts,
        sql_query: String,
        descriptions: Option<&HashMap<String, String>>,
    ) {
        self.plot_sql_with_descriptions(opts, sql_query, descriptions);
        if let Some(plot) = self.plots.last_mut() {
            plot.width = PlotWidth::Full;
        }
    }

    /// Set the optional description text rendered below the subgroup header.
    pub fn describe<T: Into<String>>(&mut self, text: T) -> &mut Self {
        self.description = Some(text.into());
        self
    }
}

#[derive(Serialize, Clone)]
pub struct Plot {
    data: Vec<Vec<f64>>,
    opts: PlotOpts,
    #[serde(skip_serializing_if = "Option::is_none")]
    min_value: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_value: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    time_data: Option<Vec<f64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    formatted_time_data: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    series_names: Option<Vec<String>>,
    /// SQL query body emitted by dashboard generators. Body conventions:
    ///   - References parquet columns directly via `[*COLUMNS('regex')]`
    ///     and/or by literal name (e.g. `"cpu_cycles/0"`).
    ///   - Uses `_src` as the parquet alias — viewer-sql binds it to
    ///     `read_parquet('<registered>')` at submit time.
    ///   - Final SELECT projects `t` (DOUBLE seconds) and `v` (numeric);
    ///     per-id queries also project label columns (e.g. `id`).
    /// See `crates/viewer-sql/duckdb.md` for the full SQL convention.
    #[serde(skip_serializing_if = "Option::is_none")]
    sql_query: Option<String>,
    /// Compare-mode experiment-side variant of `sql_query`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sql_query_experiment: Option<String>,
    #[serde(skip_serializing_if = "plot_width_is_half", default)]
    pub width: PlotWidth,
}

impl Plot {}

#[derive(Serialize, Clone)]
#[serde(rename_all = "snake_case")]
pub enum MetricType {
    Gauge,
    DeltaCounter,
    Histogram,
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

#[derive(Serialize, Clone)]
pub struct PlotOpts {
    title: String,
    id: String,
    #[serde(rename = "type")]
    metric_type: MetricType,
    #[serde(skip_serializing_if = "Option::is_none")]
    subtype: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    percentiles: Option<Vec<f64>>,
    format: FormatConfig,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
}

#[derive(Default, Clone, Serialize)]
pub struct Range {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
}

#[derive(Serialize, Clone)]
pub struct FormatConfig {
    x_axis_label: Option<String>,
    y_axis_label: Option<String>,

    unit_system: Option<String>, // e.g., "percentage", "time", "bitrate"
    precision: Option<u8>,       // decimal places

    log_scale: Option<bool>,

    // Values outside are clamped at render time.
    #[serde(skip_serializing_if = "Option::is_none")]
    range: Option<Range>,

    value_label: Option<String>, // label used in tooltips for the value
}

impl PlotOpts {
    /// A gauge metric represents a point-in-time value (e.g., memory usage, temperature).
    pub fn gauge<T: Into<String>, U: Into<String>>(title: T, id: U, unit: Unit) -> Self {
        Self {
            title: title.into(),
            id: id.into(),
            metric_type: MetricType::Gauge,
            subtype: None,
            percentiles: None,
            format: FormatConfig::new(unit),
            description: None,
        }
    }

    /// A delta counter metric represents the rate of change of a cumulative counter
    /// (e.g., CPU usage rate, packet rate).
    pub fn counter<T: Into<String>, U: Into<String>>(title: T, id: U, unit: Unit) -> Self {
        Self {
            title: title.into(),
            id: id.into(),
            metric_type: MetricType::DeltaCounter,
            subtype: None,
            percentiles: None,
            format: FormatConfig::new(unit),
            description: None,
        }
    }

    /// A histogram metric represents a distribution (e.g., latency, IO size).
    /// The subtype determines the visualization and query wrapping:
    /// - "percentiles": shows percentile scatter plot, wraps query with histogram_quantiles()
    /// - "buckets": shows bucket heatmap, wraps query with histogram_heatmap()
    pub fn histogram<T: Into<String>, U: Into<String>>(
        title: T,
        id: U,
        unit: Unit,
        subtype: &str,
    ) -> Self {
        Self {
            title: title.into(),
            id: id.into(),
            metric_type: MetricType::Histogram,
            subtype: Some(subtype.to_string()),
            percentiles: None,
            format: FormatConfig::new(unit),
            description: None,
        }
    }

    /// Convenience: a histogram metric for latency distributions with standard
    /// defaults (log scale, 100s range).
    pub fn histogram_latency<T: Into<String>, U: Into<String>>(title: T, id: U) -> Self {
        Self::histogram(title, id, Unit::Time, "percentiles")
            .with_log_scale(true)
            .range(0.0, 100_000_000_000.0)
    }

    /// Convenience: sets the standard 0..1 range used for percentage metrics.
    pub fn percentage_range(self) -> Self {
        self.range(0.0, 1.0)
    }

    pub fn with_unit_system<T: Into<String>>(mut self, unit_system: T) -> Self {
        self.format.unit_system = Some(unit_system.into());
        self
    }

    /// Per-chart subtitle rendered below the chart title. Use when the
    /// description is specific to one chart in a subgroup; for shared
    /// context, prefer `Subgroup::describe`.
    pub fn with_description<T: Into<String>>(mut self, text: T) -> Self {
        self.description = Some(text.into());
        self
    }

    pub fn maybe_unit_system(self, unit: Option<&str>) -> Self {
        match unit {
            Some(u) => self.with_unit_system(u),
            None => self,
        }
    }

    pub fn with_percentiles(mut self, percentiles: Vec<f64>) -> Self {
        self.percentiles = Some(percentiles);
        self
    }

    pub fn with_axis_label<T: Into<String>>(mut self, y_label: T) -> Self {
        self.format.y_axis_label = Some(y_label.into());
        self
    }

    pub fn with_log_scale(mut self, log_scale: bool) -> Self {
        self.format.log_scale = Some(log_scale);
        self
    }

    pub fn range(mut self, min: f64, max: f64) -> Self {
        self.format.range = Some(Range {
            min: Some(min),
            max: Some(max),
        });
        self
    }
}

impl FormatConfig {
    pub fn new(unit: Unit) -> Self {
        Self {
            x_axis_label: None,
            y_axis_label: None,
            unit_system: Some(unit.to_string()),
            precision: Some(2),
            log_scale: None,
            range: None,
            value_label: None,
        }
    }
}

pub enum Unit {
    Count,
    Rate,
    Time,
    Bytes,
    Datarate,
    Bitrate,
    Percentage,
    Frequency,
}

impl std::fmt::Display for Unit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        let s = match self {
            Self::Count => "count",
            Self::Rate => "rate",
            Self::Time => "time",
            Self::Bytes => "bytes",
            Self::Datarate => "datarate",
            Self::Bitrate => "bitrate",
            Self::Percentage => "percentage",
            Self::Frequency => "frequency",
        };

        write!(f, "{s}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_plot(width: PlotWidth) -> Plot {
        Plot {
            data: Vec::new(),
            opts: PlotOpts::counter("t", "id", Unit::Count),
            min_value: None,
            max_value: None,
            time_data: None,
            formatted_time_data: None,
            series_names: None,
            sql_query: Some("SELECT 1 AS t, 1 AS v".into()),
            sql_query_experiment: None,
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
    fn flat_plot_sql_creates_single_unnamed_subgroup() {
        let mut g = Group::new("G", "g");
        g.plot_sql(
            PlotOpts::counter("t1", "id1", Unit::Count),
            "SELECT 1 AS t, 1 AS v".into(),
        );
        g.plot_sql(
            PlotOpts::counter("t2", "id2", Unit::Count),
            "SELECT 1 AS t, 1 AS v".into(),
        );
        let json = serde_json::to_value(&g).unwrap();
        let subs = json["subgroups"].as_array().expect("subgroups present");
        assert_eq!(subs.len(), 1, "flat calls collapse to one subgroup");
        assert!(subs[0].get("name").is_none(), "default subgroup is unnamed");
        assert_eq!(
            subs[0]["plots"].as_array().unwrap().len(),
            2,
            "both flat plots land in the default subgroup"
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
    fn plot_sql_serializes_with_sql_query_field() {
        let mut g = Group::new("G", "g");
        g.subgroup("Ops").plot_sql(
            PlotOpts::counter("Total", "total", Unit::Rate),
            "SELECT timestamp::DOUBLE/1e9 AS t, irate_1s(\"cpu_cycles/0\", timestamp) AS v FROM _src".into(),
        );
        let json = serde_json::to_value(&g).unwrap();
        let plot = &json["subgroups"][0]["plots"][0];
        assert!(plot["sql_query"].as_str().unwrap().contains("irate_1s"));
        assert!(
            plot.get("promql_query").is_none(),
            "plot JSON no longer carries promql_query"
        );
    }

    #[test]
    fn plot_sql_full_marks_plot_as_full_width() {
        let mut g = Group::new("G", "g");
        g.subgroup("Ops").plot_sql_full(
            PlotOpts::counter("Wide", "wide", Unit::Rate),
            "SELECT 1 AS t, 1 AS v".into(),
        );
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

}

#[cfg(test)]
mod plot_serialize_tests {
    use super::*;

    #[test]
    fn plot_sql_query_experiment_round_trips() {
        let mut sg = SubGroup::default();
        sg.plot_sql(
            PlotOpts::counter("X", "kpi-x", Unit::Count),
            "SELECT 1 AS t, 1 AS v".to_string(),
        );
        // Mutate the just-pushed plot to set the experiment SQL, then
        // serialize and confirm it appears in the JSON.
        let plot = sg.plots.last_mut().unwrap();
        plot.sql_query_experiment = Some("SELECT 2 AS t, 2 AS v".to_string());
        let json = serde_json::to_string(plot).unwrap();
        assert!(json.contains("\"sql_query_experiment\":\"SELECT 2 AS t, 2 AS v\""));

        // Default (None) is omitted from the JSON.
        plot.sql_query_experiment = None;
        let json = serde_json::to_string(plot).unwrap();
        assert!(!json.contains("sql_query_experiment"), "got {json}");
    }
}
