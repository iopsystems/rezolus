use crate::*;

use clap::{ArgMatches, Command};
use std::path::PathBuf;

pub mod anomaly_detection;
mod backend;
pub mod correlation;
mod describe_metrics;
mod server;

// Legacy PromQL/Tsdb types — only used by `format_recording_info`,
// `run_exhaustive_detection`, and `format_query_result` below, which
// are themselves cfg-gated to `live-mode`. Once those functions are
// audited and deleted, this import block goes with them.
#[cfg(feature = "live-mode")]
use crate::viewer::promql::{QueryEngine, QueryResult};
#[cfg(feature = "live-mode")]
use crate::viewer::tsdb::Tsdb;
use chrono::{DateTime, Utc};

/// Format recording information for display (DuckDB-backed CLI path).
///
/// Mirrors [`format_recording_info`] but pulls metadata from
/// [`crate::viewer::sql_capture::SqlCapture`] (no `Tsdb` involved).
/// The legacy function below stays in place for `src/mcp/server.rs`
/// until that file is migrated too — keeping the layout one-to-one
/// makes the future server.rs migration mechanical.
pub fn format_recording_info_sql(file_path: &str, capture: &crate::viewer::sql_capture::SqlCapture) -> String {
    use dashboard::DashboardData;

    // `time_range()` is `(min, max)` in nanoseconds — convert to
    // float seconds to match the legacy `QueryEngine::get_time_range`
    // contract that the prose template was authored against.
    let (start_time, end_time) = match capture.time_range() {
        Some((lo_ns, hi_ns)) => (lo_ns as f64 / 1e9, hi_ns as f64 / 1e9),
        None => (0.0, 0.0),
    };
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
        capture.version(),
        capture.source(),
        duration_str,
        duration_seconds,
        start_datetime,
        start_time,
        end_datetime,
        end_time
    )
}

/// Format recording information for display
#[cfg(feature = "live-mode")]
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
        Mode::DetectAnomalies { file, query } => run_detect_anomalies(file, query),
        Mode::Query { file, query } => run_query(file, query),
    }
}

fn run_server(config: Config) {
    // configure logging
    let _log_drain = configure_logging(verbosity_to_level(config.verbose));

    // initialize async runtime
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("rezolus")
        .build()
        .expect("failed to launch async runtime");

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
    let (backend, capture) = match backend::open_capture(&file) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("Failed to load parquet file: {e}");
            std::process::exit(1);
        }
    };

    // Auto-resolve bare metric names to SQL (same rule as detect-anomalies).
    let sql1 = match resolve_query_to_sql(&capture, &query1) {
        Some(s) => s,
        None => {
            eprintln!(
                "First query '{query1}' is not a recognised metric and doesn't look like SQL."
            );
            std::process::exit(1);
        }
    };
    let sql2 = match resolve_query_to_sql(&capture, &query2) {
        Some(s) => s,
        None => {
            eprintln!(
                "Second query '{query2}' is not a recognised metric and doesn't look like SQL."
            );
            std::process::exit(1);
        }
    };

    match correlation::calculate_correlation_sql(&backend, &capture, &sql1, &sql2) {
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
    let (_backend, capture) = match backend::open_capture(&file) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("Failed to load parquet file: {e}");
            std::process::exit(1);
        }
    };

    let output = format_recording_info_sql(file.to_str().unwrap_or("<unknown>"), &capture);
    println!("{output}");
}

fn run_describe_metrics(file: PathBuf) {
    let (_backend, capture) = match backend::open_capture(&file) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("Failed to load parquet file: {e}");
            std::process::exit(1);
        }
    };

    let output = describe_metrics::format_metrics_description_sql(&capture);
    println!("{output}");
}

