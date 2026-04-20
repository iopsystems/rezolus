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

#[derive(Serialize)]
pub struct Group {
    name: String,
    id: String,
    plots: Vec<Plot>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    #[serde(default)]
    pub metadata: HashMap<String, serde_json::Value>,
}

impl Group {
    pub fn new<T: Into<String>, U: Into<String>>(name: T, id: U) -> Self {
        Self {
            name: name.into(),
            id: id.into(),
            plots: Vec::new(),
            metadata: HashMap::new(),
        }
    }

    pub fn plot_promql(&mut self, opts: PlotOpts, promql_query: String) {
        self.plot_promql_with_descriptions(opts, promql_query, None);
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
            width: PlotWidth::default(),
        });
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
}
