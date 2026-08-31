use crate::*;

use clap::{ArgMatches, Command};
use std::path::PathBuf;

mod recording_selector;
pub(crate) use recording_selector::{
    describe_candidates, render_labels, RecordingSelector, SelectError,
};

pub mod anomaly_detection;
pub mod correlation;
mod describe_metrics;
mod server;

use chrono::{DateTime, Utc};
use metriken_query::{MetricsSource, QueryResult};

/// Open a recording as a query source, selecting ONE recording out of a
/// multi-recording `.rez` archive.
///
/// Dispatch is by content, not extension: a `.rez` container (v2 tar or v3
/// SQLite — `RezReader` sorts out which internally) goes to `RezReader`,
/// anything else to `ParquetReader`.
///
/// `selector` must name exactly one recording. None or several is an error,
/// never a first match and never a default: the analysis tools read one
/// recording at a time, and quietly answering from one arm of an A/B while
/// presenting it as the archive's answer is the failure this exists to
/// prevent.
///
/// `pool` is shared so the stdio server and the one-shot CLI use one budget.
pub(crate) fn open_source_with_pool(
    file: &std::path::Path,
    pool: std::sync::Arc<metriken_query::BufferPool>,
    selector: &RecordingSelector,
) -> Result<std::sync::Arc<dyn metriken_query::MetricsSource>, Box<dyn std::error::Error>> {
    use crate::recorder::rez::RezFormat;
    if crate::recorder::rez::detect_rez_format(file).unwrap_or(RezFormat::NotRez)
        == RezFormat::NotRez
    {
        // Silently ignoring the selector here would be its own wrong answer:
        // the caller believes it narrowed the data and it did not.
        //
        // "not a .rez archive" rather than "is a parquet file": this branch
        // takes everything `detect_rez_format` calls `NotRez`, which includes
        // a text file or a truncated download, and asserting parquet would be
        // a guess. The advice is phrased without naming a flag because the
        // stdio server reaches this with a JSON object, not `--recording`.
        if !selector.is_empty() {
            return Err(format!(
                "{} is not a .rez archive and holds no recordings to select between; \
                 a recording selector applies only to a multi-recording .rez",
                file.display()
            )
            .into());
        }
        return Ok(std::sync::Arc::new(
            metriken_query::ParquetReader::open_with_pool(file, pool)?,
        ));
    }

    // Open the recordings ONCE and build the reader from the one that was
    // selected.
    //
    // `open_with_pool` would flatten them into one view instead, and two
    // recordings of the same agent then give every sampler two owners, so the
    // reader refuses each query as cross-recording — deliberately, since
    // silently answering from one arm is the worse failure. But the analysis
    // tools fold a per-metric query error into `NoData`, so that refusal
    // surfaces as "analyzed 41 metrics, found anomalies in 0": a clean-looking
    // wrong answer, on the shape `record --endpoint a --endpoint b -o out.rez`
    // now produces by default. So the choice is made here, where both the
    // selection and any failure to select reach the caller.
    //
    // Selecting via a second open cost a measured 2x on every invocation —
    // neither container's probe is catalog-only (v3 reads a segment per table,
    // tar reads the whole archive into memory). Consuming the recordings we
    // already have avoids that. The only field this loses versus
    // `open_with_pool` is `filename`, which nothing under `src/mcp/` reads.
    let mut recordings = crate::rez_reader::RezReader::open_recordings(file, pool)?;
    if recordings.is_empty() {
        return Err(format!("{} holds no recordings", file.display()).into());
    }

    // Only recordings that hold tables are candidates. An arm that produced no
    // rows cannot be what the caller meant, and excluding it keeps a run where
    // one endpoint never reported from needing a selector at all.
    //
    // The candidate's index into `recordings` is carried alongside its labels
    // rather than recovered afterwards by matching labels: two recordings may
    // legitimately share a label set (the recorder warns but permits it), and
    // if the duplicate is an EMPTY arm it is not a candidate at all, so a
    // label lookup over the full list could land on it and hand back a reader
    // with no data. Nothing here removes from `recordings` before the index is
    // used, so the indices stay valid.
    //
    // `candidates` is then BOTH the set `resolve` chooses from and the `all`
    // that `describe_candidates` computes uniqueness against. That has to stay
    // one set: a listing built over a wider universe would qualify selectors
    // against recordings this call would never pick, and one built over a
    // narrower universe would advertise a selector that resolves as ambiguous
    // when it is pasted back.
    let candidate_idx: Vec<usize> = recordings
        .iter()
        .enumerate()
        .filter(|(_, (_, r))| !r.is_empty())
        .map(|(i, _)| i)
        .collect();
    let candidates: Vec<std::collections::BTreeMap<String, String>> = candidate_idx
        .iter()
        .map(|&i| recordings[i].0.clone())
        .collect();
    let all_empty = candidates.is_empty();
    if all_empty && selector.is_empty() {
        // Every arm is empty and the caller named none of them. Opening the
        // first reports an empty recording, which is a truer answer than an
        // error about choosing between recordings that all hold nothing.
        let (_, reader) = recordings.swap_remove(0);
        return Ok(std::sync::Arc::new(reader));
    }

    // With every arm empty there is nothing to prefer, so the selector is
    // resolved against ALL of them: a caller that named `source=valkey` gets
    // the valkey reader — empty, but the one it asked about — instead of the
    // first arm wearing valkey's name in every downstream report, since
    // `RezReader` carries the recording's own metadata into
    // `describe-recording` and `extract-features`. Falling through to
    // `swap_remove(0)` here would be exactly the "returned a recording the
    // selector did not name" failure this function refuses everywhere else.
    let (universe_idx, universe) = if all_empty {
        (
            (0..recordings.len()).collect::<Vec<usize>>(),
            recordings
                .iter()
                .map(|(l, _)| l.clone())
                .collect::<Vec<_>>(),
        )
    } else {
        (candidate_idx, candidates)
    };
    // What the listing under an error is a listing OF. Only says "with data"
    // when that is what was filtered on.
    let listing_lead = if all_empty {
        "It holds:"
    } else {
        "Recordings with data:"
    };

    let chosen = match selector.resolve(&universe) {
        Ok(i) => universe_idx[i],
        Err(SelectError::NoMatch) => {
            // The named recording may exist and have been excluded for holding
            // no rows. "No recording matches" would then be false in the way
            // that matters: an agent reports the endpoint was never captured,
            // when it was captured and produced nothing — itself the finding.
            let named_but_empty: Vec<String> = recordings
                .iter()
                .enumerate()
                .filter(|(i, (l, _))| !universe_idx.contains(i) && selector.matches(l))
                .map(|(_, (l, _))| render_labels(l))
                .collect();
            let lead = if named_but_empty.is_empty() {
                format!(
                    "no recording in {} matches {}",
                    file.display(),
                    selector.as_flags()
                )
            } else {
                format!(
                    "{} names {} in {}, which was recorded but produced no rows, so it \
                     holds nothing to analyze",
                    selector.as_flags(),
                    named_but_empty.join("; "),
                    file.display()
                )
            };
            return Err(format!(
                "{lead}. {listing_lead}\n{}",
                describe_candidates(&universe, &[])
            )
            .into());
        }
        Err(SelectError::Ambiguous(hits)) => {
            // `hits` are HIGHLIGHTED, not passed as the candidate list:
            // uniqueness must be computed against every candidate, not just
            // the matched subset, or the listing advertises selectors that are
            // unique among the ones being shown and ambiguous against the rest.
            let lead = if selector.is_empty() {
                // Only reachable when some arm has data, so `hits` counts the
                // recordings WITH DATA — which is not the same as how many the
                // archive holds, and saying "holds 3 recordings" of a 3-arm
                // file with one dead arm would be wrong.
                format!(
                    "{} holds {} recordings with data (a multi-host or A/B archive), and \
                     the analysis tools read one at a time. Pick one:",
                    file.display(),
                    hits.len()
                )
            } else {
                format!(
                    "{} matches {} recordings in {}; add labels until it names one:",
                    selector.as_flags(),
                    hits.len(),
                    file.display()
                )
            };
            return Err(format!("{lead}\n{}", describe_candidates(&universe, &hits)).into());
        }
    };

    let (_, reader) = recordings.swap_remove(chosen);
    Ok(std::sync::Arc::new(reader))
}

