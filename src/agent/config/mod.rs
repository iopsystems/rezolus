use crate::debug;

use serde::Deserialize;

use std::collections::HashMap;
use std::net::{SocketAddr, ToSocketAddrs};
use std::path::Path;

mod external_metrics;
mod general;
mod log;
mod sampler;
mod scheduler;

use external_metrics::ExternalMetrics;
use general::General;
pub use general::SnapshotFormat;
use log::Log;
use sampler::Sampler as SamplerConfig;
use scheduler::Scheduler;

fn enabled() -> bool {
    true
}

/// Samplers that are never enabled by the `[defaults]` fallback — they must be
/// explicitly opted into with `enabled = true` in their own `[samplers.<name>]`
/// section. Reserved for samplers whose cost makes accidental activation
/// (e.g. via an absent/commented config) unacceptable.
const OPT_IN_SAMPLERS: &[&str] = &[
    "ext4_ops",
    "gpu_amd_pmu",
    "hw_sensors",
    "memory_pagecache",
    "xfs_log",
];

fn listen() -> String {
    "0.0.0.0:4241".into()
}

fn ttl() -> String {
    "10ms".into()
}

#[derive(Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    general: General,
    #[serde(default)]
    scheduler: Scheduler,
    #[serde(default)]
    log: Log,
    #[serde(default)]
    external_metrics: ExternalMetrics,
    #[serde(default)]
    defaults: SamplerConfig,
    #[serde(default)]
    samplers: HashMap<String, SamplerConfig>,
}

