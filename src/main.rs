use async_trait::async_trait;
use backtrace::Backtrace;
use clap::{value_parser, Command, ValueEnum};
use linkme::distributed_slice;
use metriken_exposition::{MsgpackToParquet, ParquetOptions};
use reqwest::blocking::Client;
use reqwest::Url;
use ringlog::*;
use serde::Deserialize;
use tempfile::tempfile_in;

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// modules for each mode of operation
mod agent;
mod exporter;
mod hindsight;
mod recorder;

mod common;

pub use common::*;

static STATE: AtomicUsize = AtomicUsize::new(RUNNING);

static RUNNING: usize = 0;
static CAPTURING: usize = 1;
static TERMINATING: usize = 2;

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
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
                .help("Configuration file")
                .value_parser(value_parser!(PathBuf))
                .action(clap::ArgAction::Set)
                .required(true)
                .index(1),
        )
        .subcommand(exporter::command())
        .subcommand(hindsight::command())
        .subcommand(recorder::command())
        .get_matches();

    match cli.subcommand() {
        None => {
            let config: PathBuf = cli.get_one::<PathBuf>("CONFIG").unwrap().to_path_buf();

            agent::run(config)
        }
        Some(("exporter", args)) => {
            let config = exporter::Config::try_from(args.clone()).expect("failed to configure");

            exporter::run(config)
        }
        Some(("hindsight", args)) => {
            let config = hindsight::Config::try_from(args.clone()).expect("failed to configure");

            hindsight::run(config)
        }
        Some(("record", args)) => {
            let config = recorder::Config::try_from(args.clone()).expect("failed to configure");

            recorder::run(config)
        }
        _ => {
            unimplemented!()
        }
    }
}
