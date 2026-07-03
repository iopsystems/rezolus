use crate::*;

use clap::{ArgMatches, Command};
use std::path::PathBuf;

pub mod anomaly_detection;
pub mod correlation;
mod describe_metrics;
mod server;

use chrono::{DateTime, Utc};
use metriken_query::{MetricsSource, ParquetReader, QueryResult};

/// Format recording information for display
pub fn format_recording_info(file_path: &str, data: &dyn MetricsSource) -> String {
    let (start_time, end_time) = data.time_range().unwrap_or((0.0, 0.0));
    let duration_seconds = end_time - start_time;

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
        data.version(),
        data.source(),
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
        Mode::DetectAnomalies { file, query } => run_detect_anomalies(file, query),
        Mode::Query { file, query } => run_query(file, query),
    }
}

fn run_server(config: Config) {
    let _log_drain = configure_logging(verbosity_to_level(config.verbose));

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("rezolus")
        .build()
        .expect("failed to launch async runtime");

    ctrlc::set_handler(move || {
        std::process::exit(2);
    })
    .expect("failed to set ctrl-c handler");

    rt.block_on(async {
        let mut server = server::Server::new();
        if let Err(e) = server.run_stdio().await {
            eprintln!("MCP server error: {e}");
            std::process::exit(1);
        }
    });
}

fn run_analyze_correlation(file: PathBuf, query1: String, query2: String) {
    let reader = match ParquetReader::open(&file) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Failed to load parquet file: {e}");
            std::process::exit(1);
        }
    };

    match correlation::calculate_correlation(&reader, &query1, &query2) {
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
    let reader = match ParquetReader::open(&file) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Failed to load parquet file: {e}");
            std::process::exit(1);
        }
    };

    let output = format_recording_info(file.to_str().unwrap_or("<unknown>"), &reader);
    println!("{output}");
}

fn run_describe_metrics(file: PathBuf) {
    let reader = match ParquetReader::open(&file) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Failed to load parquet file: {e}");
            std::process::exit(1);
        }
    };

    let output = describe_metrics::format_metrics_description(&reader);
    println!("{output}");
}

