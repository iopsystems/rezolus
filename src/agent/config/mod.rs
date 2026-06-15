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

    pub fn enabled(&self, name: &str) -> bool {
        let enabled = self
            .samplers
            .get(name)
            .and_then(|v| v.enabled())
            .unwrap_or(self.defaults.enabled().unwrap_or(enabled()));

        if enabled {
            debug!("'{name}' sampler is enabled");
        } else {
            debug!("'{name}' sampler is not enabled");
        }

        enabled
    }

    /// The configured sampling window for a sampler, falling back to the
    /// `defaults` section, then to `default` if neither is set. Only meaningful
    /// for samplers that perform interval-based collection.
    pub fn sampling_window(&self, name: &str, default: std::time::Duration) -> std::time::Duration {
        self.samplers
            .get(name)
            .and_then(|v| v.sampling_window())
            .or_else(|| self.defaults.sampling_window())
            .unwrap_or(default)
    }
}
