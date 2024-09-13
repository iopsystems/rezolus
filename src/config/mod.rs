use crate::common::AsyncInterval;
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
            .and_then(|v| v.enabled)
            .unwrap_or(self.defaults.enabled.unwrap_or(enabled()))
    }

    pub fn interval(&self, name: &str) -> Duration {
        let interval = self
            .samplers
            .get(name)
            .and_then(|v| v.interval.as_ref())
            .unwrap_or(self.defaults.interval.as_ref().unwrap_or(&interval()))
            .parse::<humantime::Duration>()
            .unwrap();

        Duration::from_nanos(interval.as_nanos() as u64)
    }

    pub fn async_interval(&self, name: &str) -> AsyncInterval {
        let interval = self
            .samplers
            .get(name)
            .and_then(|v| v.interval.as_ref())
            .unwrap_or(self.defaults.interval.as_ref().unwrap_or(&interval()))
            .parse::<humantime::Duration>()
            .unwrap();

        AsyncInterval::new(Duration::from_nanos(interval.as_nanos() as u64))
    }
}

#[derive(Deserialize)]
pub struct General {
    listen: String,
    #[serde(default = "disabled")]
    compression: bool,
    #[serde(default = "snapshot_interval")]
    snapshot_interval: String,
}

impl General {
    fn check(&self) {
        match self.snapshot_interval.parse::<humantime::Duration>() {
            Err(e) => {
                eprintln!("snapshot interval is not valid: {e}");
                std::process::exit(1);
            }
            Ok(interval) => {
                if Duration::from_nanos(interval.as_nanos() as u64) < Duration::from_millis(100) {
                    eprintln!("snapshot interval is too short. Minimum interval is: 100ms");
                    std::process::exit(1);
                }
            }
        }
    }

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

    pub fn snapshot_interval(&self) -> Duration {
        let interval = self
            .snapshot_interval
            .parse::<humantime::Duration>()
            .unwrap();

        Duration::from_nanos(interval.as_nanos() as u64)
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

pub fn snapshot_interval() -> String {
    "1s".into()
}

#[derive(Deserialize, Default)]
pub struct SamplerConfig {
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    interval: Option<String>,
}

impl SamplerConfig {
    fn check(&self, name: &str) {
        match self
            .interval
            .as_ref()
            .map(|v| v.parse::<humantime::Duration>())
        {
            Some(Err(e)) => {
                eprintln!("{name} sampler interval is not valid: {e}");
                std::process::exit(1);
            }
            Some(Ok(interval)) => {
                if Duration::from_nanos(interval.as_nanos() as u64) < Duration::from_millis(1) {
                    eprintln!("{name} sampler interval is too short. Minimum interval is: 1ms");
                    std::process::exit(1);
                }
            }
            _ => {}
        }
    }
}
