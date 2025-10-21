use crate::viewer::tsdb::Tsdb;
use std::collections::HashMap;
use std::sync::Arc;

/// Get a hashmap of metric names to their descriptions by querying metriken metrics
fn get_metric_descriptions() -> HashMap<String, String> {
    let mut descriptions = HashMap::new();

    // Iterate through all registered metrics and extract their descriptions
    for metric in metriken::metrics().iter() {
        if let Some(description) = metric.description() {
            descriptions.insert(metric.name().to_string(), description.to_string());
        }
    }

    descriptions
}

/// Format metrics description for display
pub fn format_metrics_description(tsdb: &Arc<Tsdb>) -> String {
    let mut output = String::new();
    output.push_str("Available Metrics in Recording\n");
    output.push_str("===============================\n\n");

    // Get the descriptions from metriken metrics
    let descriptions = get_metric_descriptions();

    // List counters
    let mut counter_names = tsdb.counter_names();
    if !counter_names.is_empty() {
        counter_names.sort();
        output.push_str("COUNTERS (monotonically increasing values):\n");
        output.push_str("-------------------------------------------\n");
        for name in counter_names {
            output.push_str(&format!("• {name}\n"));
            // Add description if available
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

    // List gauges
    let mut gauge_names = tsdb.gauge_names();
    if !gauge_names.is_empty() {
        gauge_names.sort();
        output.push_str("\nGAUGES (values that can go up or down):\n");
        output.push_str("----------------------------------------\n");
        for name in gauge_names {
            output.push_str(&format!("• {name}\n"));
            // Add description if available
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

    // List histograms
    let mut histogram_names = tsdb.histogram_names();
    if !histogram_names.is_empty() {
        histogram_names.sort();
        output.push_str("\nHISTOGRAMS (distributions of values):\n");
        output.push_str("--------------------------------------\n");
        for name in histogram_names {
            output.push_str(&format!("• {name}\n"));
            // Add description if available
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

    // Add summary statistics
    let total_counters = tsdb.counter_names().len();
    let total_gauges = tsdb.gauge_names().len();
    let total_histograms = tsdb.histogram_names().len();
    let total_metrics = total_counters + total_gauges + total_histograms;

    // Calculate total series counts
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

    // Add common query examples section
    output.push_str("\n\nCOMMON QUERY PATTERNS:\n");
    output.push_str("----------------------\n");
    output.push_str("Use the 'rezolus mcp query <file> <query>' command to execute these queries.\n\n");

    output.push_str("Counter queries (use rate() for per-second rates):\n");
    output.push_str("  rate(cpu_cycles[1m])              - CPU cycles per second for each core\n");
    output.push_str("  sum(rate(cpu_cycles[1m]))         - Total CPU cycles/sec across all cores\n");
    output.push_str("  rate(cpu_instructions[1m])        - Instructions retired per second\n");
    output.push_str("  sum(rate(blockio_bytes[1m]))      - Total block I/O bytes per second\n");
    output.push_str("  rate(syscall{op=\"read\"}[1m])     - Read syscalls per second (filtered by label)\n");
    output.push_str("  sum by (op) (rate(blockio_operations[1m])) - Block I/O ops/sec grouped by operation\n\n");

    output.push_str("Gauge queries (instant values):\n");
    output.push_str("  cpu_usage                          - Current CPU usage per core\n");
    output.push_str("  sum(cpu_usage)                     - Total CPU usage across all cores\n");
    output.push_str("  memory_size                        - Memory size metrics\n\n");

    output.push_str("Histogram queries (use histogram_quantile for percentiles):\n");
    output.push_str("  histogram_quantile(0.99, scheduler_runqueue_latency) - p99 runqueue latency\n");
    output.push_str("  histogram_quantile(0.50, tcp_receive_size)           - Median TCP receive size\n\n");

    output.push_str("Aggregation and filtering:\n");
    output.push_str("  sum(gauge_metric)                  - Sum across all series\n");
    output.push_str("  avg(gauge_metric)                  - Average across all series\n");
    output.push_str("  max(gauge_metric)                  - Maximum value across all series\n");
    output.push_str("  metric{label=\"value\"}             - Filter by label value\n");
    output.push_str("  sum by (label) (metric)            - Aggregate by label\n\n");

    output.push_str("Note: Counter metrics track cumulative values, so use rate() to get per-second rates.\n");
    output.push_str("      Gauges can be queried directly as they represent point-in-time values.\n");
    output.push_str("      Histograms require histogram_quantile() to extract percentiles.\n");

    output
}
