use async_trait::async_trait;
use backtrace::Backtrace;
use clap::{value_parser, Command, ValueEnum};
use linkme::distributed_slice;
use metriken_exposition::{MsgpackToParquet, ParquetOptions};
use reqwest::{Client, Url};
use serde::Deserialize;
use tempfile::tempfile_in;
use tracing::{debug, error, info, warn};

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// modules for each mode of operation
mod agent;
mod exporter;
mod hindsight;
mod mcp;
mod parquet_metadata;
mod parquet_tools;
mod recorder;
mod rez_reader;
mod status_cli;
mod viewer;

mod common;

pub use common::*;

/// Service extension templates baked into the release binary from
/// `config/templates/*.json`. Used by viewer and `parquet annotate`
/// when the user hasn't passed an explicit `--templates` path.
/// Developer-mode builds fall back to reading the same directory off
/// disk so template edits don't require a rebuild.
#[cfg(not(feature = "developer-mode"))]
pub static EMBEDDED_TEMPLATES: include_dir::Dir<'_> =
    include_dir::include_dir!("$CARGO_MANIFEST_DIR/config/templates");

static STATE: AtomicUsize = AtomicUsize::new(RUNNING);

static RUNNING: usize = 0;
static CAPTURING: usize = 1;
static TERMINATING: usize = 2;

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Format {
    Parquet,
    Raw,
    Rez,
}

fn main() {
    // custom panic hook to terminate whole process after unwinding
    std::panic::set_hook(Box::new(|s| {
        eprintln!("{s}");
        eprintln!("{:?}", Backtrace::new());
        std::process::exit(101);
    }));

    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("failed to install rustls ring crypto provider");

    let cli = Command::new(env!("CARGO_BIN_NAME"))
        .version(env!("CARGO_PKG_VERSION"))
        .about("High-resolution systems performance telemetry")
        .long_about(
            "Rezolus is a high-resolution systems performance telemetry agent. One binary runs\n\
             in several roles; the first three are long-running services, the rest are on-demand\n\
             tools.\n\n\
             MODES:\n    \
             agent      (default) Collect system metrics via eBPF/perf and serve them over HTTP.\n\
             \x20              Run with no subcommand: `rezolus <config.toml>`.\n    \
             exporter   Re-expose an agent's metrics on a Prometheus-compatible endpoint.\n    \
             hindsight  Keep an on-disk ring buffer for after-the-fact incident snapshots.\n    \
             record     Scrape an endpoint to a parquet file (optionally wrapping a command).\n    \
             view       Serve a web dashboard for a recording or a live agent.\n    \
             mcp        AI analysis tools (PromQL, anomaly detection, correlation) over a file.\n    \
             parquet    Inspect and transform parquet recordings (metadata/annotate/combine/filter).\n    \
             status     Print a running agent's status/sampler health: `rezolus status <endpoint>`.\n\n\
             The agent is the source everything else reads from; start there, then reach for a\n\
             tool. Run `rezolus <mode> --help` for a mode's options and examples.\n\n\
             EXAMPLES:\n    \
             # Run the agent (needs root for eBPF on Linux)\n    \
             sudo rezolus config/agent.toml\n\n    \
             # Record the local agent for 5 minutes, then view it\n    \
             rezolus record --duration 5m -o out.parquet\n    \
             rezolus view out.parquet",
        )
        .subcommand_negates_reqs(true)
        .arg(
            clap::Arg::new("CONFIG")
                .help("Agent config file (default mode when no subcommand is given), e.g. config/agent.toml")
                .value_parser(value_parser!(PathBuf))
                .action(clap::ArgAction::Set)
                .required(true)
                .index(1),
        )
        .subcommand(exporter::command())
        .subcommand(hindsight::command())
        .subcommand(mcp::command())
        .subcommand(parquet_tools::command())
        .subcommand(recorder::command())
        .subcommand(status_cli::command())
        .subcommand(viewer::command())
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
        Some(("mcp", args)) => {
            let config = mcp::Config::try_from(args.clone()).expect("failed to configure");

            mcp::run(config)
        }
        Some(("parquet", args)) => {
            parquet_tools::run(args.clone());
        }
        Some(("record", args)) => {
            let config = recorder::RecordingConfig::from_args(args).expect("failed to configure");

            recorder::run(config)
        }
        Some(("status", args)) => {
            status_cli::run(args);
        }
        Some(("view", args)) => {
            let config = viewer::Config::try_from(args.clone()).expect("failed to configure");

            viewer::run(config)
        }
        _ => {
            unimplemented!()
        }
    }
}
