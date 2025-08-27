use crate::*;

use clap::{ArgMatches, Command};
use std::path::PathBuf;

pub mod correlation;
mod describe_metrics;
mod server;

use crate::viewer::promql::QueryEngine;
use crate::viewer::tsdb::Tsdb;
use chrono::{DateTime, Utc};

/// Format recording information for display
pub fn format_recording_info(file_path: &str, tsdb: &Arc<Tsdb>, engine: &QueryEngine) -> String {
    let (start_time, end_time) = engine.get_time_range();
    let duration_seconds = end_time - start_time;

    // Format duration nicely
    let hours = (duration_seconds / 3600.0) as u64;
    let minutes = ((duration_seconds % 3600.0) / 60.0) as u64;
    let seconds = (duration_seconds % 60.0) as u64;

    let duration_str = if hours > 0 {
        format!("{hours}h {minutes}m {seconds}s")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else {
        format!("{seconds}s")
    };

    // Convert Unix timestamps to UTC datetime strings
    let start_datetime = DateTime::from_timestamp(start_time as i64, 0)
        .map(|dt: DateTime<Utc>| dt.format("%Y-%m-%d %H:%M:%S UTC").to_string())
        .unwrap_or_else(|| format!("{start_time:.0} (invalid timestamp)"));

    let end_datetime = DateTime::from_timestamp(end_time as i64, 0)
        .map(|dt: DateTime<Utc>| dt.format("%Y-%m-%d %H:%M:%S UTC").to_string())
        .unwrap_or_else(|| format!("{end_time:.0} (invalid timestamp)"));

    format!(
        "Recording Information\n\
         =====================\n\
         File: {}\n\
         Rezolus Version: {}\n\
         Source: {}\n\
         Recording Duration: {} ({:.1} seconds)\n\
         Start Time: {} (epoch: {:.0})\n\
         End Time: {} (epoch: {:.0})",
        file_path,
        tsdb.version(),
        tsdb.source(),
        duration_str,
        duration_seconds,
        start_datetime,
        start_time,
        end_datetime,
        end_time
    )
}

/// Run the MCP server or execute MCP commands
pub fn run(config: Config) {
    match config.mode {
        Mode::Server => run_server(config),
        Mode::AnalyzeCorrelation {
            file,
            query1,
            query2,
        } => run_analyze_correlation(file, query1, query2),
        Mode::DescribeRecording { file } => run_describe_recording(file),
        Mode::DescribeMetrics { file } => run_describe_metrics(file),
    }
}

fn run_server(config: Config) {
    // configure debug log
    let debug_output: Box<dyn Output> = Box::new(Stderr::new());

    let level = match config.verbose {
        0 => Level::Info,
        1 => Level::Debug,
        _ => Level::Trace,
    };

    let debug_log = if level <= Level::Info {
        LogBuilder::new().format(ringlog::default_format)
    } else {
        LogBuilder::new()
    }
    .output(debug_output)
    .build()
    .expect("failed to initialize debug log");

    let mut log = MultiLogBuilder::new()
        .level_filter(level.to_level_filter())
        .default(debug_log)
        .build()
        .start();

    // initialize async runtime
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("rezolus")
        .build()
        .expect("failed to launch async runtime");

    // spawn logging thread
    rt.spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            let _ = log.flush();
        }
    });

    ctrlc::set_handler(move || {
        std::process::exit(2);
    })
    .expect("failed to set ctrl-c handler");

    // launch the server
    rt.block_on(async {
        let mut server = server::Server::new();
        if let Err(e) = server.run_stdio().await {
            eprintln!("MCP server error: {e}");
            std::process::exit(1);
        }
    });
}

fn run_analyze_correlation(file: PathBuf, query1: String, query2: String) {
    use crate::viewer::promql::QueryEngine;
    use crate::viewer::tsdb::Tsdb;

    // Load the parquet file
    let tsdb = match Tsdb::load(&file) {
        Ok(tsdb) => Arc::new(tsdb),
        Err(e) => {
            eprintln!("Failed to load parquet file: {e}");
            std::process::exit(1);
        }
    };

    // Create query engine
    let engine = Arc::new(QueryEngine::new(tsdb.clone()));

    // Run correlation analysis
    match correlation::calculate_correlation(&engine, &tsdb, &query1, &query2) {
        Ok(result) => {
            println!("{}", correlation::format_correlation_result(&result));
        }
        Err(e) => {
            eprintln!("Correlation analysis failed: {e}");
            std::process::exit(1);
        }
    }
}

