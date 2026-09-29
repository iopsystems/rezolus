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
    /// Whether a sampler that can attribute its events to the calling
    /// thread's cgroup does so (`ext4_ops`, `xfs_log`). Off by default: the
    /// serial check and two atomics it adds to a request-path hook were
    /// measured at half the hook's cost. `None` means the default.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    #[serde(default)]
    cgroup_attribution: Option<bool>,
    /// Whether a sampler that keeps per-task accounting also exports it as
    /// per-task series (`cpu_usage`'s `task_cpu_usage`). Off by default: the
    /// accounting stays (host and cgroup totals are computed from it), and
    /// what is dropped is the export — a task-metadata event per new task,
    /// the walk of the per-pid map's populated slots each time a snapshot is
    /// served, and one series per thread in every recording. `None` means
    /// the default.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    #[serde(default)]
    task_attribution: Option<bool>,
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

    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn cgroup_attribution(&self) -> Option<bool> {
        self.cgroup_attribution
    }

    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn task_attribution(&self) -> Option<bool> {
        self.task_attribution
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
