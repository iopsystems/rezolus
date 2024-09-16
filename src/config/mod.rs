use crate::common::HISTOGRAM_GROUPING_POWER;

use ringlog::Level;
use serde::Deserialize;

use std::collections::HashMap;
use std::net::{SocketAddr, ToSocketAddrs};
use std::path::Path;

mod general;
mod log;
mod prometheus;
mod sampler;

use general::General;
use log::Log;
use prometheus::Prometheus;
use sampler::Sampler as SamplerConfig;

fn enabled() -> bool {
    true
}

fn histogram_grouping_power() -> u8 {
    HISTOGRAM_GROUPING_POWER
}

fn listen() -> String {
    "0.0.0.0:4242".into()
}

#[derive(Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    general: General,
    #[serde(default)]
    log: Log,
    #[serde(default)]
    prometheus: Prometheus,
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

        config.prometheus().check();

        config.defaults.check("default");

        for (name, config) in config.samplers.iter() {
            config.check(name);
        }

        Ok(config)
    }

    pub fn log(&self) -> &Log {
        &self.log
    }

    pub fn defaults(&self) -> &SamplerConfig {
        &self.defaults
    }

    pub fn sampler_config(&self, name: &str) -> Option<&SamplerConfig> {
        self.samplers.get(name)
    }

    pub fn general(&self) -> &General {
        &self.general
    }

    pub fn prometheus(&self) -> &Prometheus {
        &self.prometheus
    }

    pub fn enabled(&self, name: &str) -> bool {
        self.samplers
            .get(name)
            .and_then(|v| v.enabled())
            .unwrap_or(self.defaults.enabled().unwrap_or(enabled()))
    }
}
