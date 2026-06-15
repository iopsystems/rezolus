use super::*;

#[derive(Deserialize, Default)]
pub struct Sampler {
    #[serde(default)]
    enabled: Option<bool>,
    /// Optional per-sampler collection window (humantime string, e.g. "500ms",
    /// "1s"). Only consulted by samplers that perform interval-based collection
    /// (currently `gpu_amd_pmu`); ignored by the rest.
    #[serde(default)]
    sampling_window: Option<String>,
}

impl Sampler {
    pub fn enabled(&self) -> Option<bool> {
        self.enabled
    }

    /// The configured sampling window, if set and parseable.
    pub fn sampling_window(&self) -> Option<std::time::Duration> {
        self.sampling_window
            .as_ref()
            .and_then(|s| s.parse::<humantime::Duration>().ok())
            .map(|d| *d)
    }

    pub fn check(&self, name: &str) {
        if let Some(s) = &self.sampling_window {
            if let Err(e) = s.parse::<humantime::Duration>() {
                eprintln!("{name}: invalid sampling_window '{s}': {e}");
                std::process::exit(1);
            }
        }
    }
}