impl Config {
    pub fn load(path: &dyn AsRef<Path>) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| {
                eprintln!("unable to open config file: {e}");
                std::process::exit(1);
            })
            .unwrap();

        let config: Config = toml::from_str(&content)
            .map_err(|e| {
                eprintln!("failed to parse config file: {e}");
                std::process::exit(1);
            })
            .unwrap();

        config.general.check();
        config.scheduler.check();
        config.external_metrics.check();

        config.defaults.check("default");

        for (name, config) in config.samplers.iter() {
            config.check(name);
        }

        Ok(config)
    }

    pub fn log(&self) -> &Log {
        &self.log
    }

    pub fn general(&self) -> &General {
        &self.general
    }

    #[cfg(target_os = "linux")]
    pub fn scheduler(&self) -> &Scheduler {
        &self.scheduler
    }

    pub fn external_metrics(&self) -> &ExternalMetrics {
        &self.external_metrics
    }

    /// The configured AMD GPU performance level for `name`, falling back to the
    /// `defaults` section. `None` means leave the GPU power state untouched.
    /// Only consumed by the Linux-only `gpu_amd_pmu` sampler.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn gpu_perf_level(&self, name: &str) -> Option<String> {
        self.samplers
            .get(name)
            .and_then(|v| v.gpu_perf_level())
            .or_else(|| self.defaults.gpu_perf_level())
            .map(|s| s.to_string())
    }

    /// The configured read interval for `name` (per-sampler override, falling
    /// back to the `defaults` section). `None` if unset anywhere, in which case
    /// the sampler applies its own built-in default. Consumed by samplers that
    /// read cost-bearing sources off the sample cycle (`drivehealth`,
    /// `filesystem`, GPU PMUs and `hw_sensors`).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn sampler_interval(&self, name: &str) -> Option<std::time::Duration> {
        self.samplers
            .get(name)
            .and_then(|v| v.interval())
            .or_else(|| self.defaults.interval())
            .and_then(|s| s.parse::<humantime::Duration>().ok())
            .map(|d| *d)
    }

    /// Whether `name` attributes its events to the calling thread's cgroup
    /// (per-sampler override, falling back to the `defaults` section, then
    /// off). Consumed by the samplers whose per-cgroup path is a measured
    /// share of their probe cost (`ext4_ops`, `xfs_log`, `memory_pagecache`):
    /// with it off, the `cgroup_*` series are absent and the path is not in
    /// the loaded program. `cpu_perf` defaults it on; see
    /// `cgroup_attribution_or`.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn cgroup_attribution(&self, name: &str) -> bool {
        self.cgroup_attribution_or(name, false)
    }

    /// `cgroup_attribution` for a sampler whose own default is `default`
    /// when neither its section nor `[defaults]` says: `cpu_perf` keeps its
    /// per-cgroup series on unless asked, since the cgroups dashboard's IPC
    /// comes from them.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn cgroup_attribution_or(&self, name: &str, default: bool) -> bool {
        self.samplers
            .get(name)
            .and_then(|v| v.cgroup_attribution())
            .or_else(|| self.defaults.cgroup_attribution())
            .unwrap_or(default)
    }

    /// Whether `name` exports its per-task accounting as per-task series
    /// (per-sampler override, falling back to the `defaults` section, then
    /// off). Consumed by `cpu_usage`: with it off, `task_cpu_usage` is absent
    /// and the task-metadata and task-exit events are not sent, while the
    /// per-task accounting the host and cgroup totals rely on still runs.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn task_attribution(&self, name: &str) -> bool {
        self.samplers
            .get(name)
            .and_then(|v| v.task_attribution())
            .or_else(|| self.defaults.task_attribution())
            .unwrap_or(false)
    }

    pub fn enabled(&self, name: &str) -> bool {
        // Opt-in-only samplers are never turned on by the `[defaults]` fallback:
        // they require explicit `enabled = true` in their own section. These are
        // costly/privileged enough that an absent or commented-out config must
        // not accidentally enable them (e.g. `gpu_amd_pmu` needs CAP_PERFMON,
        // takes an exclusive per-GPU profiling lock, and spins an HSA thread at
        // ~100% of one core).
        let enabled = if OPT_IN_SAMPLERS.contains(&name) {
            self.samplers
                .get(name)
                .and_then(|v| v.enabled())
                .unwrap_or(false)
        } else {
            self.samplers
                .get(name)
                .and_then(|v| v.enabled())
                .unwrap_or(self.defaults.enabled().unwrap_or(enabled()))
        };

        if enabled {
            debug!("'{name}' sampler is enabled");
        } else {
            debug!("'{name}' sampler is not enabled");
        }

        enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn config(toml: &str) -> Config {
        toml::from_str(toml).expect("valid config")
    }

    #[test]
    fn cgroup_attribution_is_off_unless_asked_for() {
        let c = config("[samplers.ext4_ops]\nenabled = true\n");
        assert!(!c.cgroup_attribution("ext4_ops"), "off by default");
        let c = config("[samplers.ext4_ops]\nenabled = true\ncgroup_attribution = true\n");
        assert!(c.cgroup_attribution("ext4_ops"));
        assert!(!c.cgroup_attribution("xfs_log"), "per sampler");
        // The defaults section is a fallback, and a sampler can opt back out.
        let c = config(
            "[defaults]\ncgroup_attribution = true\n[samplers.xfs_log]\ncgroup_attribution = false\n",
        );
        assert!(c.cgroup_attribution("ext4_ops"));
        assert!(!c.cgroup_attribution("xfs_log"));
    }

    #[test]
    fn a_sampler_can_default_cgroup_attribution_on() {
        let c = config("[samplers.cpu_perf]\n");
        assert!(c.cgroup_attribution_or("cpu_perf", true), "its own default");
        let c = config("[samplers.cpu_perf]\ncgroup_attribution = false\n");
        assert!(!c.cgroup_attribution_or("cpu_perf", true), "section wins");
        let c = config("[defaults]\ncgroup_attribution = false\n");
        assert!(
            !c.cgroup_attribution_or("cpu_perf", true),
            "defaults win over the sampler's own"
        );
    }

    #[test]
    fn task_attribution_is_off_unless_asked_for() {
        let c = config("[samplers.cpu_usage]\n");
        assert!(!c.task_attribution("cpu_usage"), "off by default");
        let c = config("[samplers.cpu_usage]\ntask_attribution = true\n");
        assert!(c.task_attribution("cpu_usage"));
        let c = config("[defaults]\ntask_attribution = true\n");
        assert!(c.task_attribution("cpu_usage"), "defaults fallback");
        // Independent of cgroup attribution.
        assert!(!c.cgroup_attribution("cpu_usage"));
    }

    #[test]
    fn opt_in_sampler_off_when_section_absent() {
        // [defaults] enabled = true, but the opt-in sampler has no section.
        let c = config("[defaults]\nenabled = true\n");
        assert!(
            !c.enabled("gpu_amd_pmu"),
            "opt-in sampler must not follow defaults=true"
        );
        // A normal sampler does follow defaults=true.
        assert!(c.enabled("cpu_usage"));
        assert!(!c.enabled("hw_sensors"));
        // The request-path ext4 sampler is opt-in for its probe cost.
        assert!(!c.enabled("ext4_ops"));
    }

    #[test]
    fn opt_in_sampler_off_when_section_present_without_enabled() {
        // Section present but no explicit `enabled` -> still off.
        let c = config("[defaults]\nenabled = true\n\n[samplers.gpu_amd_pmu]\n");
        assert!(!c.enabled("gpu_amd_pmu"));
    }

    #[test]
    fn opt_in_sampler_on_only_when_explicitly_enabled() {
        let c = config("[defaults]\nenabled = true\n\n[samplers.gpu_amd_pmu]\nenabled = true\n");
        assert!(c.enabled("gpu_amd_pmu"));
        let c = config("[samplers.hw_sensors]\nenabled = true\ninterval = \"7s\"\n");
        assert!(c.enabled("hw_sensors"));
        assert_eq!(
            c.sampler_interval("hw_sensors"),
            Some(Duration::from_secs(7))
        );
    }

    #[test]
    fn opt_in_sampler_off_when_explicitly_disabled() {
        let c = config("[samplers.gpu_amd_pmu]\nenabled = false\n");
        assert!(!c.enabled("gpu_amd_pmu"));
    }

    #[test]
    fn defaults_false_disables_normal_samplers() {
        let c = config("[defaults]\nenabled = false\n");
        assert!(!c.enabled("cpu_usage"));
        // Opt-in sampler is still off too.
        assert!(!c.enabled("gpu_amd_pmu"));
    }

    #[test]
    fn sampler_interval_override_defaults_and_absent() {
        // Per-sampler override wins.
        let c = config("[samplers.drivehealth]\ninterval = \"30s\"\n");
        assert_eq!(
            c.sampler_interval("drivehealth"),
            Some(Duration::from_secs(30))
        );

        // Falls back to the [defaults] section.
        let c = config("[defaults]\ninterval = \"90s\"\n");
        assert_eq!(
            c.sampler_interval("drivehealth"),
            Some(Duration::from_secs(90))
        );

        // Unset anywhere -> None (sampler applies its own default).
        let c = config("[defaults]\nenabled = true\n");
        assert_eq!(c.sampler_interval("drivehealth"), None);
    }
}
