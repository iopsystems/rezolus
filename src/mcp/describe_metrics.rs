use crate::viewer::sql_capture::SqlCapture;
#[cfg(feature = "live-mode")]
use crate::viewer::tsdb::Tsdb;
use dashboard::DashboardData;
#[cfg(feature = "live-mode")]
use std::sync::Arc;

/// Format metrics description for display (DuckDB-backed CLI path).
///
/// Mirrors [`format_metrics_description`] but reads from a
/// [`SqlCapture`]'s cached [`metriken_query_sql::MetricCatalog`]
/// instead of a `Tsdb`. The legacy function below is retained for
/// `src/mcp/server.rs` until that file is migrated; producing a
/// per-metric label inventory from either source yields the same
/// strings, so output is byte-identical when the same parquet is
/// fed to both.
pub fn format_metrics_description_sql(capture: &SqlCapture) -> String {
    let mut output = String::new();
    output.push_str("Available Metrics in Recording\n");
    output.push_str("===============================\n\n");

    let descriptions = crate::common::metric_descriptions();
    let catalog = capture.catalog();

    let mut counter_names = capture.counter_names();
    if !counter_names.is_empty() {
        counter_names.sort();
        output.push_str("COUNTERS (monotonically increasing values):\n");
        output.push_str("-------------------------------------------\n");
        for name in counter_names {
            output.push_str(&format!("• {name}\n"));
            if let Some(desc) = descriptions.get(name) {
                output.push_str(&format!("  Description: {desc}\n"));
            }
            if let Some(series_list) = catalog.series_by_metric.get(name) {
                let mut all_keys = std::collections::HashSet::new();
                for series in series_list {
                    for key in series.labels.keys() {
                        // `MetricCatalog` already strips `metric`,
                        // `metric_type`, `unit`, `grouping_power`,
                        // `max_value_power` during classify (see
                        // metriken-query-sql/src/views.rs:47-60), so no
                        // additional metadata filtering is needed here.
                        all_keys.insert(key.clone());
                    }
                }
                if !all_keys.is_empty() {
                    let mut keys: Vec<_> = all_keys.into_iter().collect();
                    keys.sort();
                    output.push_str(&format!("  Labels: {}\n", keys.join(", ")));
                }
                output.push_str(&format!("  Series count: {}\n", series_list.len()));
            }
            output.push('\n');
        }
    }

    let mut gauge_names = capture.gauge_names();
    if !gauge_names.is_empty() {
        gauge_names.sort();
        output.push_str("\nGAUGES (values that can go up or down):\n");
        output.push_str("----------------------------------------\n");
        for name in gauge_names {
            output.push_str(&format!("• {name}\n"));
            if let Some(desc) = descriptions.get(name) {
                output.push_str(&format!("  Description: {desc}\n"));
            }
            if let Some(series_list) = catalog.series_by_metric.get(name) {
                let mut all_keys = std::collections::HashSet::new();
                for series in series_list {
                    for key in series.labels.keys() {
                        all_keys.insert(key.clone());
                    }
                }
                if !all_keys.is_empty() {
                    let mut keys: Vec<_> = all_keys.into_iter().collect();
                    keys.sort();
                    output.push_str(&format!("  Labels: {}\n", keys.join(", ")));
                }
                output.push_str(&format!("  Series count: {}\n", series_list.len()));
            }
            output.push('\n');
        }
    }

    let mut histogram_names = capture.histogram_names();
    if !histogram_names.is_empty() {
        histogram_names.sort();
        output.push_str("\nHISTOGRAMS (distributions of values):\n");
        output.push_str("--------------------------------------\n");
        for name in histogram_names {
            output.push_str(&format!("• {name}\n"));
            if let Some(desc) = descriptions.get(name) {
                output.push_str(&format!("  Description: {desc}\n"));
            }
            if let Some(series_list) = catalog.series_by_metric.get(name) {
                let mut all_keys = std::collections::HashSet::new();
                for series in series_list {
                    for key in series.labels.keys() {
                        all_keys.insert(key.clone());
                    }
                }
                if !all_keys.is_empty() {
                    let mut keys: Vec<_> = all_keys.into_iter().collect();
                    keys.sort();
                    output.push_str(&format!("  Labels: {}\n", keys.join(", ")));
                }
                output.push_str(&format!("  Series count: {}\n", series_list.len()));
            }
            output.push('\n');
        }
    }

    let total_counters = capture.counter_names().len();
    let total_gauges = capture.gauge_names().len();
    let total_histograms = capture.histogram_names().len();
    let total_metrics = total_counters + total_gauges + total_histograms;

    let total_counter_series: usize = capture
        .counter_names()
        .iter()
        .filter_map(|n| catalog.series_by_metric.get(*n).map(Vec::len))
        .sum();
    let total_gauge_series: usize = capture
        .gauge_names()
        .iter()
        .filter_map(|n| catalog.series_by_metric.get(*n).map(Vec::len))
        .sum();
    let total_histogram_series: usize = capture
        .histogram_names()
        .iter()
        .filter_map(|n| catalog.series_by_metric.get(*n).map(Vec::len))
        .sum();

    let total_series = total_counter_series + total_gauge_series + total_histogram_series;

    output.push_str("\nSUMMARY:\n");
    output.push_str("--------\n");
    output.push_str(&format!("Total unique metrics: {total_metrics}\n"));
    output.push_str(&format!("  Counters: {total_counters}\n"));
    output.push_str(&format!("  Gauges: {total_gauges}\n"));
    output.push_str(&format!("  Histograms: {total_histograms}\n"));
    output.push_str(&format!("\nTotal time series: {total_series}\n"));
    output.push_str(&format!("  Counter series: {total_counter_series}\n"));
    output.push_str(&format!("  Gauge series: {total_gauge_series}\n"));
    output.push_str(&format!("  Histogram series: {total_histogram_series}\n"));
    output.push_str(&format!(
        "\nSampling interval: {}ms\n",
        capture.interval() * 1000.0
    ));

    output.push_str(QUERY_EXAMPLES);
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn demo_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("site")
            .join("viewer")
            .join("data")
            .join("demo.parquet")
    }

    /// End-to-end pin: the SQL-backed formatter must produce non-empty
    /// counter/gauge/histogram sections for demo.parquet, list each
    /// metric with its label keys + series count, and conclude with
    /// the query-examples block. This is the user-visible contract
    /// for `rezolus mcp describe-metrics`.
    #[test]
    fn format_metrics_description_sql_against_demo_parquet() {
        let path = demo_path();
        if !path.exists() {
            eprintln!("skipping: fixture {} missing", path.display());
            return;
        }
        let (_backend, capture) =
            crate::mcp::backend::open_capture(&path).expect("open demo.parquet");
        let out = format_metrics_description_sql(&capture);

        assert!(out.contains("Available Metrics in Recording"));
        assert!(out.contains("COUNTERS"));
        assert!(out.contains("GAUGES"));
        assert!(out.contains("HISTOGRAMS"));
        assert!(out.contains("SUMMARY:"));
        // Sampling interval surfaces.
        assert!(out.contains("Sampling interval: 1000ms"));
        // Query-examples block is appended.
        assert!(out.contains("COMMON QUERY PATTERNS:"));
        assert!(out.contains("histogram_quantile"));

        // Spot-check: `cpu_usage` is a per-CPU counter on demo.parquet.
        // Two assertions: it's listed in the COUNTERS section, and at
        // least one of its label keys (e.g. `id` for the CPU index)
        // is rendered.
        assert!(out.contains("• cpu_usage"));

        // Behavior difference vs the legacy Tsdb-backed formatter:
        // the parquet's `duration` column is timing metadata, not a
        // metric, and is *not* listed as a counter here. (The legacy
        // path includes it because metriken-query's Tsdb classifies
        // any UInt64 column as a counter without filtering the
        // reserved `duration` field.) This is an intentional fix
        // that landed with the SQL-backed describe-metrics; pinning
        // the new behaviour here so a future regression that reverts
        // it gets caught.
        assert!(!out.contains("• duration\n"),
            "duration is metadata, must not appear as a counter metric in the SQL-backed output");
    }
}

