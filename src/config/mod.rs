use crate::Duration;
use serde::Deserialize;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::net::ToSocketAddrs;
use std::path::Path;

#[derive(Deserialize)]
// #[serde(deny_unknown_fields)]
pub struct Config {
    general: General,
    sampler_defaults: SamplerDefaults,
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

        toml::from_str(&content).map_err(|e| {
            eprintln!("failed to parse config file: {e}");
            std::process::exit(1);
        })
    }

    pub fn sampler_defaults(&self) -> &SamplerDefaults {
        &self.sampler_defaults
    }

    pub fn sampler_config(&self, name: &str) -> Option<&SamplerConfig> {
        self.samplers.get(name)
    }

    pub fn general(&self) -> &General {
        &self.general
    }

    #[cfg(feature = "bpf")]
    pub fn bpf(&self) -> bool {
        true
    }

    #[cfg(not(feature = "bpf"))]
    pub fn bpf(&self) -> bool {
        false
    }

    pub fn enabled(&self, name: &str) -> bool {
        self.samplers
            .get(name)
            .map(|c| c.enabled())
            .unwrap_or(self.sampler_defaults.enabled())
    }

    pub fn interval(&self, name: &str) -> Duration {
        self.samplers
            .get(name)
            .map(|c| c.interval())
            .unwrap_or(self.sampler_defaults.interval())
    }

    pub fn distribution_interval(&self, name: &str) -> Duration {
        self.samplers
            .get(name)
            .map(|c| c.distribution_interval())
            .unwrap_or(self.sampler_defaults.distribution_interval())
    }
}

#[derive(Deserialize)]
pub struct General {
    listen: String,
}

impl General {
    pub fn listen(&self) -> SocketAddr {
        self.listen
            .to_socket_addrs()
            .map_err(|e| {
                eprintln!("bad listen address: {e}");
                std::process::exit(1);
            })
            .unwrap()
            .next()
            .ok_or_else(|| {
                eprintln!("could not resolve socket addr");
                std::process::exit(1);
            })
            .unwrap()
    }
}

pub fn enabled() -> bool {
    true
}

pub fn interval() -> core::time::Duration {
    core::time::Duration::from_millis(10)
}

pub fn distribution_interval() -> core::time::Duration {
    core::time::Duration::from_millis(50)
}

#[derive(Deserialize)]
pub struct SamplerDefaults {
    #[serde(default = "enabled")]
    enabled: bool,
    #[serde(default = "interval")]
    interval: core::time::Duration,
    #[serde(default = "distribution_interval")]
    distribution_interval: core::time::Duration,
}

impl SamplerDefaults {
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn interval(&self) -> Duration {
        Duration::from_nanos(self.interval.as_nanos() as _)
    }

    pub fn distribution_interval(&self) -> Duration {
        Duration::from_nanos(self.distribution_interval.as_nanos() as _)
    }
}

#[derive(Deserialize)]
pub struct SamplerConfig {
    enabled: bool,
    #[serde(default = "interval")]
    interval: core::time::Duration,
    #[serde(default = "distribution_interval")]
    distribution_interval: core::time::Duration,
}

impl SamplerConfig {
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn interval(&self) -> Duration {
        Duration::from_nanos(self.interval.as_nanos() as _)
    }

    pub fn distribution_interval(&self) -> Duration {
        Duration::from_nanos(self.distribution_interval.as_nanos() as _)
    }
}
