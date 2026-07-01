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
const OPT_IN_SAMPLERS: &[&str] = &["gpu_amd_pmu"];

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

    fn config(toml: &str) -> Config {
        toml::from_str(toml).expect("valid config")
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
}
