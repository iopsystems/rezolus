use async_trait::async_trait;
use backtrace::Backtrace;
use clap::{value_parser, Command};
use linkme::distributed_slice;
use reqwest::{Client, Url};
use tracing::{debug, error, info, warn};

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// modules for each mode of operation
mod agent;
mod analysis;
mod exporter;
mod hindsight;
mod mcp;
mod parquet_metadata;
mod parquet_tools;
mod recorder;
pub(crate) use ::rez::reader as rez_reader;
mod status_cli;
mod viewer;

mod common;
mod dendro_copy;

pub use common::*;

static STATE: AtomicUsize = AtomicUsize::new(RUNNING);

static RUNNING: usize = 0;
static TERMINATING: usize = 2;

/// What `rezolus recording` is mounted with. `parquet` is kept as an alias
/// of `recording` because it is what existing scripts type: the subcommands
/// stopped being parquet-specific once `.rez` arrived, and renaming them was
/// not worth breaking those scripts.
fn recording_host() -> parquet_tools::Host<'static> {
    parquet_tools::Host::new("rezolus", &viewer::load_template_registry)
        .display_name("Rezolus")
        .agent("rezolus")
        .alias("parquet")
        .table_flag(parquet_tools::TableFlag::Samplers)
        .list_recordings("rezolus mcp describe-recording")
        .record("rezolus record")
        .hindsight("rezolus hindsight")
        .view("rezolus view")
        .mcp("rezolus mcp")
        .buffer_path("/var/lib/rezolus/hindsight.dendro")
}

fn main() {
    // custom panic hook: print the message and a backtrace, then exit with
    // status 101. The hook runs before unwinding starts, so neither
    // catch_unwind nor a tokio JoinHandle sees the panic, and no destructors run.
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
             record     Record one or more endpoints to a .dendro archive on disk (optionally\n\
             \x20              wrapping a command).\n    \
             view       Serve a web dashboard for a recording or a live agent.\n    \
             mcp        AI analysis tools (PromQL, anomaly detection, correlation, feature\n\
             \x20              extraction) over a file.\n    \
             recording  Inspect and transform recordings (metadata/annotate/combine/filter/upgrade).\n    \
             status     Print a running agent's status/sampler health: `rezolus status <endpoint>`.\n\n\
             The agent is the source everything else reads from; start there, then reach for a\n\
             tool. Run `rezolus <mode> --help` for a mode's options and examples.\n\n\
             EXAMPLES:\n    \
             # Run the agent (needs root for eBPF on Linux)\n    \
             sudo rezolus config/agent.toml\n\n    \
             # Record the local agent for 5 minutes, then view it\n    \
             rezolus record --duration 5m -o out.dendro\n    \
             rezolus view out.dendro",
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
        .subcommand(recording_host().command())
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
            // Same as `record` below: a malformed `--recording` is the
            // caller's typo, and `expect` reported it as a panic whose
            // backtrace buried the one line saying what to fix. That matters
            // more here than for a human — an agent reading a page of unwind
            // frames has to guess whether it hit a bug or mistyped a flag.
            let config = match mcp::Config::try_from(args.clone()) {
                Ok(config) => config,
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(2);
                }
            };

            mcp::run(config)
        }
        // Clap resolves the `parquet` alias to this name, so one arm serves both.
        Some(("recording", args)) => {
            use clap::FromArgMatches;
            let cmd = parquet_tools::Recording::from_arg_matches(args).unwrap_or_else(|e| e.exit());
            std::process::exit(recording_host().run(cmd));
        }
        Some(("record", args)) => {
            // A usage error here is the user's typo, not a bug: `expect` sent
            // it out as a panic with a backtrace, burying the one line that
            // says what to fix.
            let config = match recorder::RecordingConfig::from_args(args) {
                Ok(config) => config,
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(2);
                }
            };

            recorder::run(config)
        }
        Some(("status", args)) => {
            status_cli::run(args);
        }
        Some(("view", args)) => {
            // A message and a non-zero exit, not a panic: every error out of
            // `Config::try_from` is a malformed argument (an unparseable URL,
            // a `--baseline`/`--experiment` pair with no `=`), and answering a
            // typo with a backtrace buries the one line that says what to fix.
            let config = match viewer::Config::try_from(args.clone()) {
                Ok(config) => config,
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(2);
                }
            };

            viewer::run(config)
        }
        _ => {
            unimplemented!()
        }
    }
}
