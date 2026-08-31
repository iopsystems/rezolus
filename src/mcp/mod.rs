use crate::*;

use clap::{ArgMatches, Command};
use std::path::PathBuf;

mod recording_selector;
// SelectError is unused until Task 2 (`RecordingSelector::resolve`) consumes it.
#[allow(unused_imports)]
pub(crate) use recording_selector::{RecordingSelector, SelectError};

pub mod anomaly_detection;
pub mod correlation;
mod describe_metrics;
mod server;

use chrono::{DateTime, Utc};
use metriken_query::{MetricsSource, QueryResult};

/// Open a recording as a `MetricsSource`, dispatching `.rez` archives to
/// `RezReader` and everything else to `ParquetReader` (by content, not extension).
/// Dispatch covers both `.rez` containers (v2 tar and v3 SQLite) — `RezReader`
/// itself dispatches on the container internally.
/// Open a recording as a query source, refusing what the analysis tools cannot
/// read.
///
/// `pool` is shared so the stdio server and the one-shot CLI use one budget.
pub(crate) fn open_source_with_pool(
    file: &std::path::Path,
    pool: std::sync::Arc<metriken_query::BufferPool>,
) -> Result<std::sync::Arc<dyn metriken_query::MetricsSource>, Box<dyn std::error::Error>> {
    use crate::recorder::rez::RezFormat;
    if crate::recorder::rez::detect_rez_format(file).unwrap_or(RezFormat::NotRez)
        == RezFormat::NotRez
    {
        return Ok(std::sync::Arc::new(
            metriken_query::ParquetReader::open_with_pool(file, pool)?,
        ));
    }

    // Open the recordings ONCE and build the reader from what comes back.
    //
    // `open_with_pool` would flatten them into one view, and two recordings of
    // the same agent then give every sampler two owners, so the reader refuses
    // each query as cross-recording — deliberately, since silently answering
    // from one arm of an A/B is the worse failure. But the analysis tools fold
    // a per-metric query error into `NoData`, so that refusal surfaces as
    // "analyzed 41 metrics, found anomalies in 0": a clean-looking wrong
    // answer, on the shape `record --endpoint a --endpoint b -o out.rez` now
    // produces by default. So refuse here, where the message reaches the
    // caller.
    //
    // Counting via a second open cost a measured 2x on every invocation —
    // neither container's probe is catalog-only (v3 reads a segment per table,
    // tar reads the whole archive into memory). Consuming the recordings we
    // already have avoids that. The only field this loses versus
    // `open_with_pool` is `filename`, which nothing under `src/mcp/` reads.
    let mut recordings = crate::rez_reader::RezReader::open_recordings(file, pool)?;

    // Count only recordings that actually hold tables. An arm that produced no
    // rows cannot collide with anything, and the flattened reader answers such
    // an archive correctly — refusing it would reject exactly the run this
    // branch's own endpoint-activation fix is about.
    let with_tables = recordings.iter().filter(|(_, r)| !r.is_empty()).count();
    if with_tables > 1 {
        return Err(format!(
            "{} holds {with_tables} recordings with data (a multi-host or A/B archive), \
             and the analysis tools read one recording at a time. Re-record the endpoint \
             you want on its own (`record --endpoint <one> -o one.rez`), or open it in \
             `rezolus view`, which reads two recordings as a comparison",
            file.display()
        )
        .into());
    }

    // The one with data, else the first — an all-empty archive still opens, and
    // reports an empty recording rather than erroring.
    let idx = recordings
        .iter()
        .position(|(_, r)| !r.is_empty())
        .unwrap_or(0);
    if idx >= recordings.len() {
        return Err(format!("{} holds no recordings", file.display()).into());
    }
    let (_, reader) = recordings.swap_remove(idx);
    Ok(std::sync::Arc::new(reader))
}

pub(crate) fn open_source(
    file: &std::path::Path,
) -> Result<std::sync::Arc<dyn metriken_query::MetricsSource>, Box<dyn std::error::Error>> {
    open_source_with_pool(file, metriken_query::BufferPool::new(256 * 1024 * 1024))
}

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
        Mode::ExtractFeatures { file } => run_extract_features(file),
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
    let reader = match open_source(&file) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Failed to load parquet file: {e}");
            std::process::exit(1);
        }
    };

    match correlation::calculate_correlation(reader.as_ref(), &query1, &query2) {
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
    let reader = match open_source(&file) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Failed to load parquet file: {e}");
            std::process::exit(1);
        }
    };

    let output = format_recording_info(file.to_str().unwrap_or("<unknown>"), reader.as_ref());
    println!("{output}");
}