fn run_detect_anomalies(file: PathBuf, query: Option<String>) {
    let reader = match ParquetReader::open(&file) {
        Ok(r) => Arc::new(r),
        Err(e) => {
            eprintln!("Failed to load parquet file: {e}");
            std::process::exit(1);
        }
    };

    if let Some(query) = query {
        match anomaly_detection::detect_anomalies(reader.as_ref(), &query) {
            Ok(result) => {
                println!(
                    "{}",
                    anomaly_detection::format_anomaly_detection_result(&result)
                );
            }
            Err(e) => {
                eprintln!("Anomaly detection failed: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    run_exhaustive_detection(reader);
}

fn run_exhaustive_detection(reader: Arc<ParquetReader>) {
    // Metrics to skip - these are raw building blocks or redundant metrics
    let skip_metrics = [
        // CPU building blocks - only meaningful when combined
        "cpu_tsc",          // Raw TSC counter - only useful for frequency calculation
        "cpu_aperf",        // Actual perf counter - combine with mperf for frequency
        "cpu_mperf",        // Max perf counter - combine with aperf for frequency
        "cgroup_cpu_aperf", // Same for cgroup versions
        "cgroup_cpu_mperf",
        // NUMA metrics - focus on local (good) and foreign (bad) instead of these
        "memory_numa_hit",        // Redundant with local/foreign
        "memory_numa_miss",       // Redundant with local/foreign
        "memory_numa_other",      // Less actionable than foreign
        "memory_numa_interleave", // Rarely used policy
        // Cgroup CPU bandwidth config - skip static configuration values
        "cgroup_cpu_bandwidth_periods", // Total periods elapsed - not actionable
        "cgroup_cpu_bandwidth_period_duration", // Static config value
        "cgroup_cpu_bandwidth_quota",   // Static config value
    ];

    let mut metrics_to_analyze = Vec::new();

    for name in reader.counter_names() {
        if !skip_metrics.contains(&name.as_str()) {
            metrics_to_analyze.push((name.to_string(), "counter", None));
        }
    }

    for name in reader.gauge_names() {
        if !skip_metrics.contains(&name.as_str()) {
            metrics_to_analyze.push((name.to_string(), "gauge", None));
        }
    }

    for name in reader.histogram_names() {
        metrics_to_analyze.push((name.to_string(), "histogram_p50", None));
        metrics_to_analyze.push((name.to_string(), "histogram_p90", None));
        metrics_to_analyze.push((name.to_string(), "histogram_p99", None));
    }

    // Add derived metrics that combine raw counters into meaningful calculations
    let mut derived_metrics = Vec::new();

    // CPU Frequency = (aperf / mperf) - shows actual vs max performance
    if reader.has_counter("cpu_aperf") && reader.has_counter("cpu_mperf") {
        derived_metrics.push((
            "cpu_frequency_ratio".to_string(),
            "derived",
            Some("sum(rate(cpu_aperf[1m])) / sum(rate(cpu_mperf[1m]))".to_string()),
        ));
    }

    // CPU Instructions Per Cycle (IPC) - efficiency metric
    if reader.has_counter("cpu_instructions") && reader.has_counter("cpu_cycles") {
        derived_metrics.push((
            "cpu_instructions_per_cycle".to_string(),
            "derived",
            Some("sum(rate(cpu_instructions[1m])) / sum(rate(cpu_cycles[1m]))".to_string()),
        ));
    }

    // Cgroup versions of the same
    if reader.has_counter("cgroup_cpu_aperf") && reader.has_counter("cgroup_cpu_mperf") {
        derived_metrics.push((
            "cgroup_cpu_frequency_ratio".to_string(),
            "derived",
            Some("sum(rate(cgroup_cpu_aperf[1m])) / sum(rate(cgroup_cpu_mperf[1m]))".to_string()),
        ));
    }

    if reader.has_counter("cgroup_cpu_instructions") && reader.has_counter("cgroup_cpu_cycles") {
        derived_metrics.push((
            "cgroup_cpu_instructions_per_cycle".to_string(),
            "derived",
            Some(
                "sum(rate(cgroup_cpu_instructions[1m])) / sum(rate(cgroup_cpu_cycles[1m]))"
                    .to_string(),
            ),
        ));
    }

    metrics_to_analyze.extend(derived_metrics);

    println!(
        "Exhaustive Anomaly Detection\n\
         ============================\n\
         Analyzing {} metrics from recording\n",
        metrics_to_analyze.len()
    );

    let mut total_anomalies = 0;
    let mut metrics_with_anomalies = Vec::new();

    for (metric_name, metric_type, custom_query) in &metrics_to_analyze {
        let query = if let Some(q) = custom_query {
            q.clone()
        } else {
            match &**metric_type {
                "counter" => format!("sum(rate({}[1m]))", metric_name),
                "gauge" => format!("sum({})", metric_name),
                "histogram_p50" => format!("histogram_quantile(0.50, {})", metric_name),
                "histogram_p90" => format!("histogram_quantile(0.90, {})", metric_name),
                "histogram_p99" => format!("histogram_quantile(0.99, {})", metric_name),
                _ => continue,
            }
        };

        match anomaly_detection::detect_anomalies(reader.as_ref(), &query) {
            Ok(result) => {
                if !result.anomalies.is_empty() {
                    let high_severity = result
                        .anomalies
                        .iter()
                        .filter(|a| {
                            matches!(
                                a.severity,
                                anomaly_detection::AnomalySeverity::High
                                    | anomaly_detection::AnomalySeverity::Critical
                            )
                        })
                        .count();
                    let medium_severity = result
                        .anomalies
                        .iter()
                        .filter(|a| {
                            matches!(a.severity, anomaly_detection::AnomalySeverity::Medium)
                        })
                        .count();
                    let low_severity = result
                        .anomalies
                        .iter()
                        .filter(|a| matches!(a.severity, anomaly_detection::AnomalySeverity::Low))
                        .count();

                    total_anomalies += result.anomalies.len();
                    metrics_with_anomalies.push((
                        metric_name.clone(),
                        metric_type.to_string(),
                        result.anomalies.len(),
                        high_severity,
                        medium_severity,
                        low_severity,
                    ));
                }
            }
            Err(_e) => {
                // Silently skip metrics that fail (e.g., histograms that don't exist)
            }
        }
    }

    println!("\nSummary");
    println!("=======");
    println!(
        "Analyzed {} metrics, found anomalies in {} metrics",
        metrics_to_analyze.len(),
        metrics_with_anomalies.len()
    );
    println!("Total anomalies detected: {}\n", total_anomalies);

    if !metrics_with_anomalies.is_empty() {
        println!("Metrics with Anomalies:");
        println!("----------------------");

        // Sort by total anomalies (descending)
        metrics_with_anomalies.sort_by_key(|k| std::cmp::Reverse(k.2));

        for (metric, metric_type, total, high, medium, low) in metrics_with_anomalies {
            let type_label = match metric_type.as_ref() {
                "counter" => "COUNTER",
                "gauge" => "GAUGE",
                "histogram_p50" => "HISTOGRAM (p50)",
                "histogram_p90" => "HISTOGRAM (p90)",
                "histogram_p99" => "HISTOGRAM (p99)",
                "derived" => "DERIVED",
                _ => &metric_type,
            };

            println!(
                "• {} ({}) - {} anomalies (HIGH: {}, MEDIUM: {}, LOW: {})",
                metric, type_label, total, high, medium, low
            );
        }

        println!(
            "\nRun 'detect-anomalies <file> <metric>' for detailed analysis of specific metrics."
        );
    }
}

fn run_query(file: PathBuf, query: String) {
    let reader = match ParquetReader::open(&file) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Failed to load parquet file: {e}");
            std::process::exit(1);
        }
    };

    let (start_time, end_time) = reader.time_range().unwrap_or((0.0, 0.0));
    let step = 1.0;

    match reader.query_range(&query, start_time, end_time, step) {
        Ok(result) => {
            println!("{}", format_query_result(&result));
        }
        Err(e) => {
            eprintln!("Query failed: {e}");
            std::process::exit(1);
        }
    }
}

fn format_query_result(result: &QueryResult) -> String {
    use std::fmt::Write;
    let mut output = String::new();

    match result {
        QueryResult::Vector { result } => {
            writeln!(&mut output, "Instant Vector Result:").unwrap();
            writeln!(&mut output, "======================").unwrap();
            for sample in result {
                writeln!(
                    &mut output,
                    "{} = {}",
                    format_metric(&sample.metric),
                    sample.value.1
                )
                .unwrap();
            }
        }
        QueryResult::Matrix { result } => {
            writeln!(&mut output, "Range Vector Result:").unwrap();
            writeln!(&mut output, "====================").unwrap();
            for series in result {
                writeln!(&mut output, "{}:", format_metric(&series.metric)).unwrap();
                writeln!(
                    &mut output,
                    "  Time series with {} points",
                    series.values.len()
                )
                .unwrap();
                if !series.values.is_empty() {
                    let first = &series.values[0];
                    let last = &series.values[series.values.len() - 1];
                    writeln!(&mut output, "  First: {} = {}", first.0, first.1).unwrap();
                    writeln!(&mut output, "  Last:  {} = {}", last.0, last.1).unwrap();

                    let values: Vec<f64> = series.values.iter().map(|(_, v)| *v).collect();
                    let min = values.iter().copied().fold(f64::INFINITY, f64::min);
                    let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                    let sum: f64 = values.iter().sum();
                    let mean = sum / values.len() as f64;

                    writeln!(&mut output, "  Min:   {}", min).unwrap();
                    writeln!(&mut output, "  Max:   {}", max).unwrap();
                    writeln!(&mut output, "  Mean:  {}", mean).unwrap();
                }
                writeln!(&mut output).unwrap();
            }
        }
        QueryResult::Scalar { result } => {
            writeln!(&mut output, "Scalar Result:").unwrap();
            writeln!(&mut output, "==============").unwrap();
            writeln!(&mut output, "{} = {}", result.0, result.1).unwrap();
        }
        QueryResult::HistogramHeatmap { result } => {
            writeln!(&mut output, "Histogram Heatmap Result:").unwrap();
            writeln!(&mut output, "=========================").unwrap();
            writeln!(
                &mut output,
                "Time points: {}, Buckets: {}, Data points: {}",
                result.timestamps.len(),
                result.bucket_bounds.len(),
                result.data.len()
            )
            .unwrap();
            writeln!(
                &mut output,
                "Value range: {:.2} - {:.2}",
                result.min_value, result.max_value
            )
            .unwrap();
        }
    }

    output
}

fn format_metric(metric: &std::collections::HashMap<String, String>) -> String {
    if metric.is_empty() {
        return String::from("{}");
    }

    let mut parts: Vec<String> = metric
        .iter()
        .map(|(k, v)| format!("{}=\"{}\"", k, v))
        .collect();
    parts.sort();

    format!("{{{}}}", parts.join(", "))
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
    DetectAnomalies {
        file: PathBuf,
        query: Option<String>,
    },
    Query {
        file: PathBuf,
        query: String,
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
            Some(("detect-anomalies", sub_args)) => {
                let file = sub_args
                    .get_one::<PathBuf>("FILE")
                    .ok_or("File argument is required")?
                    .clone();
                let query = sub_args.get_one::<String>("QUERY").cloned();
                Mode::DetectAnomalies { file, query }
            }
            Some(("query", sub_args)) => {
                let file = sub_args
                    .get_one::<PathBuf>("FILE")
                    .ok_or("File argument is required")?
                    .clone();
                let query = sub_args
                    .get_one::<String>("QUERY")
                    .ok_or("Query argument is required")?
                    .clone();
                Mode::Query { file, query }
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
        .long_about(
            "AI-assisted analysis of a parquet recording. With no subcommand, runs as a Model\n\
             Context Protocol server over stdio for an LLM client. Each subcommand also runs\n\
             one-shot from the CLI, printing the same analysis to stdout.\n\n\
             SUBCOMMANDS:\n    \
             describe-recording   Summarize a recording (source, version, time range, duration)\n    \
             describe-metrics     List every metric in a recording with its type and labels\n    \
             query                Run a PromQL query against a recording\n    \
             detect-anomalies     Flag anomalies for one metric, or exhaustively across all\n    \
             analyze-correlation  Correlate two PromQL series over the recording\n\n\
             A good workflow is describe-metrics (see what's there) → query / detect-anomalies\n\
             (dig in). Run `rezolus mcp <subcommand> --help` for per-subcommand examples.\n\n\
             EXAMPLES:\n    \
             # Run as a stdio MCP server for an LLM client\n    \
             rezolus mcp\n\n    \
             # One-shot: list the metrics in a recording\n    \
             rezolus mcp describe-metrics file.parquet\n\n    \
             # One-shot: run a PromQL query\n    \
             rezolus mcp query file.parquet \"sum(rate(cpu_cycles[1m]))\"",
        )
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
                .long_about(
                    "Compute how two PromQL series move together across the whole recording\n\
                     (Pearson correlation plus supporting stats). Useful for testing a hunch\n\
                     that one metric drives another — e.g. does CPU usage track memory growth.\n\n\
                     Both arguments are full PromQL expressions, not bare metric names; wrap\n\
                     counters in rate()/irate() as you would in a query.\n\n\
                     EXAMPLE:\n    \
                     rezolus mcp analyze-correlation file.parquet \\\n        \
                     \"irate(cgroup_cpu_usage[1m])\" \"cgroup_memory_used\"",
                )
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
                .long_about(
                    "Print a high-level summary of a recording: source, Rezolus version, the\n\
                     wall-clock time range it spans, and total duration. Start here to confirm\n\
                     you have the right file before querying it.\n\n\
                     EXAMPLE:\n    \
                     rezolus mcp describe-recording file.parquet",
                )
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
                .long_about(
                    "List every metric in a recording with its type (counter/gauge/histogram),\n\
                     help text, and labels. Run this before `query` or `analyze-correlation` to\n\
                     find exact metric names and see how to phrase a PromQL expression for each\n\
                     type.\n\n\
                     EXAMPLE:\n    \
                     rezolus mcp describe-metrics file.parquet",
                )
                .arg(
                    clap::Arg::new("FILE")
                        .help("Parquet file to analyze")
                        .value_parser(clap::value_parser!(PathBuf))
                        .required(true)
                        .index(1),
                ),
        )
        .subcommand(
            Command::new("detect-anomalies")
                .about("Detect anomalies in time series data using MAD, CUSUM, and FFT analysis")
                .long_about(
                    "Detect anomalies in time series data using MAD, CUSUM, and FFT analysis.\n\n\
                     If QUERY is provided, analyzes that specific metric.\n\
                     If QUERY is omitted, performs exhaustive analysis on all metrics in the recording."
                )
                .arg(
                    clap::Arg::new("FILE")
                        .help("Parquet file to analyze")
                        .value_parser(clap::value_parser!(PathBuf))
                        .required(true)
                        .index(1),
                )
                .arg(
                    clap::Arg::new("QUERY")
                        .help(
                            "Optional PromQL query or metric name (e.g., 'cpu_usage' or 'sum(rate(cpu_cycles[1m]))')\n\
                             If omitted, analyzes all metrics in the recording",
                        )
                        .required(false)
                        .index(2),
                ),
        )
        .subcommand(
            Command::new("query")
                .about("Execute a PromQL query against a recording and display results")
                .long_about(
                    "Execute a PromQL query against a recording and display results.\n\n\
                     For example queries and patterns, run 'describe-metrics' first to see\n\
                     available metrics and common query examples."
                )
                .arg(
                    clap::Arg::new("FILE")
                        .help("Parquet file to query")
                        .value_parser(clap::value_parser!(PathBuf))
                        .required(true)
                        .index(1),
                )
                .arg(
                    clap::Arg::new("QUERY")
                        .help("PromQL query (e.g., 'sum(rate(cpu_cycles[1m]))')")
                        .required(true)
                        .index(2),
                ),
        )
}
