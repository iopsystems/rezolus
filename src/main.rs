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
mod summarize;

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
        .subcommand(flight_recorder::command())
        .subcommand(recorder::command())
        .subcommand(summarize::command())
        .get_matches();

    match cli.subcommand() {
        None => {
            let config: PathBuf = cli.get_one::<PathBuf>("CONFIG").unwrap().to_path_buf();

            agent::run(config)
        }
        Some(("flight-recorder", args)) => {
            let config =
                flight_recorder::Config::try_from(args.clone()).expect("failed to configure");

            flight_recorder::run(config)
        }
        Some(("record", args)) => {
            let config = recorder::Config::try_from(args.clone()).expect("failed to configure");

            recorder::run(config)
        }
        Some(("summarize", args)) => {
            let config = summarize::Config::try_from(args.clone()).expect("failed to configure");

            summarize::run(config)
        }
        _ => {
            unimplemented!()
        }
    }
}
