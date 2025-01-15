use async_trait::async_trait;
use backtrace::Backtrace;
use chrono::Timelike;
use chrono::Utc;
use clap::value_parser;
use clap::Command;
use clap::ValueEnum;
use linkme::distributed_slice;
use metriken_exposition::MsgpackToParquet;
use metriken_exposition::ParquetOptions;
use reqwest::blocking::Client;
use reqwest::Url;
use ringlog::*;
use std::io::Read;
use std::io::Seek;
use std::io::SeekFrom;
use std::io::Write;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;
use tempfile::tempfile_in;

use std::sync::Arc;

/// modules for each mode of operation
mod agent;
mod flight_recorder;
mod recorder;

/// general modules for core functionality
mod common;
mod config;
mod exposition;
mod samplers;

use config::Config;
use samplers::{Sampler, SamplerResult};

#[distributed_slice]
pub static SAMPLERS: [fn(config: Arc<Config>) -> SamplerResult] = [..];

static STATE: AtomicUsize = AtomicUsize::new(RUNNING);

static RUNNING: usize = 0;
static CAPTURING: usize = 1;
static TERMINATING: usize = 2;

struct AgentArgs {
    config: PathBuf,
}

struct FlightRecorderConfig {
    interval: humantime::Duration,
    duration: humantime::Duration,
    format: Format,
    verbose: u8,
    url: Url,
    output: PathBuf,
}

struct RecorderConfig {
    interval: humantime::Duration,
    duration: Option<humantime::Duration>,
    format: Format,
    verbose: u8,
    url: Url,
    output: PathBuf,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
enum Format {
    Parquet,
    Raw,
}

fn main() {
    // custom panic hook to terminate whole process after unwinding
    std::panic::set_hook(Box::new(|s| {
        eprintln!("{s}");
        eprintln!("{:?}", Backtrace::new());
        std::process::exit(101);
    }));

    // parse command line options
    let cli = Command::new(env!("CARGO_BIN_NAME"))
        .version(env!("CARGO_PKG_VERSION"))
        .long_about("Rezolus provides high-resolution systems performance telemetry.")
        .subcommand_negates_reqs(true)
        .arg(
            clap::Arg::new("CONFIG")
                .help("Server configuration file")
                .value_parser(value_parser!(PathBuf))
                .action(clap::ArgAction::Set)
                .required(true)
                .index(1),
        )
        .subcommand(
            Command::new("flight-recorder")
                .about("Continuous recording to an on-disk ring buffer")
                .arg(
                    clap::Arg::new("URL")
                        .help("Rezolus HTTP endpoint")
                        .action(clap::ArgAction::Set)
                        .value_parser(value_parser!(Url))
                        .required(true)
                        .index(1),
                )
                .arg(
                    clap::Arg::new("OUTPUT")
                        .help("Path to the output file")
                        .action(clap::ArgAction::Set)
                        .value_parser(value_parser!(PathBuf))
                        .required(true)
                        .index(2),
                )
                .arg(
                    clap::Arg::new("VERBOSE")
                        .long("verbose")
                        .short('v')
                        .help("Increase the verbosity")
                        .action(clap::ArgAction::Count),
                )
                .arg(
                    clap::Arg::new("INTERVAL")
                        .long("interval")
                        .short('i')
                        .help("Sets the collection interval")
                        .action(clap::ArgAction::Set)
                        .default_value("1s")
                        .value_parser(value_parser!(humantime::Duration)),
                )
                .arg(
                    clap::Arg::new("DURATION")
                        .long("duration")
                        .short('d')
                        .help("Sets the collection interval")
                        .action(clap::ArgAction::Set)
                        .default_value("15m")
                        .value_parser(value_parser!(humantime::Duration)),
                )
                .arg(
                    clap::Arg::new("FORMAT")
                        .long("format")
                        .short('f')
                        .help("Sets the collection format")
                        .action(clap::ArgAction::Set)
                        .default_value("parquet")
                        .value_parser(value_parser!(Format)),
                ),
        )
        .subcommand(
            Command::new("record")
                .about("On-demand recording to a file")
                .arg(
                    clap::Arg::new("URL")
                        .help("Rezolus HTTP endpoint")
                        .action(clap::ArgAction::Set)
                        .value_parser(value_parser!(Url))
                        .required(true)
                        .index(1),
                )
                .arg(
                    clap::Arg::new("OUTPUT")
                        .help("Path to the output file")
                        .action(clap::ArgAction::Set)
                        .value_parser(value_parser!(PathBuf))
                        .required(true)
                        .index(2),
                )
                .arg(
                    clap::Arg::new("VERBOSE")
                        .long("verbose")
                        .short('v')
                        .help("Increase the verbosity")
                        .action(clap::ArgAction::Count),
                )
                .arg(
                    clap::Arg::new("INTERVAL")
                        .long("interval")
                        .short('i')
                        .help("Sets the collection interval")
                        .action(clap::ArgAction::Set)
                        .default_value("1s")
                        .value_parser(value_parser!(humantime::Duration)),
                )
                .arg(
                    clap::Arg::new("DURATION")
                        .long("duration")
                        .short('d')
                        .help("Sets the collection interval")
                        .action(clap::ArgAction::Set)
                        .value_parser(value_parser!(humantime::Duration)),
                )
                .arg(
                    clap::Arg::new("FORMAT")
                        .long("format")
                        .short('f')
                        .help("Sets the collection format")
                        .action(clap::ArgAction::Set)
                        .default_value("parquet")
                        .value_parser(value_parser!(Format)),
                ),
        )
        .get_matches();

    match cli.subcommand() {
        None => {
            let config: PathBuf = cli.get_one::<PathBuf>("CONFIG").unwrap().to_path_buf();

            agent::run(AgentArgs { config })
        }
        Some(("flight-recorder", args)) => flight_recorder::run(FlightRecorderConfig {
            url: args.get_one::<Url>("URL").unwrap().clone(),
            output: args.get_one::<PathBuf>("OUTPUT").unwrap().to_path_buf(),
            verbose: *args.get_one::<u8>("VERBOSE").unwrap_or(&0),
            interval: *args
                .get_one::<humantime::Duration>("INTERVAL")
                .unwrap_or(&humantime::Duration::from_str("1s").unwrap()),
            duration: *args
                .get_one::<humantime::Duration>("DURATION")
                .unwrap_or(&humantime::Duration::from_str("15m").unwrap()),
            format: args
                .get_one::<Format>("FORMAT")
                .copied()
                .unwrap_or(Format::Parquet),
        }),
        Some(("record", args)) => recorder::run(RecorderConfig {
            url: args.get_one::<Url>("URL").unwrap().clone(),
            output: args.get_one::<PathBuf>("OUTPUT").unwrap().to_path_buf(),
            verbose: *args.get_one::<u8>("VERBOSE").unwrap_or(&0),
            interval: *args
                .get_one::<humantime::Duration>("INTERVAL")
                .unwrap_or(&humantime::Duration::from_str("1s").unwrap()),
            duration: args.get_one::<humantime::Duration>("DURATION").copied(),
            format: args
                .get_one::<Format>("FORMAT")
                .copied()
                .unwrap_or(Format::Parquet),
        }),
        _ => {
            unimplemented!()
        }
    }
}
