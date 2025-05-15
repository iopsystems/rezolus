use crate::Format;

use clap::ArgMatches;
use ringlog::Level;
use serde::Deserialize;

use std::net::{SocketAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};

mod general;
mod log;

use general::General;
use log::Log;

fn source() -> String {
    "0.0.0.0:4241".into()
}

fn interval() -> String {
    "1s".into()
}

fn duration() -> String {
    "15m".into()
}

fn output() -> String {
    "/tmp/rezolus.parquet".into()
}

fn parquet() -> Format {
    Format::Parquet
}

#[derive(Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    general: General,
    #[serde(default)]
    log: Log,
}

impl TryFrom<ArgMatches> for Config {
    type Error = String;

    fn try_from(
        args: ArgMatches,
    ) -> Result<Self, <Self as std::convert::TryFrom<clap::ArgMatches>>::Error> {
        let config: PathBuf = args.get_one::<PathBuf>("CONFIG").unwrap().to_path_buf();
        match Config::load(&config) {
            Ok(c) => Ok(c),
            Err(error) => {
                eprintln!("error loading config file: {:?}\n{error}", config);
                std::process::exit(1);
            }
        }
    }
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

        Ok(config)
    }

    pub fn log(&self) -> &Log {
        &self.log
    }

    pub fn general(&self) -> &General {
        &self.general
    }
}