fn run_detect_anomalies(file: PathBuf, query: Option<String>) {
    let (backend, capture) = match backend::open_capture(&file) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("Failed to load parquet file: {e}");
            std::process::exit(1);
        }
    };

    if let Some(input) = query {
        match detect_anomalies_for_input(&backend, &capture, &input) {
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

    // No query provided - run exhaustive anomaly detection
    run_exhaustive_detection_sql(&backend, &capture);
}

/// SQL-backed exhaustive anomaly detection. Mirrors
/// [`run_exhaustive_detection`] but iterates the SqlCapture catalog
/// and uses the SQL builders in [`backend`].
fn run_exhaustive_detection_sql(
    backend: &metriken_query_sql::DuckDbBackend,
    capture: &crate::viewer::sql_capture::SqlCapture,
) {
    use dashboard::DashboardData;

    // Same skip list as the legacy path — these metrics are raw
    // building blocks or NUMA-policy fields where standalone anomaly
    // detection isn't meaningful.
    let skip_metrics = [
        "cpu_tsc",
        "cpu_aperf",
        "cpu_mperf",
        "cgroup_cpu_aperf",
        "cgroup_cpu_mperf",
        "memory_numa_hit",
        "memory_numa_miss",
        "memory_numa_other",
        "memory_numa_interleave",
        "cgroup_cpu_bandwidth_periods",
        "cgroup_cpu_bandwidth_period_duration",
        "cgroup_cpu_bandwidth_quota",
    ];

    // (label, kind, prebuilt_sql_or_none)
    // The third entry is `Some` for hand-written derived metrics whose SQL doesn't
    // fall out of a single-metric builder (e.g. a ratio of two rates).
    let mut metrics: Vec<(String, &'static str, Option<String>)> = Vec::new();

    for name in capture.counter_names() {
        if !skip_metrics.contains(&name) {
            metrics.push((name.to_string(), "counter", None));
        }
    }
    for name in capture.gauge_names() {
        if !skip_metrics.contains(&name) {
            metrics.push((name.to_string(), "gauge", None));
        }
    }
    for name in capture.histogram_names() {
        metrics.push((name.to_string(), "histogram_p50", None));
        metrics.push((name.to_string(), "histogram_p90", None));
        metrics.push((name.to_string(), "histogram_p99", None));
    }

    // Derived metrics: ratios of paired counter rates. Each is built
    // only when both contributing metrics exist in the catalog.
    let counters: std::collections::HashSet<&str> = capture.counter_names().into_iter().collect();
    let catalog = capture.catalog();
    if counters.contains("cpu_aperf") && counters.contains("cpu_mperf") {
        if let Some(sql) = counter_ratio_sql(&catalog, "cpu_aperf", "cpu_mperf") {
            metrics.push(("cpu_frequency_ratio".to_string(), "derived", Some(sql)));
        }
    }
    if counters.contains("cpu_instructions") && counters.contains("cpu_cycles") {
        if let Some(sql) = counter_ratio_sql(&catalog, "cpu_instructions", "cpu_cycles") {
            metrics.push((
                "cpu_instructions_per_cycle".to_string(),
                "derived",
                Some(sql),
            ));
        }
    }
    if counters.contains("cgroup_cpu_aperf") && counters.contains("cgroup_cpu_mperf") {
        if let Some(sql) = counter_ratio_sql(&catalog, "cgroup_cpu_aperf", "cgroup_cpu_mperf") {
            metrics.push((
                "cgroup_cpu_frequency_ratio".to_string(),
                "derived",
                Some(sql),
            ));
        }
    }
    if counters.contains("cgroup_cpu_instructions") && counters.contains("cgroup_cpu_cycles") {
        if let Some(sql) =
            counter_ratio_sql(&catalog, "cgroup_cpu_instructions", "cgroup_cpu_cycles")
        {
            metrics.push((
                "cgroup_cpu_instructions_per_cycle".to_string(),
                "derived",
                Some(sql),
            ));
        }
    }

    println!(
        "Exhaustive Anomaly Detection\n\
         ============================\n\
         Analyzing {} metrics from recording\n",
        metrics.len()
    );

    let mut total_anomalies = 0;
    let mut metrics_with_anomalies = Vec::new();
    let path_str = capture.parquet_path().to_string_lossy().to_string();
    let step = capture.interval();

    for (metric_name, metric_type, custom_sql) in &metrics {
        let sql = match (custom_sql, *metric_type) {
            (Some(s), _) => s.clone(),
            (None, "counter") => match backend::counter_sum_rate_sql(&catalog, metric_name) {
                Some(s) => s,
                None => continue,
            },
            (None, "gauge") => match backend::gauge_sum_sql(&catalog, metric_name) {
                Some(s) => s,
                None => continue,
            },
            (None, "histogram_p50") => {
                match backend::histogram_quantile_sql(&catalog, metric_name, 0.50) {
                    Some(s) => s,
                    None => continue,
                }
            }
            (None, "histogram_p90") => {
                match backend::histogram_quantile_sql(&catalog, metric_name, 0.90) {
                    Some(s) => s,
                    None => continue,
                }
            }
            (None, "histogram_p99") => {
                match backend::histogram_quantile_sql(&catalog, metric_name, 0.99) {
                    Some(s) => s,
                    None => continue,
                }
            }
            _ => continue,
        };

        let result = backend
            .run_sql(&sql, &path_str)
            .ok()
            .map(|batches| backend::batches_to_series(&batches))
            .map(|series| reduce_series_to_pair(&series))
            .and_then(|(ts, vs)| {
                if vs.is_empty() {
                    None
                } else {
                    anomaly_detection::analyze_time_series(sql.clone(), ts, vs, step).ok()
                }
            });

        if let Some(result) = result {
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
                    .filter(|a| matches!(a.severity, anomaly_detection::AnomalySeverity::Medium))
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
    }

    print_exhaustive_summary(metrics.len(), total_anomalies, &mut metrics_with_anomalies);
}

/// Build the SQL for `sum(rate(num)) / sum(rate(den))` against `_src`.
/// Both metrics must have at least one physical column; returns `None`
/// otherwise.
fn counter_ratio_sql(
    catalog: &metriken_query_sql::MetricCatalog,
    numerator: &str,
    denominator: &str,
) -> Option<String> {
    let num_series = catalog.series_by_metric.get(numerator)?;
    let den_series = catalog.series_by_metric.get(denominator)?;
    if num_series.is_empty() || den_series.is_empty() {
        return None;
    }
    let num_expr = num_series
        .iter()
        .map(|s| {
            format!(
                "COALESCE(irate_1s(\"{}\", timestamp), 0)",
                s.physical.replace('"', "\"\"")
            )
        })
        .collect::<Vec<_>>()
        .join(" + ");
    let den_expr = den_series
        .iter()
        .map(|s| {
            format!(
                "COALESCE(irate_1s(\"{}\", timestamp), 0)",
                s.physical.replace('"', "\"\"")
            )
        })
        .collect::<Vec<_>>()
        .join(" + ");
    Some(format!(
        "SELECT CAST(timestamp / 1e9 AS DOUBLE) AS t, ({num_expr}) / NULLIF({den_expr}, 0) AS v FROM _src ORDER BY t"
    ))
}

/// Shared summary printer for both legacy and SQL exhaustive paths.
fn print_exhaustive_summary(
    total_metrics: usize,
    total_anomalies: usize,
    metrics_with_anomalies: &mut Vec<(String, String, usize, usize, usize, usize)>,
) {
    println!("\nSummary");
    println!("=======");
    println!(
        "Analyzed {} metrics, found anomalies in {} metrics",
        total_metrics,
        metrics_with_anomalies.len()
    );
    println!("Total anomalies detected: {}\n", total_anomalies);

    if !metrics_with_anomalies.is_empty() {
        println!("Metrics with Anomalies:");
        println!("----------------------");
        metrics_with_anomalies.sort_by_key(|k| std::cmp::Reverse(k.2));
        for (metric, metric_type, total, high, medium, low) in metrics_with_anomalies {
            let type_label = match metric_type.as_str() {
                "counter" => "COUNTER",
                "gauge" => "GAUGE",
                "histogram_p50" => "HISTOGRAM (p50)",
                "histogram_p90" => "HISTOGRAM (p90)",
                "histogram_p99" => "HISTOGRAM (p99)",
                "derived" => "DERIVED",
                _ => metric_type.as_str(),
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

#[cfg(feature = "live-mode")]
#[allow(dead_code)] // legacy Tsdb-backed exhaustive detection;
                    // run_exhaustive_detection_sql is the live path.
fn run_exhaustive_detection(engine: Arc<QueryEngine>, tsdb: Arc<Tsdb>) {
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

    for name in tsdb.counter_names() {
        if !skip_metrics.contains(&name) {
            metrics_to_analyze.push((name.to_string(), "counter", None));
        }
    }

    for name in tsdb.gauge_names() {
        if !skip_metrics.contains(&name) {
            metrics_to_analyze.push((name.to_string(), "gauge", None));
        }
    }

    for name in tsdb.histogram_names() {
        metrics_to_analyze.push((name.to_string(), "histogram_p50", None));
        metrics_to_analyze.push((name.to_string(), "histogram_p90", None));
        metrics_to_analyze.push((name.to_string(), "histogram_p99", None));
    }

    // Add derived metrics that combine raw counters into meaningful calculations
    let mut derived_metrics = Vec::new();

    // CPU Frequency = (aperf / mperf) - shows actual vs max performance
    if tsdb.counter_names().contains(&"cpu_aperf") && tsdb.counter_names().contains(&"cpu_mperf") {
        derived_metrics.push((
            "cpu_frequency_ratio".to_string(),
            "derived",
            Some("sum(rate(cpu_aperf[1m])) / sum(rate(cpu_mperf[1m]))".to_string()),
        ));
    }

    // CPU Instructions Per Cycle (IPC) - efficiency metric
    if tsdb.counter_names().contains(&"cpu_instructions")
        && tsdb.counter_names().contains(&"cpu_cycles")
    {
        derived_metrics.push((
            "cpu_instructions_per_cycle".to_string(),
            "derived",
            Some("sum(rate(cpu_instructions[1m])) / sum(rate(cpu_cycles[1m]))".to_string()),
        ));
    }

    // Cgroup versions of the same
    if tsdb.counter_names().contains(&"cgroup_cpu_aperf")
        && tsdb.counter_names().contains(&"cgroup_cpu_mperf")
    {
        derived_metrics.push((
            "cgroup_cpu_frequency_ratio".to_string(),
            "derived",
            Some("sum(rate(cgroup_cpu_aperf[1m])) / sum(rate(cgroup_cpu_mperf[1m]))".to_string()),
        ));
    }

    if tsdb.counter_names().contains(&"cgroup_cpu_instructions")
        && tsdb.counter_names().contains(&"cgroup_cpu_cycles")
    {
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
        // Use custom query if provided, otherwise construct based on type
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

        match anomaly_detection::detect_anomalies(&engine, &tsdb, &query) {
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
    match execute_query(&file, &query) {
        Ok(text) => println!("{text}"),
        Err(e) => {
            eprintln!("Query failed: {e}");
            std::process::exit(1);
        }
    }
}

/// Run a SQL query against a parquet and return the formatted text
/// table. Pulled out of [`run_query`] so it's testable without the
/// `process::exit` exit-handling around the CLI wrapper.
///
/// The parquet is exposed as the `_src` view; SHARED_MACROS are
/// pre-registered (see metriken-query-sql/src/shared_macros.sql).
/// Any well-formed `RecordBatch` schema renders — the user owns the
/// projection.
pub(crate) fn execute_query(
    file: &std::path::Path,
    query: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let (backend, capture) = backend::open_capture(file)?;
    let path_str = capture.parquet_path().to_string_lossy().to_string();
    let batches = backend.run_sql(query, &path_str)?;
    Ok(arrow::util::pretty::pretty_format_batches(&batches)?.to_string())
}

/// Resolve a `detect-anomalies` / `analyze-correlation` input string
/// into the SQL the engine will execute.
///
/// Bare metric names auto-resolve based on what kind the metric is
/// in `capture`'s catalog:
/// - **counter** → `sum(rate(M[1m]))` equivalent — `counter_sum_rate_sql`.
/// - **gauge**   → `sum(M)` equivalent          — `gauge_sum_sql`.
/// - **histogram** → `histogram_quantile(0.99, M)` equivalent —
///   `histogram_quantile_sql(M, 0.99)`. (p99 matches the legacy
///   `auto_construct_query` default.)
///
/// Anything that already looks like SQL (contains `SELECT`,
/// whitespace, or operator characters) passes through unchanged.
/// Mirrors the heuristic in `anomaly_detection::auto_construct_query`.
pub(crate) fn resolve_query_to_sql(
    capture: &crate::viewer::sql_capture::SqlCapture,
    input: &str,
) -> Option<String> {
    use dashboard::DashboardData;

    let trimmed = input.trim();
    let is_bare = !trimmed.contains('(')
        && !trimmed.contains('[')
        && !trimmed.contains('{')
        && !trimmed.chars().any(|c| c.is_whitespace())
        && !trimmed.contains('/')
        && !trimmed.contains('+')
        && !trimmed.contains('-')
        && !trimmed.contains('*');
    if !is_bare {
        return Some(trimmed.to_string());
    }

    let catalog = capture.catalog();
    if capture.counter_names().contains(&trimmed) {
        backend::counter_sum_rate_sql(&catalog, trimmed)
    } else if capture.gauge_names().contains(&trimmed) {
        backend::gauge_sum_sql(&catalog, trimmed)
    } else if capture.histogram_names().contains(&trimmed) {
        backend::histogram_quantile_sql(&catalog, trimmed, 0.99)
    } else {
        None
    }
}

/// Run anomaly detection on a single SQL/metric input. Returns the
/// label to render alongside the result (the SQL we actually ran)
/// plus the analysis.
pub(crate) fn detect_anomalies_for_input(
    backend: &metriken_query_sql::DuckDbBackend,
    capture: &crate::viewer::sql_capture::SqlCapture,
    input: &str,
) -> Result<anomaly_detection::AnomalyDetectionResult, Box<dyn std::error::Error>> {
    use dashboard::DashboardData;
    let sql = resolve_query_to_sql(capture, input).ok_or_else(|| {
        format!(
            "'{}' is not a recognised metric name and doesn't look like SQL. \
             Pass either a bare metric name (run `describe-metrics` for the \
             catalogue) or a full SQL string projecting `t DOUBLE, v DOUBLE`.",
            input
        )
    })?;
    let path_str = capture.parquet_path().to_string_lossy().to_string();
    let batches = backend.run_sql(&sql, &path_str)?;
    let series = backend::batches_to_series(&batches);
    let (timestamps, values) = reduce_series_to_pair(&series);
    let step = capture.interval();
    anomaly_detection::analyze_time_series(sql, timestamps, values, step)
}

/// Collapse a `Vec<Series>` into `(timestamps, values)`. Mirrors the
/// `extract_time_series` Matrix branch in `anomaly_detection/mod.rs`:
/// if multiple series, sum by timestamp; otherwise pass through the
/// single series. The output is sorted by timestamp so the
/// statistical analyses (Allan, MAD, CUSUM) see a monotonic axis.
fn reduce_series_to_pair(series: &[backend::Series]) -> (Vec<f64>, Vec<f64>) {
    if series.is_empty() {
        return (Vec::new(), Vec::new());
    }
    if series.len() == 1 {
        let mut pts: Vec<(f64, f64)> = series[0].values.clone();
        pts.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        let timestamps = pts.iter().map(|(t, _)| *t).collect();
        let values = pts.into_iter().map(|(_, v)| v).collect();
        return (timestamps, values);
    }
    // Sum by timestamp across multiple series.
    let mut by_ts: std::collections::BTreeMap<u64, f64> = std::collections::BTreeMap::new();
    for s in series {
        for (t, v) in &s.values {
            let key = (t * 1e6) as u64; // microsecond keys so we don't lose 1Hz precision
            *by_ts.entry(key).or_insert(0.0) += v;
        }
    }
    let timestamps: Vec<f64> = by_ts.keys().map(|k| *k as f64 / 1e6).collect();
    let values: Vec<f64> = by_ts.values().copied().collect();
    (timestamps, values)
}

#[cfg(feature = "live-mode")]
#[allow(dead_code)] // retained behind live-mode; deletable once the legacy
                    // QueryResult-formatting code path is audited away.
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

                    // Calculate basic stats
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

#[cfg(feature = "live-mode")]
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

#[cfg(test)]
mod tests {
    use super::*;

    fn demo_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("site")
            .join("viewer")
            .join("data")
            .join("demo.parquet")
    }

    /// End-to-end: `execute_query` runs a scalar SELECT and the
    /// pretty-printed output contains the expected count cell.
    /// `demo.parquet` is a 301-second recording at 1Hz, so `_src` has
    /// 302 rows (302 timestamps including the trailing one).
    #[test]
    fn execute_query_scalar_count() {
        let path = demo_path();
        if !path.exists() {
            eprintln!("skipping: fixture {} missing", path.display());
            return;
        }
        let text = execute_query(&path, "SELECT count(*) AS n FROM _src")
            .expect("scalar SELECT runs");
        // `arrow::util::pretty::pretty_format_batches` renders this as
        // a 3-row text table with a single "n" column and a 302 value.
        assert!(text.contains("| n"), "missing 'n' header: {text}");
        assert!(text.contains("302"), "missing expected count: {text}");
    }

    /// `resolve_query_to_sql` auto-constructs SQL for bare metric
    /// names by looking up their kind in the capture. Pinned: a
    /// known counter resolves to a counter-rate SQL; a known gauge
    /// resolves to a gauge-sum SQL; a known histogram resolves to
    /// a p99 quantile SQL. Anything that already looks like SQL
    /// passes through unchanged.
    #[test]
    fn resolve_query_to_sql_dispatches_by_metric_kind() {
        let path = demo_path();
        if !path.exists() {
            eprintln!("skipping: fixture {} missing", path.display());
            return;
        }
        let (_backend, capture) = backend::open_capture(&path).expect("open");

        // Counter → contains irate_1s.
        let counter_sql = resolve_query_to_sql(&capture, "cpu_cycles").expect("cpu_cycles counter");
        assert!(counter_sql.contains("irate_1s"), "counter SQL: {counter_sql}");

        // Gauge → no rate fn, just COALESCE(col, 0).
        let gauge_sql = resolve_query_to_sql(&capture, "cpu_cores").expect("cpu_cores gauge");
        assert!(gauge_sql.contains("COALESCE"), "gauge SQL: {gauge_sql}");
        assert!(!gauge_sql.contains("irate_1s"), "gauge must not use rate: {gauge_sql}");

        // Pass-through: a SQL string contains characters bare names can't.
        let user_sql = "SELECT count(*) AS v FROM _src";
        let pass = resolve_query_to_sql(&capture, user_sql).expect("pass-through");
        assert_eq!(pass, user_sql);

        // Unknown bare name → None.
        assert!(resolve_query_to_sql(&capture, "definitely_not_a_metric").is_none());
    }

    /// End-to-end: `analyze-correlation` via the SQL pipeline returns
    /// a non-empty result with finite correlation values. cpu_cycles
    /// and cpu_instructions are tightly coupled on demo.parquet
    /// (CPUs run instructions to generate cycles), so we expect a
    /// strong positive correlation. The threshold (0.5) is loose to
    /// survive small numerical drift between recordings.
    #[test]
    fn analyze_correlation_via_sql_pipeline() {
        let path = demo_path();
        if !path.exists() {
            eprintln!("skipping: fixture {} missing", path.display());
            return;
        }
        let (backend, capture) = backend::open_capture(&path).expect("open");
        let sql1 = resolve_query_to_sql(&capture, "cpu_cycles").expect("cpu_cycles");
        let sql2 = resolve_query_to_sql(&capture, "cpu_instructions").expect("cpu_instructions");
        let result =
            correlation::calculate_correlation_sql(&backend, &capture, &sql1, &sql2)
                .expect("correlation runs");
        assert!(result.sample_count > 0, "have sample points");
        assert!(
            result.max_correlation.is_finite(),
            "correlation is a finite number"
        );
        assert!(
            result.max_correlation.abs() > 0.5,
            "cpu_cycles ↔ cpu_instructions should be strongly correlated; got {}",
            result.max_correlation
        );
    }

    /// End-to-end: detect_anomalies_for_input on a real metric runs
    /// the SQL builder + DuckDB + analysis pipeline and returns a
    /// populated result. Pinned because this is the integration
    /// path the `mcp detect-anomalies <file> <metric>` CLI exercises.
    #[test]
    fn detect_anomalies_for_input_runs_end_to_end() {
        let path = demo_path();
        if !path.exists() {
            eprintln!("skipping: fixture {} missing", path.display());
            return;
        }
        let (backend, capture) = backend::open_capture(&path).expect("open");
        let result =
            detect_anomalies_for_input(&backend, &capture, "cpu_cores").expect("analyse cpu_cores");
        assert!(result.total_points > 0, "got data points");
        assert_eq!(
            result.total_points,
            result.values.len(),
            "total_points matches values len"
        );
        // The query field should record the executed SQL, not the bare
        // metric name — that's the user-visible audit trail.
        assert!(
            result.query.contains("FROM _src"),
            "query records executed SQL: {}",
            result.query
        );
    }

    /// `execute_query` surfaces DuckDB errors rather than panicking
    /// on a malformed query. Pinned because the CLI's `run_query`
    /// catches `Err` and exits with code 1, so a panic here would
    /// regress the user-visible error UX.
    #[test]
    fn execute_query_propagates_sql_errors() {
        let path = demo_path();
        if !path.exists() {
            eprintln!("skipping: fixture {} missing", path.display());
            return;
        }
        let err = execute_query(&path, "SELECT bogus_column FROM _src").expect_err("must fail");
        let msg = err.to_string();
        // DuckDB's binder reports the missing column by name. We don't
        // assert on the exact phrasing (it can shift across DuckDB
        // versions), but the column name should land in the message.
        assert!(
            msg.to_lowercase().contains("bogus_column") || msg.to_lowercase().contains("binder"),
            "expected binder error mentioning bogus_column or 'binder', got: {msg}",
        );
    }
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
                    "Analyze correlation between two metrics. Each query is either a bare\n\
                     metric name (auto-resolved to the canonical rate/sum/quantile SQL based\n\
                     on its kind) or a full DuckDB SQL string projecting `t DOUBLE, v DOUBLE`."
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
                        .help("First metric name or DuckDB SQL string")
                        .required(true)
                        .index(2),
                )
                .arg(
                    clap::Arg::new("QUERY2")
                        .help("Second metric name or DuckDB SQL string")
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
                            "Optional bare metric name (auto-resolved to SQL based on its kind)\n\
                             or full DuckDB SQL projecting `t DOUBLE, v DOUBLE`. Omit for an\n\
                             exhaustive sweep across every metric in the recording.",
                        )
                        .required(false)
                        .index(2),
                ),
        )
        .subcommand(
            Command::new("query")
                .about("Execute a DuckDB SQL query against a recording and display results")
                .long_about(
                    "Execute a DuckDB SQL query against a recording and display results.\n\n\
                     The parquet is exposed as the `_src` table. Shared macros from the\n\
                     dashboard (irate_1s, rate_5m, hist_p99, …) are pre-registered.\n\
                     Example: SELECT timestamp/1e9 AS t, \"cpu_usage/user/0\"::DOUBLE AS v FROM _src ORDER BY t\n\n\
                     For schema introspection (column names, metric types), run 'describe-metrics'."
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
                        .help("DuckDB SQL query (e.g., 'SELECT count(*) FROM _src')")
                        .required(true)
                        .index(2),
                ),
        )
}
