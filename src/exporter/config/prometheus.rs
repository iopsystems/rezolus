use super::*;

#[derive(Deserialize)]
pub struct Prometheus {
    // control whether full histograms should be exported and what grouping
    // power to export them at
    #[serde(default = "disabled")]
    histograms: bool,
    #[serde(default = "histogram_grouping_power")]
    histogram_grouping_power: u8,

    // by sampling periodically, the exporter can produce summaries from
    // histograms, these control the export of summaries and their percentiles
    #[serde(default = "enabled")]
    summaries: bool,
}

impl Default for Prometheus {
    fn default() -> Self {
        Self {
            histograms: disabled(),
            histogram_grouping_power: HISTOGRAM_GROUPING_POWER,
            summaries: enabled(),
        }
    }
}

impl Prometheus {
    pub fn check(&self) {
        if !(0..=HISTOGRAM_GROUPING_POWER).contains(&self.histogram_grouping_power) {
            eprintln!("prometheus histogram downsample factor must be in the range 0..={HISTOGRAM_GROUPING_POWER}");
            std::process::exit(1);
        }
    }

    pub fn histograms(&self) -> bool {
        self.histograms
    }

    pub fn histogram_grouping_power(&self) -> u8 {
        self.histogram_grouping_power
    }

    pub fn summaries(&self) -> bool {
        self.summaries
    }
}
