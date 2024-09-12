use crate::config::*;

#[derive(Deserialize)]
pub struct Prometheus {
    #[serde(default = "enabled")]
    histograms: bool,
    #[serde(default = "histogram_grouping_power")]
    histogram_grouping_power: u8,
}

impl Default for Prometheus {
    fn default() -> Self {
        Self {
            histograms: true,
            histogram_grouping_power: HISTOGRAM_GROUPING_POWER,
        }
    }
}

impl Prometheus {
    pub fn check(&self) {
        if !(0..=HISTOGRAM_GROUPING_POWER).contains(&self.histogram_grouping_power) {
            eprintln!("prometheus histogram downsample factor must be in the range 0..={HISTOGRAM_GROUPING_POWER}",);
            std::process::exit(1);
        }
    }

    pub fn histograms(&self) -> bool {
        self.histograms
    }

    pub fn histogram_grouping_power(&self) -> u8 {
        self.histogram_grouping_power
    }
}
