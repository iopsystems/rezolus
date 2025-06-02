use super::*;

#[derive(Default, Serialize)]
pub struct View {
    // interval between consecutive datapoints as fractional seconds
    interval: f64,
    source: String,
    version: String,
    filename: String,
    groups: Vec<Group>,
    sections: Vec<Section>,
}

impl View {
    pub fn new(data: &Tsdb, sections: Vec<Section>) -> Self {
        let interval = data.interval();
        let source = data.source();
        let version = data.version();
        let filename = data.filename();

        Self {
            interval,
            source,
            version,
            filename,
            groups: Vec::new(),
            sections,
        }
    }

    pub fn group(&mut self, group: Group) -> &Self {
        self.groups.push(group);
        self
    }
}

#[derive(Clone, Serialize)]
pub struct Section {
    pub(crate) name: String,
    pub(crate) route: String,
}

#[derive(Serialize)]
pub struct Group {
    name: String,
    id: String,
    plots: Vec<Plot>,
}

impl Group {
    pub fn new<T: Into<String>, U: Into<String>>(name: T, id: U) -> Self {
        Self {
            name: name.into(),
            id: id.into(),
            plots: Vec::new(),
        }
    }

    pub fn push(&mut self, plot: Option<Plot>) {
        if let Some(plot) = plot {
            self.plots.push(plot);
        }
    }

    pub fn plot(&mut self, opts: PlotOpts, series: Option<UntypedSeries>) {
        if let Some(data) = series.map(|v| v.as_data()) {
            self.plots.push(Plot {
                opts,
                data,
                min_value: None,
                max_value: None,
                time_data: None,
                formatted_time_data: None,
                series_names: None,
            })
        }
    }

    pub fn heatmap(&mut self, opts: PlotOpts, series: Option<Heatmap>) {
        if let Some(heatmap) = series {
            let echarts_data = heatmap.as_data();

            if !echarts_data.data.is_empty() {
                self.plots.push(Plot {
                    opts,
                    data: echarts_data.data,
                    min_value: Some(echarts_data.min_value),
                    max_value: Some(echarts_data.max_value),
                    time_data: Some(echarts_data.time),
                    formatted_time_data: Some(echarts_data.formatted_time),
                    series_names: None,
                })
            }
        }
    }

    pub fn scatter(&mut self, opts: PlotOpts, data: Option<Vec<UntypedSeries>>) {
        if data.is_none() {
            return;
        }

        let d = data.unwrap();

        let mut data = Vec::new();

        for series in &d {
            let d = series.as_data();

            if data.is_empty() {
                data.push(d[0].clone());
            }

            data.push(d[1].clone());
        }

        self.plots.push(Plot {
            opts,
            data,
            min_value: None,
            max_value: None,
            time_data: None,
            formatted_time_data: None,
            series_names: None,
        })
    }

    // New method to add a multi-series plot
    pub fn multi(&mut self, opts: PlotOpts, cgroup_data: Option<Vec<(String, UntypedSeries)>>) {
        if cgroup_data.is_none() {
            return;
        }

        let mut cgroup_data = cgroup_data.unwrap();

        let mut data = Vec::new();
        let mut labels = Vec::new();

        for (label, series) in cgroup_data.drain(..) {
            labels.push(label);
            let d = series.as_data();

            if data.is_empty() {
                data.push(d[0].clone());
            }

            data.push(d[1].clone());
        }

        self.plots.push(Plot {
            opts,
            data,
            min_value: None,
            max_value: None,
            time_data: None,
            formatted_time_data: None,
            series_names: Some(labels),
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
}

impl Plot {
    pub fn line<T: Into<String>, U: Into<String>>(
        title: T,
        id: U,
        unit: Unit,
        series: Option<UntypedSeries>,
    ) -> Option<Self> {
        series.map(|series| Self {
            data: series.as_data(),
            opts: PlotOpts::line(title, id, unit),
            min_value: None,
            max_value: None,
            time_data: None,
            formatted_time_data: None,
            series_names: None,
        })
    }

    pub fn heatmap<T: Into<String>, U: Into<String>>(
        title: T,
        id: U,
        unit: Unit,
        series: Option<Heatmap>,
    ) -> Option<Self> {
        if let Some(heatmap) = series {
            let echarts_data = heatmap.as_data();
            if !echarts_data.data.is_empty() {
                return Some(Plot {
                    opts: PlotOpts::heatmap(title, id, unit),
                    data: echarts_data.data,
                    min_value: Some(echarts_data.min_value),
                    max_value: Some(echarts_data.max_value),
                    time_data: Some(echarts_data.time),
                    formatted_time_data: Some(echarts_data.formatted_time),
                    series_names: None,
                });
            }
        }

        None
    }
}

#[derive(Serialize, Clone)]
pub struct PlotOpts {
    title: String,
    id: String,
    style: String,
    // Unified configuration for value formatting, axis labels, etc.
    format: Option<FormatConfig>,
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
    min: Option<f64>,        // Min value for y-axis
    max: Option<f64>,        // Max value for y-axis

    // Additional customization
    value_label: Option<String>, // Label used in tooltips for the value
}

impl PlotOpts {
    // Basic constructors without formatting
    pub fn line<T: Into<String>, U: Into<String>>(title: T, id: U, unit: Unit) -> Self {
        Self {
            title: title.into(),
            id: id.into(),
            style: "line".to_string(),
            format: Some(FormatConfig::new(unit)),
        }
    }

    pub fn multi<T: Into<String>, U: Into<String>>(title: T, id: U, unit: Unit) -> Self {
        Self {
            title: title.into(),
            id: id.into(),
            style: "multi".to_string(),
            format: Some(FormatConfig::new(unit)),
        }
    }

    pub fn scatter<T: Into<String>, U: Into<String>>(title: T, id: U, unit: Unit) -> Self {
        Self {
            title: title.into(),
            id: id.into(),
            style: "scatter".to_string(),
            format: Some(FormatConfig::new(unit)),
        }
    }

    pub fn heatmap<T: Into<String>, U: Into<String>>(title: T, id: U, unit: Unit) -> Self {
        Self {
            title: title.into(),
            id: id.into(),
            style: "heatmap".to_string(),
            format: Some(FormatConfig::new(unit)),
        }
    }

    // Convenience methods
    pub fn with_unit_system<T: Into<String>>(mut self, unit_system: T) -> Self {
        if let Some(ref mut format) = self.format {
            format.unit_system = Some(unit_system.into());
        }

        self
    }

    pub fn with_axis_label<T: Into<String>>(mut self, y_label: T) -> Self {
        if let Some(ref mut format) = self.format {
            format.y_axis_label = Some(y_label.into());
        }

        self
    }

    pub fn with_log_scale(mut self, log_scale: bool) -> Self {
        if let Some(ref mut format) = self.format {
            format.log_scale = Some(log_scale);
        }

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
            min: None,
            max: None,
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