fn run_describe_metrics(file: PathBuf) {
    let reader = match open_source(&file) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Failed to load parquet file: {e}");
            std::process::exit(1);
        }
    };

    let output = describe_metrics::format_metrics_description(reader.as_ref());
    println!("{output}");
}

fn run_detect_anomalies(file: PathBuf, query: Option<String>) {
    let reader = match open_source(&file) {
        Ok(r) => r,
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

fn run_exhaustive_detection(reader: Arc<dyn MetricsSource>) {
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
    let reader = match open_source(&file) {
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

fn run_extract_features(file: PathBuf) {
    let reader = match open_source(&file) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Failed to open recording: {e}");
            std::process::exit(1);
        }
    };

    match crate::analysis::extract::extract(reader.as_ref()) {
        Ok(record) => match serde_json::to_string_pretty(&record) {
            Ok(json) => println!("{json}"),
            Err(e) => {
                eprintln!("Failed to serialize record: {e}");
                std::process::exit(1);
            }
        },
        Err(e) => {
            eprintln!("Feature extraction failed: {e}");
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
                let bound = sample
                    .interval
                    .map(|(lo, hi)| format!("  [{lo:.6}, {hi:.6}]"))
                    .unwrap_or_default();
                writeln!(
                    &mut output,
                    "{} = {}{}",
                    format_metric(&sample.metric),
                    sample.value.1,
                    bound
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
                    // Acquisition-window uncertainty bounds (rate()/irate() only).
                    let ivl = series.intervals.as_ref();
                    let bound = |i: usize| -> String {
                        ivl.and_then(|v| v.get(i))
                            .map(|(lo, hi)| format!("  [{lo:.6}, {hi:.6}]"))
                            .unwrap_or_default()
                    };
                    writeln!(
                        &mut output,
                        "  First: {} = {}{}",
                        first.0,
                        first.1,
                        bound(0)
                    )
                    .unwrap();
                    writeln!(
                        &mut output,
                        "  Last:  {} = {}{}",
                        last.0,
                        last.1,
                        bound(series.values.len() - 1)
                    )
                    .unwrap();
                    if ivl.is_some() {
                        writeln!(&mut output, "  (bounds = acquisition-window uncertainty)")
                            .unwrap();
                    }

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
    /// Extract structured features from a recording as JSON
    ExtractFeatures {
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
            Some(("extract-features", sub_args)) => {
                let file = sub_args
                    .get_one::<PathBuf>("FILE")
                    .ok_or("File argument is required")?
                    .clone();
                Mode::ExtractFeatures { file }
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
            "AI-assisted analysis of a recording — a .parquet file or a .rez archive, told\n\
             apart by content rather than by name. With no subcommand, runs as a Model\n\
             Context Protocol server over stdio for an LLM client. Each subcommand also runs\n\
             one-shot from the CLI, printing the same analysis to stdout.\n\n\
             SUBCOMMANDS:\n    \
             describe-recording   Summarize a recording (source, version, time range, duration)\n    \
             describe-metrics     List every metric in a recording with its type and labels\n    \
             query                Run a PromQL query against a recording\n    \
             detect-anomalies     Flag anomalies for one metric, or exhaustively across all\n    \
             analyze-correlation  Correlate two PromQL series over the recording\n    \
             extract-features     Extract structured features from a recording as JSON\n\n\
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
                        .help("Recording to analyze: a .parquet file or a .rez archive")
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
                        .help("Recording to describe: a .parquet file or a .rez archive")
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
                        .help("Recording to analyze: a .parquet file or a .rez archive")
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
                     If QUERY is omitted, performs exhaustive analysis on all metrics in the recording.\n\n\
                     EXAMPLES:\n    \
                     # Sweep every metric in the recording\n    \
                     rezolus mcp detect-anomalies out.rez\n\n    \
                     # Focus on one metric\n    \
                     rezolus mcp detect-anomalies out.rez cpu_usage"
                )
                .arg(
                    clap::Arg::new("FILE")
                        .help("Recording to analyze: a .parquet file or a .rez archive")
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
                     available metrics and common query examples.\n\n\
                     rate()/irate() values print an acquisition-window uncertainty band\n\
                     [lo, hi] next to the value (derived from per-observation acquisition\n\
                     windows). A scalar op scales the band (e.g. rate(x)*k), and a\n\
                     series-op-series combines both operands' bands by interval arithmetic —\n\
                     widening first to the union span when the two came from different\n\
                     acquisition tables. Queries with no rate() and no histogram have no band\n\
                     to show.\n\n\
                     EXAMPLES:\n    \
                     # Total cycles per second across all CPUs\n    \
                     rezolus mcp query out.rez 'sum(rate(cpu_cycles[1m]))'\n\n    \
                     # A ratio: both operands' bands are combined\n    \
                     rezolus mcp query out.rez 'sum(irate(cpu_usage[1m])) / cpu_cores'"
                )
                .arg(
                    clap::Arg::new("FILE")
                        .help("Recording to query: a .parquet file or a .rez archive")
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
        .subcommand(
            Command::new("extract-features")
                .about("Extract structured features from a recording as JSON")
                .long_about(
                    "Produce a deterministic, versioned overview record of a recording's \
                     Rezolus-native features (per-metric stats, noise class, anomalies, \
                     regime shifts, acquisition-window uncertainty, correlations, resource \
                     rankings, subsystem coverage) as JSON on stdout. The record is the \
                     input half of a recording assessment. Requires a recording of at \
                     least 10 seconds.\n\n\
                     EXAMPLES:\n    \
                     # Emit the feature record\n    \
                     rezolus mcp extract-features out.rez\n\n    \
                     # Keep it for an assessment step\n    \
                     rezolus mcp extract-features out.rez > features.json",
                )
                .arg(
                    clap::Arg::new("FILE")
                        .help("Recording to analyze: a .parquet file or a .rez archive")
                        .value_parser(clap::value_parser!(PathBuf))
                        .required(true)
                        .index(1),
                ),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recorder::rez::RezRecorder;
    use metriken::Window;
    use metriken_exposition::{Counter, Snapshot, SnapshotV2};
    use std::collections::HashMap;
    use std::time::SystemTime;

    #[test]
    fn format_query_result_shows_rate_bounds() {
        use metriken_query::{MatrixSample, QueryResult};
        let mut metric = HashMap::new();
        metric.insert("__name__".to_string(), "rate".to_string());
        let with = QueryResult::Matrix {
            result: vec![
                MatrixSample::new(metric.clone(), vec![(1.0, 300.0), (2.0, 310.0)])
                    .with_intervals(Some(vec![(291.26, 306.12), (300.0, 320.0)])),
            ],
        };
        let s = format_query_result(&with);
        assert!(s.contains("[291.26"), "expected bound in output: {s}");
        assert!(s.contains("acquisition-window uncertainty"), "{s}");

        // No intervals → no bound text.
        let without = QueryResult::Matrix {
            result: vec![MatrixSample::new(metric, vec![(1.0, 300.0)])],
        };
        let s2 = format_query_result(&without);
        assert!(!s2.contains('['), "no bounds expected: {s2}");
    }

    fn counter(name: &str, sampler: &str, v: u64, w: Option<Window>) -> Counter {
        Counter::new(
            name.to_string(),
            v,
            [
                ("metric".to_string(), name.to_string()),
                ("sampler".to_string(), sampler.to_string()),
            ]
            .into_iter()
            .collect(),
        )
        .with_window(w)
    }

    fn snap(ts: u64, counters: Vec<Counter>) -> Snapshot {
        Snapshot::V2(SnapshotV2 {
            systemtime: SystemTime::UNIX_EPOCH + std::time::Duration::from_nanos(ts),
            duration: std::time::Duration::ZERO,
            metadata: HashMap::new(),
            counters,
            gauges: Vec::new(),
            histograms: Vec::new(),
        })
    }

    /// Populate a recorder with a few rows of a single-sampler counter.
    fn build_recorder() -> RezRecorder {
        let mut r = RezRecorder::new(
            [("source".to_string(), "rezolus".to_string())]
                .into_iter()
                .collect(),
            [("source".to_string(), "rezolus".to_string())]
                .into_iter()
                .collect(),
            "rezolus".to_string(),
        );
        for i in 0..3u64 {
            let ts = 1_000_000_000 * (i + 1);
            let w = Some(Window::new(ts - 50_000_000, ts));
            r.ingest(
                &snap(ts, vec![counter("cpu_cycles", "cpu_usage", i, w)]),
                ts,
            );
        }
        r
    }

    #[test]
    fn open_source_reads_rez_and_parquet() {
        let dir = tempfile::tempdir().unwrap();

        // Build a .rez archive and open it via open_source.
        let rez_path = dir.path().join("rec.rez");
        build_recorder().finalize(&rez_path).unwrap();
        let rez_source = open_source(&rez_path).unwrap();
        assert!(
            !rez_source.counter_names().is_empty(),
            ".rez source should expose counter names"
        );

        // Build a bare parquet table and open it via open_source.
        let parquet_path = dir.path().join("rec.parquet");
        let tables = build_recorder().finalize_tables();
        let bytes = crate::recorder::rez::write_table_parquet(&tables[0]).unwrap();
        std::fs::write(&parquet_path, bytes).unwrap();
        assert!(
            open_source(&parquet_path).is_ok(),
            "bare parquet should open as a MetricsSource"
        );
    }

    /// Build a v3 archive holding one recording per `source`, each with rows.
    pub(crate) fn multi_recording_rez(
        path: &std::path::Path,
        sources: &[&str],
        with_rows: &[bool],
    ) {
        use crate::recorder::rez_v3_writer::{ManifestSeed, RezArchive, StreamRecorderV3};
        let mut archive = RezArchive::create(path).unwrap();
        let mut recs: Vec<StreamRecorderV3> = Vec::new();
        for source in sources {
            let seed = ManifestSeed {
                labels: [("source".to_string(), source.to_string())]
                    .into_iter()
                    .collect(),
                metadata: Default::default(),
                clock_anchor_wall_ns: 1_000_000_000,
            };
            recs.push(StreamRecorderV3::new(archive.add_recording(seed).unwrap()));
        }
        for (i, rec) in recs.iter_mut().enumerate() {
            if !with_rows.get(i).copied().unwrap_or(true) {
                continue;
            }
            for t in 0..3u64 {
                let ts = 1_000_000_000 * (t + 1);
                let w = Some(Window::new(ts - 50_000_000, ts));
                rec.ingest(
                    &snap(ts, vec![counter("cpu_cycles", "cpu_usage", t, w)]),
                    ts,
                    0,
                )
                .unwrap();
            }
        }
        for rec in recs {
            rec.finalize((4_000_000_000, 0)).unwrap();
        }
        archive.join().unwrap();
    }

    /// A multi-recording archive must be refused HERE, with a message, rather
    /// than opening into a reader that answers nothing.
    ///
    /// `record --endpoint a --endpoint b -o out.rez` now produces this shape
    /// by default. Flattening the recordings gives every sampler two owners,
    /// so the reader refuses each query as cross-recording — which is the
    /// right call, but `extract-features` and `detect-anomalies` fold a
    /// per-metric query error into `NoData`, so the run would report
    /// "analyzed N metrics, found anomalies in 0" and look clean.
    #[test]
    fn open_source_refuses_a_multi_recording_archive_with_a_message() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ab.rez");
        multi_recording_rez(&path, &["redis", "valkey"], &[true, true]);

        let msg = match open_source(&path) {
            Ok(_) => panic!("a 2-recording archive must be refused"),
            Err(e) => e.to_string(),
        };
        assert!(
            msg.contains("2 recordings"),
            "the message must say how many recordings carry data: {msg}"
        );
        assert!(
            msg.contains("rezolus view"),
            "and must point at something that CAN read it: {msg}"
        );
        assert!(
            !msg.contains("recording filter"),
            "must not suggest `filter`, which cannot split by recording: {msg}"
        );
    }

    /// An arm that produced no rows must NOT trigger the refusal.
    ///
    /// It collides with nothing, so the flattened view answers correctly from
    /// the arm that has data. Refusing here would reject exactly the run this
    /// branch's endpoint-activation fix is about — one endpoint up, one that
    /// never produced a sample.
    #[test]
    fn open_source_accepts_an_archive_whose_second_arm_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("one-empty.rez");
        multi_recording_rez(&path, &["redis", "valkey"], &[true, false]);

        let source = open_source(&path).expect("an empty second arm must not refuse");
        assert_eq!(
            source.counter_names(),
            vec!["cpu_cycles".to_string()],
            "and the arm that has data must be the one that is read"
        );
    }

    /// `open_source` must dispatch a v3 (SQLite) `.rez` to `RezReader`, not
    /// `ParquetReader`. Mutation check: reverting the `detect_rez_format`
    /// check in `open_source` to `is_rez_path` makes this fail — `is_rez_path`
    /// (a tar sniff) reports `false` for a SQLite file, so the call falls
    /// through to `ParquetReader::open`, which errors on the SQLite header.
    #[test]
    fn open_source_reads_v3_sqlite_rez() {
        let dir = tempfile::tempdir().unwrap();
        let rez_path = dir.path().join("rec.rez");
        crate::recorder::rez::recorder_tests_support::empty_v3_rez(&rez_path);

        assert_eq!(
            crate::recorder::rez::detect_rez_format(&rez_path).unwrap(),
            crate::recorder::rez::RezFormat::V3Sqlite,
            "fixture sanity: must actually be a v3 SQLite archive"
        );
        let source = open_source(&rez_path);
        assert!(
            source.is_ok(),
            "open_source must accept a v3 .rez: {:?}",
            source.err()
        );
    }
}
