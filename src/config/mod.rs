use crate::Duration;
use ringlog::Level;
use serde::Deserialize;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::net::ToSocketAddrs;
use std::path::Path;

#[derive(Deserialize)]
pub struct Config {
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
            .map(|c| c.enabled())
            .unwrap_or(self.defaults.enabled())
    }

    pub fn bpf(&self, name: &str) -> bool {
        self.samplers
            .get(name)
            .map(|c| c.bpf())
            .unwrap_or(self.defaults.bpf())
    }

    pub fn interval(&self, name: &str) -> Duration {
        self.samplers
            .get(name)
            .map(|c| c.interval())
            .unwrap_or(self.defaults.interval())
    }

    pub fn distribution_interval(&self, name: &str) -> Duration {
        self.samplers
            .get(name)
            .map(|c| c.distribution_interval())
            .unwrap_or(self.defaults.distribution_interval())
    }
}

#[derive(Deserialize)]
pub struct General {
    listen: String,
    #[serde(default = "disabled")]
    compression: bool,
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

    pub fn compression(&self) -> bool {
        self.compression
    }
}

#[derive(Deserialize)]
pub struct Log {
    #[serde(with = "LevelDef")]
    #[serde(default = "log_level")]
    level: Level,
}

impl Default for Log {
    fn default() -> Self {
        Self { level: log_level() }
    }
}

impl Log {
    pub fn level(&self) -> Level {
        self.level
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
#[serde(remote = "Level")]
#[serde(deny_unknown_fields)]
enum LevelDef {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

fn log_level() -> Level {
    Level::Info
}

#[derive(Deserialize)]
pub struct Prometheus {
    #[serde(default = "disabled")]
    histograms: bool,
    #[serde(default = "four")]
    histogram_grouping_power: u8,
}

impl Default for Prometheus {
    fn default() -> Self {
        Self {
            histograms: false,
            histogram_grouping_power: 4,
        }
    }
}

impl Prometheus {
    pub fn check(&self) {
        if !(2..=(crate::common::HISTOGRAM_GROUPING_POWER)).contains(&self.histogram_grouping_power)
        {
            eprintln!(
                "prometheus histogram downsample factor must be in the range 2..={}",
                crate::common::HISTOGRAM_GROUPING_POWER
            );
            std::process::exit(1);
        }
    }

    pub fn histograms(&self) -> bool {
        self.histograms
    }

    pub fn histogram_grouping_power(&self) -> u8 {
        self.histogram_grouping_power
    }
}

pub fn enabled() -> bool {
    true
}

pub fn disabled() -> bool {
    false
}

pub fn four() -> u8 {
    4
}

pub fn interval() -> String {
    "10ms".into()
}

pub fn distribution_interval() -> String {
    "50ms".into()
}

#[derive(Deserialize)]
pub struct SamplerConfig {
    #[serde(default = "enabled")]
    enabled: bool,
    #[serde(default = "enabled")]
    bpf: bool,
    #[serde(default = "interval")]
    interval: String,
    #[serde(default = "distribution_interval")]
    distribution_interval: String,
}

impl Default for SamplerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            bpf: true,
            interval: interval(),
            distribution_interval: distribution_interval(),
        }
    }
}

impl SamplerConfig {
    pub fn check(&self, name: &str) {
        if let Err(e) = self.interval.parse::<humantime::Duration>() {
            eprintln!("{name} sampler interval is not valid: {e}");
            std::process::exit(1);
        }
        if self.interval() < Duration::from_millis(1) {
            eprintln!("{name} sampler interval is too short. Minimum interval is: 1ms");
            std::process::exit(1);
        }

        if let Err(e) = self.distribution_interval.parse::<humantime::Duration>() {
            eprintln!("{name} sampler distribution interval is not valid: {e}");
            std::process::exit(1);
        }

        if self.distribution_interval() < Duration::from_millis(1) {
            eprintln!(
                "{name} sampler distribution interval is too short. Minimum interval is: 1ms"
            );
            std::process::exit(1);
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn bpf(&self) -> bool {
        self.bpf
    }

    pub fn interval(&self) -> Duration {
        Duration::from_nanos(
            self.interval
                .parse::<humantime::Duration>()
                .unwrap()
                .as_nanos() as _,
        )
    }

    pub fn distribution_interval(&self) -> Duration {
        Duration::from_nanos(
            self.distribution_interval
                .parse::<humantime::Duration>()
                .unwrap()
                .as_nanos() as _,
        )
    }
}