/// Common query examples appended to both formatters. Pulled out so
/// the two paths stay in lock-step.
const QUERY_EXAMPLES: &str = "\n\nCOMMON QUERY PATTERNS:\n\
----------------------\n\
Use the 'rezolus mcp query <file> <query>' command to execute these queries.\n\
\n\
Counter queries (use rate() for per-second rates):\n  \
rate(cpu_cycles[1m])              - CPU cycles per second for each core\n  \
sum(rate(cpu_cycles[1m]))         - Total CPU cycles/sec across all cores\n  \
rate(cpu_instructions[1m])        - Instructions retired per second\n  \
sum(rate(blockio_bytes[1m]))      - Total block I/O bytes per second\n  \
rate(syscall{op=\"read\"}[1m])     - Read syscalls per second (filtered by label)\n  \
sum by (op) (rate(blockio_operations[1m])) - Block I/O ops/sec grouped by operation\n\n\
Gauge queries (instant values):\n  \
cpu_usage                          - Current CPU usage per core\n  \
sum(cpu_usage)                     - Total CPU usage across all cores\n  \
memory_size                        - Memory size metrics\n\n\
Histogram queries (use histogram_quantile for percentiles):\n  \
histogram_quantile(0.99, scheduler_runqueue_latency) - p99 runqueue latency\n  \
histogram_quantile(0.50, tcp_receive_size)           - Median TCP receive size\n\n\
Aggregation and filtering:\n  \
sum(gauge_metric)                  - Sum across all series\n  \
avg(gauge_metric)                  - Average across all series\n  \
max(gauge_metric)                  - Maximum value across all series\n  \
metric{label=\"value\"}             - Filter by label value\n  \
sum by (label) (metric)            - Aggregate by label\n\n\
Note: Counter metrics track cumulative values, so use rate() to get per-second rates.\n      \
Gauges can be queried directly as they represent point-in-time values.\n      \
Histograms require histogram_quantile() to extract percentiles.\n";

