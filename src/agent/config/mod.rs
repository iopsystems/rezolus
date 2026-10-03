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

/// An old sampler, the part of the merged sampler it became, and whether it
/// had a per-cgroup path (so its `cgroup_attribution` carries over).
type MergedPart = (&'static str, &'static str, bool);

/// Samplers that were merged into one, so that each kernel hook carries one
/// Rezolus program (docs/journal/2026-10-03-one-program-per-hook.md): the new
/// sampler and its old ones.
const MERGED_SAMPLERS: &[(&str, &[MergedPart])] = &[
    (
        "syscall",
        &[
            ("syscall_counts", "counts", true),
            ("syscall_latency", "latency", false),
        ],
    ),
    (
        "blockio",
        &[
            ("blockio_requests", "requests", false),
            ("blockio_latency", "latency", false),
        ],
    ),
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

        let mut config: Config = toml::from_str(&content)
            .map_err(|e| {
                eprintln!("failed to parse config file: {e}");
                std::process::exit(1);
            })
            .unwrap();

        for warning in config.translate_merged_samplers() {
            eprintln!("config: {warning}");
        }
        for name in config.samplers.keys() {
            if !crate::analysis::extract::context::EXPECTED_SUBSYSTEMS.contains(&name.as_str()) {
                eprintln!("config: [samplers.{name}] names no known sampler and is ignored");
            }
        }

        config.general.check();
        config.scheduler.check();
        config.external_metrics.check();

        config.defaults.check("default");

        for (name, config) in config.samplers.iter() {
            config.check(name);
        }

        Ok(config)
    }

    /// Rewrite the sections of samplers that were merged into one as the
    /// merged sampler's section, so an old config keeps its meaning, and
    /// return a warning for each section rewritten or ignored.
    ///
    /// Each part's switch is the old sampler's resolved `enabled` (its own
    /// section, else `[defaults]`, else on), so an old section that is absent
    /// neither adds nor drops a part. The merged sampler is enabled when any
    /// part is. The `cgroup_attribution` of an old sampler that had a
    /// per-cgroup path becomes the merged section's; on one that had none it
    /// did nothing, and is reported and dropped. A section for the merged sampler wins over old ones, which
    /// are then reported as ignored.
    fn translate_merged_samplers(&mut self) -> Vec<String> {
        let mut warnings = Vec::new();
        for (merged, parts) in MERGED_SAMPLERS {
            if !parts
                .iter()
                .any(|(old, _, _)| self.samplers.contains_key(*old))
            {
                continue;
            }
            if self.samplers.contains_key(*merged) {
                for (old, _, _) in parts.iter() {
                    if self.samplers.remove(*old).is_some() {
                        warnings.push(format!(
                            "[samplers.{old}] is ignored: the sampler is now part of \
                             [samplers.{merged}], which this config also sets"
                        ));
                    }
                }
                continue;
            }
            let default_on = self.defaults.enabled().unwrap_or(enabled());
            let mut section = SamplerConfig::default();
            let mut any_on = false;
            for (old, part, attributes) in parts.iter() {
                let old_section = self.samplers.remove(*old);
                let on = old_section
                    .as_ref()
                    .and_then(|s| s.enabled())
                    .unwrap_or(default_on);
                section.set_part(part, on);
                any_on |= on;
                if let Some(old_section) = old_section {
                    if let Some(attribution) = old_section.cgroup_attribution() {
                        if *attributes {
                            section.set_cgroup_attribution(attribution);
                        } else {
                            warnings.push(format!(
                                "[samplers.{old}] cgroup_attribution is ignored: {old} had no \
                                 per-cgroup series"
                            ));
                        }
                    }
                    warnings.push(format!(
                        "[samplers.{old}] is deprecated: the sampler is now part `{part}` of \
                         [samplers.{merged}] (read as {part} = {on})"
                    ));
                }
            }
            section.set_enabled(any_on);
            self.samplers.insert(merged.to_string(), section);
        }
        warnings
    }

    /// Whether `part` of the merged sampler `name` is on: its section's
    /// switch, else on. The sampler's own `enabled` gates every part.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn part(&self, name: &str, part: &str) -> bool {
        self.samplers
            .get(name)
            .and_then(|v| v.part(part))
            .unwrap_or(true)
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
    /// off). The request-path samplers read it this way (`ext4_ops`,
    /// `xfs_log`, `memory_pagecache`): with it off, the `cgroup_*` series are
    /// absent and the path is not in the loaded program. The samplers whose
    /// per-cgroup series predate the option (`cpu_usage`, `cpu_migrations`,
    /// `cpu_perf`, `cpu_tlb_flush`, `scheduler_runqueue`, `syscall`)
    /// default it on; see `cgroup_attribution_or`.
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

    /// An old section keeps its meaning: each part is the old sampler's
    /// resolved enable, the merged sampler is on when any part is, and an
    /// old `cgroup_attribution` carries over.
    #[test]
    fn old_sections_of_a_merged_sampler_are_translated() {
        fn translated(toml: &str) -> (Config, Vec<String>) {
            let mut c = config(toml);
            let w = c.translate_merged_samplers();
            (c, w)
        }

        // No old section: nothing changes, and every part is on.
        let (c, w) = translated("");
        assert!(w.is_empty());
        assert!(c.enabled("syscall"));
        assert!(c.part("syscall", "counts") && c.part("syscall", "latency"));

        // One part switched off.
        let (c, w) = translated("[samplers.syscall_latency]\nenabled = false\n");
        assert_eq!(w.len(), 1);
        assert!(c.enabled("syscall"));
        assert!(c.part("syscall", "counts"));
        assert!(!c.part("syscall", "latency"));

        // Both off: the merged sampler is off.
        let (c, _) = translated(
            "[samplers.syscall_counts]\nenabled = false\n[samplers.syscall_latency]\nenabled = false\n",
        );
        assert!(!c.enabled("syscall"));

        // [defaults] off, one old sampler on: only that part, and an absent
        // old section follows [defaults] rather than turning its part on.
        let (c, _) =
            translated("[defaults]\nenabled = false\n[samplers.syscall_counts]\nenabled = true\n");
        assert!(c.enabled("syscall"));
        assert!(c.part("syscall", "counts"));
        assert!(!c.part("syscall", "latency"));

        // cgroup_attribution carries over from the part that had a cgroup
        // path, and only from that one.
        let (c, _) = translated("[samplers.syscall_counts]\ncgroup_attribution = false\n");
        assert!(!c.cgroup_attribution_or("syscall", true));
        let (c, w) = translated("[samplers.syscall_latency]\ncgroup_attribution = false\n");
        assert!(c.cgroup_attribution_or("syscall", true));
        assert!(w
            .iter()
            .any(|w| w.contains("cgroup_attribution is ignored")));

        // A section for the merged sampler wins; the old one is reported.
        let (c, w) = translated(
            "[samplers.syscall]\nlatency = true\n[samplers.syscall_latency]\nenabled = false\n",
        );
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("ignored"));
        assert!(c.part("syscall", "latency"));
    }

    #[test]
    fn old_blockio_sections_are_translated() {
        let mut c = config("[samplers.blockio_latency]\nenabled = false\n");
        let w = c.translate_merged_samplers();
        assert_eq!(w.len(), 1);
        assert!(c.enabled("blockio"));
        assert!(c.part("blockio", "requests"));
        assert!(!c.part("blockio", "latency"));
    }

    /// The config the packages install (`config/agent.toml`, which the deb
    /// and the rpm put at `/etc/rezolus/agent.toml`) documents the defaults
    /// and must not change them: every sampler is enabled, attributed and
    /// paced the same with it as with an empty config.
    #[test]
    fn the_packaged_config_changes_no_default() {
        let packaged = config(include_str!("../../../config/agent.toml"));
        let bare = config("");
        for &name in crate::analysis::extract::context::EXPECTED_SUBSYSTEMS {
            assert_eq!(
                packaged.enabled(name),
                bare.enabled(name),
                "{name}: enabled"
            );
            assert_eq!(
                packaged.cgroup_attribution(name),
                bare.cgroup_attribution(name),
                "{name}: cgroup_attribution"
            );
            assert_eq!(
                packaged.cgroup_attribution_or(name, true),
                bare.cgroup_attribution_or(name, true),
                "{name}: cgroup_attribution for a default-on sampler"
            );
            assert_eq!(
                packaged.task_attribution(name),
                bare.task_attribution(name),
                "{name}: task_attribution"
            );
            assert_eq!(
                packaged.sampler_interval(name),
                bare.sampler_interval(name),
                "{name}: interval"
            );
            assert_eq!(
                packaged.gpu_perf_level(name),
                bare.gpu_perf_level(name),
                "{name}: gpu_perf_level"
            );
        }
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
