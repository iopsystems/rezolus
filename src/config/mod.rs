use std::collections::HashMap;
use std::net::SocketAddr;
use std::net::ToSocketAddrs;
use std::path::Path;
use serde::Deserialize;

#[derive(Deserialize)]
// #[serde(deny_unknown_fields)]
pub struct Config {
    general: General,
    samplers: HashMap<String, SamplerConfig>,
}

impl Config {
    pub fn load(path: &dyn AsRef<Path>) -> Result<Self, String> {
        let content = std::fs::read_to_string(path).map_err(|e| {
            eprintln!("unable to open config file: {e}");
            std::process::exit(1);
        }).unwrap();

        toml::from_str(&content).map_err(|e| {
            eprintln!("failed to parse config file: {e}");
            std::process::exit(1);
        })
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
}
#[derive(Deserialize)]
pub struct General {
    listen: String,
}

impl General {
    pub fn listen(&self) -> SocketAddr {
        self.listen.to_socket_addrs().map_err(|e| {
            eprintln!("bad listen address: {e}");
            std::process::exit(1);
        }).unwrap().next().ok_or_else(|| {
            eprintln!("could not resolve socket addr");
            std::process::exit(1);
        }).unwrap()
    }
}

#[derive(Deserialize)]
pub struct SamplerConfig {
    enabled: bool,
}

impl SamplerConfig {
    pub fn enabled(&self) -> bool {
        self.enabled
    }
}