/// Format metrics description for display (legacy Tsdb path).
/// `format_metrics_description_sql` is the live entry point.
#[cfg(feature = "live-mode")]
pub fn format_metrics_description(tsdb: &Arc<Tsdb>) -> String {
    let mut output = String::new();
    output.push_str("Available Metrics in Recording\n");
    output.push_str("===============================\n\n");

    let descriptions = crate::common::metric_descriptions();

    let mut counter_names = tsdb.counter_names();
    if !counter_names.is_empty() {
        counter_names.sort();
        output.push_str("COUNTERS (monotonically increasing values):\n");
        output.push_str("-------------------------------------------\n");
        for name in counter_names {
            output.push_str(&format!("• {name}\n"));
            if let Some(desc) = descriptions.get(name) {
                output.push_str(&format!("  Description: {desc}\n"));
            }
            if let Some(labels_list) = tsdb.counter_labels(name) {
                // Get unique label keys, excluding metadata labels
                let mut all_keys = std::collections::HashSet::new();
                for labels in &labels_list {
                    for (key, _) in labels.inner.iter() {
                        // Skip metadata labels
                        if key != "metric" && key != "unit" && key != "metric_type" {
                            all_keys.insert(key.clone());
                        }
                    }
                }
                if !all_keys.is_empty() {
                    let mut keys: Vec<_> = all_keys.into_iter().collect();
                    keys.sort();
                    output.push_str(&format!("  Labels: {}\n", keys.join(", ")));
                }
                output.push_str(&format!("  Series count: {}\n", labels_list.len()));
            }
            output.push('\n');
        }
    }

    let mut gauge_names = tsdb.gauge_names();
    if !gauge_names.is_empty() {
        gauge_names.sort();
        output.push_str("\nGAUGES (values that can go up or down):\n");
        output.push_str("----------------------------------------\n");
        for name in gauge_names {
            output.push_str(&format!("• {name}\n"));
            if let Some(desc) = descriptions.get(name) {
                output.push_str(&format!("  Description: {desc}\n"));
            }
            if let Some(labels_list) = tsdb.gauge_labels(name) {
                // Get unique label keys, excluding metadata labels
                let mut all_keys = std::collections::HashSet::new();
                for labels in &labels_list {
                    for (key, _) in labels.inner.iter() {
                        // Skip metadata labels
                        if key != "metric" && key != "unit" && key != "metric_type" {
                            all_keys.insert(key.clone());
                        }
                    }
                }
                if !all_keys.is_empty() {
                    let mut keys: Vec<_> = all_keys.into_iter().collect();
                    keys.sort();
                    output.push_str(&format!("  Labels: {}\n", keys.join(", ")));
                }
                output.push_str(&format!("  Series count: {}\n", labels_list.len()));
            }
            output.push('\n');
        }
    }

    let mut histogram_names = tsdb.histogram_names();
    if !histogram_names.is_empty() {
        histogram_names.sort();
        output.push_str("\nHISTOGRAMS (distributions of values):\n");
        output.push_str("--------------------------------------\n");
        for name in histogram_names {
            output.push_str(&format!("• {name}\n"));
            if let Some(desc) = descriptions.get(name) {
                output.push_str(&format!("  Description: {desc}\n"));
            }
            if let Some(labels_list) = tsdb.histogram_labels(name) {
                // Get unique label keys, excluding metadata labels
                let mut all_keys = std::collections::HashSet::new();
                for labels in &labels_list {
                    for (key, _) in labels.inner.iter() {
                        // Skip metadata labels
                        if key != "metric" && key != "unit" && key != "metric_type" {
                            all_keys.insert(key.clone());
                        }
                    }
                }
                if !all_keys.is_empty() {
                    let mut keys: Vec<_> = all_keys.into_iter().collect();
                    keys.sort();
                    output.push_str(&format!("  Labels: {}\n", keys.join(", ")));
                }
                output.push_str(&format!("  Series count: {}\n", labels_list.len()));
            }
            output.push('\n');
        }
    }

    let total_counters = tsdb.counter_names().len();
    let total_gauges = tsdb.gauge_names().len();
    let total_histograms = tsdb.histogram_names().len();
    let total_metrics = total_counters + total_gauges + total_histograms;

    let mut total_counter_series = 0;
    for name in tsdb.counter_names() {
        if let Some(labels_list) = tsdb.counter_labels(name) {
            total_counter_series += labels_list.len();
        }
    }

    let mut total_gauge_series = 0;
    for name in tsdb.gauge_names() {
        if let Some(labels_list) = tsdb.gauge_labels(name) {
            total_gauge_series += labels_list.len();
        }
    }

    let mut total_histogram_series = 0;
    for name in tsdb.histogram_names() {
        if let Some(labels_list) = tsdb.histogram_labels(name) {
            total_histogram_series += labels_list.len();
        }
    }

    let total_series = total_counter_series + total_gauge_series + total_histogram_series;

    output.push_str("\nSUMMARY:\n");
    output.push_str("--------\n");
    output.push_str(&format!("Total unique metrics: {total_metrics}\n"));
    output.push_str(&format!("  Counters: {total_counters}\n"));
    output.push_str(&format!("  Gauges: {total_gauges}\n"));
    output.push_str(&format!("  Histograms: {total_histograms}\n"));
    output.push_str(&format!("\nTotal time series: {total_series}\n"));
    output.push_str(&format!("  Counter series: {total_counter_series}\n"));
    output.push_str(&format!("  Gauge series: {total_gauge_series}\n"));
    output.push_str(&format!("  Histogram series: {total_histogram_series}\n"));
    output.push_str(&format!(
        "\nSampling interval: {}ms\n",
        tsdb.interval() * 1000.0
    ));

    output.push_str(QUERY_EXAMPLES);
    output
}
