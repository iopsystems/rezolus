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
}

impl Sampler {
    pub fn enabled(&self) -> Option<bool> {
        self.enabled
    }

    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn gpu_perf_level(&self) -> Option<&str> {
        self.gpu_perf_level.as_deref()
    }

    pub fn check(&self, _name: &str) {}
}
