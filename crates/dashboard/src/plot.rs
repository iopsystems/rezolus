use metriken_query::Tsdb;
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
    pub fn new(data: &Tsdb, sections: Vec<Section>) -> Self {
        let interval = data.interval();
        let source = data.source().to_string();
        let version = data.version().to_string();
        let filename = data.filename().to_string();

        // Compute time bounds as epoch milliseconds
        let (start_time, end_time) = match data.time_range() {
            Some((min, max)) => (Some(min as f64 / 1e6), Some(max as f64 / 1e6)),
            None => (None, None),
        };

        // Count total time series (each metric x label combination)
        let num_series = {
            let mut count = 0usize;
            for name in data.counter_names() {
                count += data.counter_labels(name).map_or(0, |l| l.len());
            }
            for name in data.gauge_names() {
                count += data.gauge_labels(name).map_or(0, |l| l.len());
            }
            for name in data.histogram_names() {
                count += data.histogram_labels(name).map_or(0, |l| l.len());
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
    pub fn plot_promql(&mut self, opts: PlotOpts, promql_query: String) {
        self.tail_subgroup_mut().plot_promql(opts, promql_query);
    }

    /// Legacy: append a plot with description-autofill support.
    pub fn plot_promql_with_descriptions(
        &mut self,
        opts: PlotOpts,
        promql_query: String,
        descriptions: Option<&HashMap<String, String>>,
    ) {
        self.tail_subgroup_mut()
            .plot_promql_with_descriptions(opts, promql_query, descriptions);
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
    pub fn plot_promql(&mut self, opts: PlotOpts, promql_query: String) {
        self.plot_promql_with_descriptions(opts, promql_query, None);
    }

    /// Mutable access to the most recently pushed plot. Used by callers
    /// that mutate per-plot fields (e.g. `promql_query_experiment` on the
    /// bridge generator) right after `plot_promql*`.
    pub fn plots_mut_last(&mut self) -> Option<&mut Plot> {
        self.plots.last_mut()
    }

    pub fn plot_promql_with_descriptions(
        &mut self,
        mut opts: PlotOpts,
        promql_query: String,
        descriptions: Option<&HashMap<String, String>>,
    ) {
        if opts.description.is_none()
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
            promql_query: Some(promql_query),
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
    pub fn plot_promql_full(&mut self, opts: PlotOpts, promql_query: String) {
        self.plot_promql_full_with_descriptions(opts, promql_query, None);
    }

    /// Full-width variant with description autofill.
    pub fn plot_promql_full_with_descriptions(
        &mut self,
        opts: PlotOpts,
        promql_query: String,
        descriptions: Option<&HashMap<String, String>>,
    ) {
        self.plot_promql_with_descriptions(opts, promql_query, descriptions);
        if let Some(plot) = self.plots.last_mut() {
            plot.width = PlotWidth::Full;
        }
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
    #[serde(skip_serializing_if = "Option::is_none")]
    promql_query: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub promql_query_experiment: Option<String>,
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
    // Axis labels
    x_axis_label: Option<String>,
    y_axis_label: Option<String>,

    // Value formatting
    unit_system: Option<String>, // e.g., "percentage", "time", "bitrate"
    precision: Option<u8>,       // Number of decimal places

    // Scale configuration
    log_scale: Option<bool>, // Whether to use log scale for y-axis

    // Expected data range -- values outside are clamped at render time
    #[serde(skip_serializing_if = "Option::is_none")]
    range: Option<Range>,

    // Additional customization
    value_label: Option<String>, // Label used in tooltips for the value
}

impl PlotOpts {
    // Constructors based on metric type

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
    /// - "percentiles": shows percentile scatter plot, wraps query with histogram_percentiles()
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

    // Builder methods
    pub fn with_unit_system<T: Into<String>>(mut self, unit_system: T) -> Self {
        self.format.unit_system = Some(unit_system.into());
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
            promql_query: Some("up".into()),
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
        g.plot_promql(PlotOpts::counter("t1", "id1", Unit::Count), "up".into());
        g.plot_promql(PlotOpts::counter("t2", "id2", Unit::Count), "up".into());
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
        sg.plot_promql_full(
            PlotOpts::counter("Summary", "sum", Unit::Count),
            "up".into(),
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
    fn plot_promql_query_experiment_round_trips() {
        let mut sg = SubGroup::default();
        sg.plot_promql(
            PlotOpts::counter("X", "kpi-x", Unit::Count),
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