/// Open with a selector and a fresh pool — the one-shot CLI path.
pub(crate) fn open_source_selected(
    file: &std::path::Path,
    selector: &RecordingSelector,
) -> Result<std::sync::Arc<dyn metriken_query::MetricsSource>, Box<dyn std::error::Error>> {
    open_source_with_pool(
        file,
        metriken_query::BufferPool::new(256 * 1024 * 1024),
        selector,
    )
}

pub(crate) fn open_source(
    file: &std::path::Path,
) -> Result<std::sync::Arc<dyn metriken_query::MetricsSource>, Box<dyn std::error::Error>> {
    open_source_selected(file, &RecordingSelector::default())
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
        multi_recording_rez_with_arms(path, sources, &vec![None; sources.len()], with_rows)
    }

    /// As `multi_recording_rez`, plus an optional `arm` label per recording.
    ///
    /// `source` is unique per recording and `host` is shared by all of them,
    /// so with those two alone every selector either names one recording or
    /// names them all. An `arm` shared by SOME of the recordings is what makes
    /// a partial match possible, which is the only way to tell a listing that
    /// renders the recordings a selector matched from one that renders every
    /// recording it could have matched.
    pub(crate) fn multi_recording_rez_with_arms(
        path: &std::path::Path,
        sources: &[&str],
        arms: &[Option<&str>],
        with_rows: &[bool],
    ) {
        use crate::recorder::rez_v3_writer::{ManifestSeed, RezArchive, StreamRecorderV3};
        let mut archive = RezArchive::create(path).unwrap();
        let mut recs: Vec<StreamRecorderV3> = Vec::new();
        for (i, source) in sources.iter().enumerate() {
            let mut labels: std::collections::BTreeMap<String, String> = [
                ("source".to_string(), source.to_string()),
                ("host".to_string(), "web-01".to_string()),
            ]
            .into_iter()
            .collect();
            if let Some(arm) = arms.get(i).copied().flatten() {
                labels.insert("arm".to_string(), arm.to_string());
            }
            let seed = ManifestSeed {
                // `host` is shared across the arms on purpose: with `source`
                // as the only label, no selector could ever match two
                // recordings, and the ambiguous path — the one that must not
                // silently fall through to the first arm — would be
                // untestable through this helper. A real multi-endpoint
                // capture of one host's two services looks exactly like this.
                labels,
                // Mirrors `recorder::build_rez_metadata`, which puts the same
                // `source` in the per-recording metadata as in the labels.
                // Tests identify which arm came back by reading it, so a
                // helper that left it empty would make that check vacuous.
                metadata: [("source".to_string(), source.to_string())]
                    .into_iter()
                    .collect(),
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
    ///
    /// Companion to `no_selector_on_a_multi_recording_archive_lists_them`,
    /// which pins the listing itself; this one pins the count and the
    /// suggestions the message must not make.
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
            msg.contains("2 recordings with data"),
            "the message must say how many recordings carry DATA — the count is of \
             candidates, not of what the archive holds, and the two differ whenever \
             an arm produced no rows: {msg}"
        );
        // No longer asserts that the message points at `rezolus view`, and
        // deliberately so: the fix on offer is now `--recording`, which reads
        // the arm the caller asked about in the tool they are already in.
        // Sending them to another tool would be worse advice than the listing.
        assert!(
            !msg.contains("recording filter"),
            "must not suggest `filter`, which cannot split by recording: {msg}"
        );
        assert!(
            !msg.contains("Re-record"),
            "must not tell the caller to re-record what they can now select: {msg}"
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

    #[test]
    fn a_selector_picks_the_named_recording() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ab.rez");
        multi_recording_rez(&path, &["redis", "valkey"], &[true, true]);

        let sel = RecordingSelector::parse(["source=valkey".to_string()]).unwrap();
        let src = open_source_selected(&path, &sel).expect("the selector names one recording");
        // Asserted by VALUE, not merely "some series came back": both arms
        // hold `cpu_cycles`, so an assertion that data exists would pass even
        // if the wrong arm were opened.
        assert_eq!(src.counter_names(), vec!["cpu_cycles".to_string()]);
        assert_eq!(
            src.metadata_get("source").as_deref(),
            Some("valkey"),
            "the reader must be the valkey arm, not the first one"
        );
    }

    /// With no selector, a multi-recording archive is still refused — but now
    /// the message lists the recordings and the flag that picks each one,
    /// rather than telling the caller to re-record.
    #[test]
    fn no_selector_on_a_multi_recording_archive_lists_them() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ab.rez");
        multi_recording_rez(&path, &["redis", "valkey"], &[true, true]);

        let msg = match open_source(&path) {
            Ok(_) => panic!("must not silently pick an arm"),
            Err(e) => e.to_string(),
        };
        assert!(msg.contains("source=redis"), "{msg}");
        assert!(msg.contains("source=valkey"), "{msg}");
        assert!(msg.contains("--recording"), "{msg}");
    }

    #[test]
    fn a_selector_matching_nothing_lists_the_candidates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ab.rez");
        multi_recording_rez(&path, &["redis", "valkey"], &[true, true]);

        let sel = RecordingSelector::parse(["source=nope".to_string()]).unwrap();
        let msg = match open_source_selected(&path, &sel) {
            Ok(_) => panic!("must not fall back to any recording"),
            Err(e) => e.to_string(),
        };
        assert!(
            msg.contains("source=nope"),
            "the error names the selector: {msg}"
        );
        assert!(msg.contains("source=redis"), "and lists candidates: {msg}");
    }

    #[test]
    fn an_ambiguous_selector_lists_the_ones_it_matched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ab.rez");
        multi_recording_rez(&path, &["redis", "valkey"], &[true, true]);

        // Both arms carry host=web-01, so selecting on it matches two. This
        // must NOT fall through to the first.
        let sel = RecordingSelector::parse(["host=web-01".to_string()]).unwrap();
        let msg = match open_source_selected(&path, &sel) {
            Ok(_) => panic!("an ambiguous selector must not pick an arm"),
            Err(e) => e.to_string(),
        };
        assert!(msg.contains("matches 2 recordings"), "{msg}");
        assert!(msg.contains("source=redis"), "{msg}");
        assert!(msg.contains("source=valkey"), "{msg}");
    }

    #[test]
    fn a_selector_against_a_parquet_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let parquet_path = dir.path().join("rec.parquet");
        let tables = build_recorder().finalize_tables();
        let bytes = crate::recorder::rez::write_table_parquet(&tables[0]).unwrap();
        std::fs::write(&parquet_path, bytes).unwrap();

        let sel = RecordingSelector::parse(["source=redis".to_string()]).unwrap();
        let msg = match open_source_selected(&parquet_path, &sel) {
            Ok(_) => panic!("a parquet file has no recordings to select"),
            Err(e) => e.to_string(),
        };
        assert!(msg.contains("no recordings"), "{msg}");
        // Says what the file is NOT, rather than asserting it is parquet:
        // this branch also takes a text file or a truncated download, so
        // naming the format would be a guess. The check also distinguishes
        // this from the `.rez` path's own "holds no recordings", which the
        // test would otherwise accept if a parquet file were somehow routed
        // into the archive branch and rejected there for another reason.
        assert!(
            msg.contains("not a .rez archive"),
            "must say why there is nothing to select: {msg}"
        );
        // Phrased for both front ends: the stdio server passes a JSON object,
        // so advice spelled as a CLI flag would be wrong there.
        assert!(
            !msg.contains("--recording"),
            "must not name a CLI flag the server caller never typed: {msg}"
        );
    }

    #[test]
    fn a_single_recording_archive_is_unaffected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("one.rez");
        build_recorder().finalize(&path).unwrap();
        assert!(open_source(&path).is_ok(), "no selector needed");
    }

    /// An empty arm must not shadow the arm that has data, even when the two
    /// carry IDENTICAL labels.
    ///
    /// The recorder only warns about duplicate label sets, so this shape is
    /// reachable. Selecting among *candidates* (recordings with tables) and
    /// then locating the chosen one by matching its labels against the FULL
    /// recording list would find the empty duplicate first and hand back a
    /// reader with nothing in it — an analysis run reporting "no data" over
    /// an archive that holds data. Carrying the recording index through the
    /// selection instead is what prevents it.
    /// Every arm empty and a selector given: the caller gets the arm it
    /// named, not the first one.
    ///
    /// The reader carries its recording's own metadata into
    /// `describe-recording` and `extract-features`, so first-matching here
    /// would not merely return "an empty recording" — it would attribute the
    /// empty result to the WRONG endpoint, which is the same wrong-arm
    /// failure the selector exists to prevent, just with no rows to hide it.
    #[test]
    fn an_all_empty_archive_still_honors_the_selector() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("both-empty.rez");
        multi_recording_rez(&path, &["redis", "valkey"], &[false, false]);

        let sel = RecordingSelector::parse(["source=valkey".to_string()]).unwrap();
        let src = open_source_selected(&path, &sel).expect("the named arm still opens");
        assert!(src.counter_names().is_empty(), "it really is empty");
        assert_eq!(
            src.metadata_get("source").as_deref(),
            Some("valkey"),
            "and it must be the arm that was named, not the first one"
        );
    }

    /// ...but with NO selector, an all-empty archive still opens rather than
    /// erroring: reporting one empty recording beats an error about choosing
    /// between recordings that all hold nothing.
    #[test]
    fn an_all_empty_archive_opens_without_a_selector() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("both-empty.rez");
        multi_recording_rez(&path, &["redis", "valkey"], &[false, false]);

        let src = open_source(&path).expect("an all-empty archive must still open");
        assert!(src.counter_names().is_empty());
    }

    #[test]
    fn a_selector_matching_nothing_in_an_all_empty_archive_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("both-empty.rez");
        multi_recording_rez(&path, &["redis", "valkey"], &[false, false]);

        let sel = RecordingSelector::parse(["source=nope".to_string()]).unwrap();
        let msg = match open_source_selected(&path, &sel) {
            Ok(_) => panic!("must not fall back to an arm the selector did not name"),
            Err(e) => e.to_string(),
        };
        assert!(
            msg.contains("source=redis"),
            "it lists what is there: {msg}"
        );
        assert!(
            !msg.contains("with data"),
            "and must not claim these recordings hold data: {msg}"
        );
    }

    /// Naming a recording that exists but produced no rows must say so, not
    /// "no recording matches".
    ///
    /// The distinction is the whole finding: "that endpoint was never
    /// captured" and "that endpoint was captured and reported nothing" lead
    /// an agent to opposite conclusions, and only the second is true here.
    #[test]
    fn naming_an_empty_recording_says_it_produced_no_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("one-empty.rez");
        multi_recording_rez(&path, &["redis", "valkey"], &[true, false]);

        let sel = RecordingSelector::parse(["source=valkey".to_string()]).unwrap();
        let msg = match open_source_selected(&path, &sel) {
            Ok(_) => panic!("an empty arm holds nothing to analyze"),
            Err(e) => e.to_string(),
        };
        assert!(
            msg.contains("produced no rows"),
            "must not report a recorded-but-silent endpoint as absent: {msg}"
        );
        assert!(
            msg.contains("source=valkey"),
            "and must name the recording it found: {msg}"
        );
        assert!(
            msg.contains("source=redis"),
            "then list what can be analyzed: {msg}"
        );
    }

    /// A selector echoed back in an error must be pasteable.
    ///
    /// `Display` joins pairs with a comma, but `parse` splits each argument
    /// on its first `=`, so `--recording host=web-01,source=nope` comes back
    /// as the single label `host="web-01,source=nope"` and fails as a
    /// no-match for an invisible reason. An agent told to adjust and retry
    /// does exactly this paste.
    #[test]
    fn an_echoed_selector_is_in_the_repeatable_flag_form() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ab.rez");
        multi_recording_rez(&path, &["redis", "valkey"], &[true, true]);

        let sel = RecordingSelector::parse(["host=web-01".to_string(), "source=nope".to_string()])
            .unwrap();
        let msg = match open_source_selected(&path, &sel) {
            Ok(_) => panic!("no arm matches source=nope"),
            Err(e) => e.to_string(),
        };
        assert!(
            msg.contains("--recording host=web-01 --recording source=nope"),
            "the echo must be the form that round-trips through parse: {msg}"
        );
        assert!(
            !msg.contains("host=web-01,source=nope"),
            "the comma-joined Display form is not pasteable: {msg}"
        );
    }

    /// An ambiguity listing renders the recordings the selector MATCHED, not
    /// every candidate.
    ///
    /// Needs three arms where a selector matches exactly two: with only two
    /// recordings, every ambiguous selector matches both and a listing that
    /// ignored `hits` would look identical.
    #[test]
    fn an_ambiguity_listing_renders_only_the_matched_recordings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("three.rez");
        multi_recording_rez_with_arms(
            &path,
            &["redis", "valkey", "memcached"],
            &[Some("a"), Some("a"), Some("b")],
            &[true, true, true],
        );

        let sel = RecordingSelector::parse(["arm=a".to_string()]).unwrap();
        let msg = match open_source_selected(&path, &sel) {
            Ok(_) => panic!("arm=a matches two recordings"),
            Err(e) => e.to_string(),
        };
        assert!(msg.contains("matches 2 recordings"), "{msg}");
        assert!(
            msg.contains("source=redis") && msg.contains("source=valkey"),
            "{msg}"
        );
        assert!(
            !msg.contains("memcached"),
            "arm=b was never matched, so listing it would send the caller to a \
             recording their selector excluded: {msg}"
        );
    }

    #[test]
    fn an_empty_arm_with_the_same_labels_does_not_shadow_the_one_with_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dup.rez");
        multi_recording_rez(&path, &["redis", "redis"], &[false, true]);

        let src = open_source(&path).expect("only one arm has data, so no selector is needed");
        assert_eq!(
            src.counter_names(),
            vec!["cpu_cycles".to_string()],
            "the arm with rows must be the one opened"
        );
    }
}
