use super::*;

#[derive(Deserialize, Default)]
pub struct Sampler {
    #[serde(default)]
    enabled: Option<bool>,
    /// AMD GPU performance level to set for this sampler (only meaningful for
    /// `gpu_amd_pmu`, which is Linux-only). `None` means leave the GPU power
    /// state untouched.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    #[serde(default)]
    gpu_perf_level: Option<String>,
    /// Read interval for samplers that poll a slow, expensive source on their
    /// own cadence rather than on the scrape/TTL sample cycle (currently
    /// `drivehealth`). A humantime string (e.g. `"60s"`). `None` means use the
    /// sampler's built-in default.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    #[serde(default)]
    interval: Option<String>,
}

impl Sampler {
    pub fn enabled(&self) -> Option<bool> {
        self.enabled
    }

    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn gpu_perf_level(&self) -> Option<&str> {
        self.gpu_perf_level.as_deref()
    }

    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn interval(&self) -> Option<&str> {
        self.interval.as_deref()
    }

    pub fn check(&self, name: &str) {
        if let Some(ref interval) = self.interval {
            if let Err(e) = interval.parse::<humantime::Duration>() {
                eprintln!("sampler '{name}' interval couldn't be parsed: {e}");
                std::process::exit(1);
            }
        }
    }
}