fn run_describe_recording(file: PathBuf) {
    // Load the parquet file
    let tsdb = match Tsdb::load(&file) {
        Ok(tsdb) => Arc::new(tsdb),
        Err(e) => {
            eprintln!("Failed to load parquet file: {e}");
            std::process::exit(1);
        }
    };

    // Create query engine
    let engine = QueryEngine::new(tsdb.clone());

    // Use the shared formatting function
    let output = format_recording_info(file.to_str().unwrap_or("<unknown>"), &tsdb, &engine);
    println!("{output}");
}

fn run_describe_metrics(file: PathBuf) {
    // Load the parquet file
    let tsdb = match Tsdb::load(&file) {
        Ok(tsdb) => Arc::new(tsdb),
        Err(e) => {
            eprintln!("Failed to load parquet file: {e}");
            std::process::exit(1);
        }
    };

    // Format and print the metrics list
    let output = describe_metrics::format_metrics_description(&tsdb);
    println!("{output}");
}

/// MCP operation mode
pub enum Mode {
    Server,
    AnalyzeCorrelation {
        file: PathBuf,
        query1: String,
        query2: String,
    },
    DescribeRecording {
        file: PathBuf,
    },
    DescribeMetrics {
        file: PathBuf,
    },
}

/// MCP server configuration
pub struct Config {
    pub verbose: u8,
    pub mode: Mode,
}

impl TryFrom<ArgMatches> for Config {
    type Error = String;

    fn try_from(args: ArgMatches) -> Result<Self, String> {
        let verbose = args.get_count("VERBOSE");

        let mode = match args.subcommand() {
            Some(("analyze-correlation", sub_args)) => {
                let file = sub_args
                    .get_one::<PathBuf>("FILE")
                    .ok_or("File argument is required")?
                    .clone();
                let query1 = sub_args
                    .get_one::<String>("QUERY1")
                    .ok_or("Query1 argument is required")?
                    .clone();
                let query2 = sub_args
                    .get_one::<String>("QUERY2")
                    .ok_or("Query2 argument is required")?
                    .clone();

                Mode::AnalyzeCorrelation {
                    file,
                    query1,
                    query2,
                }
            }
            Some(("describe-recording", sub_args)) => {
                let file = sub_args
                    .get_one::<PathBuf>("FILE")
                    .ok_or("File argument is required")?
                    .clone();
                Mode::DescribeRecording { file }
            }
            Some(("describe-metrics", sub_args)) => {
                let file = sub_args
                    .get_one::<PathBuf>("FILE")
                    .ok_or("File argument is required")?
                    .clone();
                Mode::DescribeMetrics { file }
            }
            _ => Mode::Server,
        };

        Ok(Config { verbose, mode })
    }
}

/// Create the MCP subcommand
pub fn command() -> Command {
    Command::new("mcp")
        .about("Run Rezolus MCP server for AI analysis or execute analysis commands")
        .arg(
            clap::Arg::new("VERBOSE")
                .long("verbose")
                .short('v')
                .help("Increase verbosity")
                .action(clap::ArgAction::Count),
        )
        .subcommand(
            Command::new("analyze-correlation")
                .about("Analyze correlation between two metrics using the full recording")
                .arg(
                    clap::Arg::new("FILE")
                        .help("Parquet file to analyze")
                        .value_parser(clap::value_parser!(PathBuf))
                        .required(true)
                        .index(1),
                )
                .arg(
                    clap::Arg::new("QUERY1")
                        .help("First PromQL query (e.g., 'irate(cgroup_cpu_usage[1m])')")
                        .required(true)
                        .index(2),
                )
                .arg(
                    clap::Arg::new("QUERY2")
                        .help("Second PromQL query (e.g., 'cgroup_memory_used')")
                        .required(true)
                        .index(3),
                ),
        )
        .subcommand(
            Command::new("describe-recording")
                .about("Describe the contents of a recording file")
                .arg(
                    clap::Arg::new("FILE")
                        .help("Parquet file to describe")
                        .value_parser(clap::value_parser!(PathBuf))
                        .required(true)
                        .index(1),
                ),
        )
        .subcommand(
            Command::new("describe-metrics")
                .about("List and describe all metrics available in a recording")
                .arg(
                    clap::Arg::new("FILE")
                        .help("Parquet file to analyze")
                        .value_parser(clap::value_parser!(PathBuf))
                        .required(true)
                        .index(1),
                ),
        )
}
