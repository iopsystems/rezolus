use metriken_query::MetricsSource;
use metriken_storage::{is_internal_label, is_storage_key};

/// Format metrics description for display
pub fn format_metrics_description(data: &dyn MetricsSource) -> String {
    let mut output = String::new();
    output.push_str("Available Metrics in Recording\n");
    output.push_str("===============================\n\n");

    let descriptions = crate::common::metric_descriptions();

    let helper = |output: &mut String,
                  name: &str,
                  labels_list: &[std::collections::BTreeMap<String, String>]| {
        let mut all_keys = std::collections::HashSet::new();
        for labels in labels_list {
            for key in labels.keys() {
                if !is_storage_key(key) && !is_internal_label(key) {
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
        let _ = name;
    };

    let mut counter_names = data.counter_names();
    if !counter_names.is_empty() {
        counter_names.sort();
        output.push_str("COUNTERS (monotonically increasing values):\n");
        output.push_str("-------------------------------------------\n");
        for name in &counter_names {
            output.push_str(&format!("• {name}\n"));
            if let Some(desc) = descriptions.get(name.as_str()) {
                output.push_str(&format!("  Description: {desc}\n"));
            }
            let labels_list = data.counter_labels(name);
            helper(&mut output, name, &labels_list);
            output.push('\n');
        }
    }

    let mut gauge_names = data.gauge_names();
    if !gauge_names.is_empty() {
        gauge_names.sort();
        output.push_str("\nGAUGES (values that can go up or down):\n");
        output.push_str("----------------------------------------\n");
        for name in &gauge_names {
            output.push_str(&format!("• {name}\n"));
            if let Some(desc) = descriptions.get(name.as_str()) {
                output.push_str(&format!("  Description: {desc}\n"));
            }
            let labels_list = data.gauge_labels(name);
            helper(&mut output, name, &labels_list);
            output.push('\n');
        }
    }

    let mut histogram_names = data.histogram_names();
    if !histogram_names.is_empty() {
        histogram_names.sort();
        output.push_str("\nHISTOGRAMS (distributions of values):\n");
        output.push_str("--------------------------------------\n");
        for name in &histogram_names {
            output.push_str(&format!("• {name}\n"));
            if let Some(desc) = descriptions.get(name.as_str()) {
                output.push_str(&format!("  Description: {desc}\n"));
            }
            let labels_list = data.histogram_labels(name);
            helper(&mut output, name, &labels_list);
            output.push('\n');
        }
    }

    let mut gauge_histogram_names = data.gauge_histogram_names();
    if !gauge_histogram_names.is_empty() {
        gauge_histogram_names.sort();
        output.push_str("\nGAUGE HISTOGRAMS (distributions as they are at each sample):\n");
        output.push_str("-------------------------------------------------------------\n");
        for name in &gauge_histogram_names {
            output.push_str(&format!("• {name}\n"));
            if let Some(desc) = descriptions.get(name.as_str()) {
                output.push_str(&format!("  Description: {desc}\n"));
            }
            let labels_list = data.gauge_histogram_labels(name);
            helper(&mut output, name, &labels_list);
            output.push('\n');
        }
    }

    let total_counters = counter_names.len();
    let total_gauges = gauge_names.len();
    let total_histograms = histogram_names.len();
    let total_gauge_histograms = gauge_histogram_names.len();
    let total_metrics = total_counters + total_gauges + total_histograms + total_gauge_histograms;

    let total_counter_series: usize = counter_names
        .iter()
        .map(|n| data.counter_labels(n).len())
        .sum();
    let total_gauge_series: usize = gauge_names.iter().map(|n| data.gauge_labels(n).len()).sum();
    let total_histogram_series: usize = histogram_names
        .iter()
        .map(|n| data.histogram_labels(n).len())
        .sum();
    let total_gauge_histogram_series: usize = gauge_histogram_names
        .iter()
        .map(|n| data.gauge_histogram_labels(n).len())
        .sum();
    let total_series = total_counter_series
        + total_gauge_series
        + total_histogram_series
        + total_gauge_histogram_series;

    output.push_str("\nSUMMARY:\n");
    output.push_str("--------\n");
    output.push_str(&format!("Total unique metrics: {total_metrics}\n"));
    output.push_str(&format!("  Counters: {total_counters}\n"));
    output.push_str(&format!("  Gauges: {total_gauges}\n"));
    output.push_str(&format!("  Histograms: {total_histograms}\n"));
    if total_gauge_histograms > 0 {
        output.push_str(&format!("  Gauge histograms: {total_gauge_histograms}\n"));
    }
    output.push_str(&format!("\nTotal time series: {total_series}\n"));
    output.push_str(&format!("  Counter series: {total_counter_series}\n"));
    output.push_str(&format!("  Gauge series: {total_gauge_series}\n"));
    output.push_str(&format!("  Histogram series: {total_histogram_series}\n"));
    if total_gauge_histograms > 0 {
        output.push_str(&format!(
            "  Gauge histogram series: {total_gauge_histogram_series}\n"
        ));
    }
    output.push_str(&format!(
        "\nSampling interval: {}ms\n",
        data.interval() * 1000.0
    ));

    output.push_str("\n\nCOMMON QUERY PATTERNS:\n");
    output.push_str("----------------------\n");
    output.push_str(
        "Use the 'rezolus mcp query <file> <query>' command to execute these queries.\n\n",
    );

    output.push_str(
        "Counter queries (irate() is the per-second rate; rate(x[r]) averages over r):\n",
    );
    output.push_str("  irate(cpu_cycles[1m])             - CPU cycles per second for each core\n");
    output
        .push_str("  sum(irate(cpu_cycles[1m]))        - Total CPU cycles/sec across all cores\n");
    output.push_str("  irate(cpu_instructions[1m])       - Instructions retired per second\n");
    output.push_str("  sum(irate(blockio_bytes[1m]))     - Total block I/O bytes per second\n");
    output.push_str(
        "  irate(syscall{op=\"read\"}[1m])    - Read syscalls per second (filtered by label)\n",
    );
    output.push_str(
        "  sum by (op) (irate(blockio_operations[1m])) - Block I/O ops/sec grouped by operation\n\n",
    );

    output.push_str("Gauge queries (instant values):\n");
    output.push_str("  cpu_usage                          - Current CPU usage per core\n");
    output.push_str("  sum(cpu_usage)                     - Total CPU usage across all cores\n");
    output.push_str("  memory_size                        - Memory size metrics\n\n");

    output.push_str("Histogram queries (use histogram_quantile for percentiles):\n");
    output.push_str(
        "  histogram_quantile(0.99, scheduler_runqueue_latency) - p99 runqueue latency\n",
    );
    output.push_str(
        "  histogram_quantile(0.50, tcp_receive_size)           - Median TCP receive size\n\n",
    );

    output.push_str("Aggregation and filtering:\n");
    output.push_str("  sum(gauge_metric)                  - Sum across all series\n");
    output.push_str("  avg(gauge_metric)                  - Average across all series\n");
    output.push_str("  max(gauge_metric)                  - Maximum value across all series\n");
    output.push_str("  metric{label=\"value\"}             - Filter by label value\n");
    output.push_str("  sum by (label) (metric)            - Aggregate by label\n\n");

    output.push_str(
        "Note: Counter metrics track cumulative values, so use irate() to get per-second rates.\n",
    );
    output
        .push_str("      Gauges can be queried directly as they represent point-in-time values.\n");
    output.push_str("      Histograms require histogram_quantile() to extract percentiles.\n");

    output
}

#[cfg(test)]
mod tests {
    /// A gauge histogram has its own section and counts in the summary.
    #[test]
    fn a_gauge_histogram_is_described() {
        use ::rez::fixtures::{map, snap_gauge_histogram, write_dendro, Finish, Source, ANCHOR};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gh.dendro");
        let rows = (0..3u64)
            .map(|i| {
                let ts = ANCHOR + i * 1_000_000_000;
                (snap_gauge_histogram(ts, "depth", "queue", 100), ts)
            })
            .collect();
        let source = Source {
            labels: map(&[("source", "rezolus")]),
            metadata: map(&[]),
            rows,
        };
        write_dendro(&path, &[source], None, Finish::Finalized);
        let readers = crate::rez_reader::RezReader::open_recordings(
            &path,
            metriken_storage::BufferPool::new(8 << 20),
        )
        .unwrap();
        let text = super::format_metrics_description(&readers[0].1);
        assert!(text.contains("GAUGE HISTOGRAMS"), "{text}");
        assert!(text.contains("• depth"), "{text}");
        assert!(text.contains("Gauge histograms: 1"), "{text}");
    }
}
