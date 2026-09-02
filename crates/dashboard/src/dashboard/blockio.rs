use crate::MetricsSource;
use crate::plot::*;

pub fn generate(data: &dyn MetricsSource, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);

    let mut operations = Group::new("Operations", "operations");

    let totals = operations.subgroup("Totals");
    totals.describe("Throughput and operation rate aggregated across all block devices.");
    totals.plot_promql(
        PlotOpts::counter(
            "Total Throughput",
            "blockio-throughput-total",
            Unit::Datarate,
        ),
        "sum(irate(blockio_bytes[5m]))".to_string(),
    );
    totals.plot_promql(
        PlotOpts::counter("Total IOPS", "blockio-iops-total", Unit::Count),
        "sum(irate(blockio_operations[5m]))".to_string(),
    );

    for op in &["Read", "Write"] {
        let op_lower = op.to_lowercase();
        let sg = operations.subgroup(*op);
        sg.plot_promql(
            PlotOpts::counter(
                format!("{op} Throughput"),
                format!("throughput-{op_lower}"),
                Unit::Datarate,
            ),
            format!("sum(irate(blockio_bytes{{op=\"{op_lower}\"}}[5m]))"),
        );
        sg.plot_promql(
            PlotOpts::counter(
                format!("{op} IOPS"),
                format!("iops-{op_lower}"),
                Unit::Count,
            ),
            format!("sum(irate(blockio_operations{{op=\"{op_lower}\"}}[5m]))"),
        );
    }

    view.group(operations);

    let mut latency = Group::new("Latency", "latency");

    // Pre-rename recordings call the device phase `blockio_latency`; the alias
    // lives in one place so the web dashboard and the TUI cannot disagree.
    let device = blockio_device_latency_metric(data);

    let by_op = latency.subgroup("Device");
    by_op.describe(
        "How long the device took, from the moment it began servicing a request until the request \
         completed. Percentiles broken down by read vs write.",
    );
    for op in &["Read", "Write"] {
        let op_lower = op.to_lowercase();
        by_op.histogram_rate_mean(
            op,
            &format!("latency-{op_lower}"),
            &format!("{device}{{op=\"{op_lower}\"}}"),
            RateSource::Counter(format!(
                "sum(irate(blockio_operations{{op=\"{op_lower}\"}}[5m]))"
            )),
            Unit::Time,
        );
        by_op.plot_promql(
            PlotOpts::histogram_latency(*op, format!("latency-{op_lower}")),
            format!("{device}{{op=\"{op_lower}\"}}"),
        );
    }

    // Queue and end-to-end exist only on recordings from an agent that splits
    // the phases. Gate on the metric being in the recording at all: this is an
    // AGENT-VERSION gate and only that. `label_values` is a schema query, and
    // `Histogram::refresh` calls `update_from` unconditionally
    // (src/agent/bpf/histogram.rs:91), so a collecting agent publishes the
    // series every tick whether or not any bucket is non-zero — it cannot tell
    // us whether requests actually queued. Where nothing queues, these read a
    // flat zero, which is the honest answer, not a gap; the rate beside them is
    // derived from the same histogram so it reads zero too rather than showing
    // a non-zero completion rate that would imply missing data.
    if metric_unique_label_count(data, "blockio_queue_latency", "op") > 0 {
        let queue = latency.subgroup("Queue Wait");
        queue.describe(
            "How long requests waited before the device began servicing them. This is the \
             component that grows under saturation — a device at its limit shows queue wait \
             climbing while the device phase above stays flat.",
        );
        for op in &["Read", "Write"] {
            let op_lower = op.to_lowercase();
            queue.histogram_rate_mean(
                op,
                &format!("queue-latency-{op_lower}"),
                &format!("blockio_queue_latency{{op=\"{op_lower}\"}}"),
                // Not `blockio_operations`: that counts COMPLETED requests, not
                // queued ones, and reusing it here would render a plot
                // byte-identical to the one in "Device" above — while reading
                // non-zero on a host where nothing ever queues.
                RateSource::FromHistogram,
                Unit::Time,
            );
            queue.plot_promql(
                PlotOpts::histogram_latency(*op, format!("queue-latency-{op_lower}")),
                format!("blockio_queue_latency{{op=\"{op_lower}\"}}"),
            );
        }
    }

    if metric_unique_label_count(data, "blockio_total_latency", "op") > 0 {
        let total = latency.subgroup("End-to-End");
        total.describe(
            "Queue wait and device service together, from the request entering the queue until it \
             completed — what the workload issuing the IO actually experiences. Measured \
             directly rather than summed, because two histograms cannot be added.",
        );
        for op in &["Read", "Write"] {
            let op_lower = op.to_lowercase();
            total.histogram_rate_mean(
                op,
                &format!("total-latency-{op_lower}"),
                &format!("blockio_total_latency{{op=\"{op_lower}\"}}"),
                RateSource::Counter(format!(
                    "sum(irate(blockio_operations{{op=\"{op_lower}\"}}[5m]))"
                )),
                Unit::Time,
            );
            total.plot_promql(
                PlotOpts::histogram_latency(*op, format!("total-latency-{op_lower}")),
                format!("blockio_total_latency{{op=\"{op_lower}\"}}"),
            );
        }
    }

    view.group(latency);

    let mut size = Group::new("Size", "size");

    let by_op = size.subgroup("By Operation");
    by_op.describe("IO size distribution percentiles broken down by read vs write.");
    for op in &["Read", "Write"] {
        let op_lower = op.to_lowercase();
        by_op.histogram_rate_mean(
            op,
            &format!("size-{op_lower}"),
            &format!("blockio_size{{op=\"{op_lower}\"}}"),
            RateSource::Counter(format!(
                "sum(irate(blockio_operations{{op=\"{op_lower}\"}}[5m]))"
            )),
            Unit::Bytes,
        );
        by_op.plot_promql(
            PlotOpts::histogram(*op, format!("size-{op_lower}"), Unit::Bytes, "percentiles")
                .with_log_scale(true),
            format!("blockio_size{{op=\"{op_lower}\"}}"),
        );
    }

    view.group(size);

    view
}

#[cfg(test)]
mod tests {
    use super::*;
    use metriken_query::MemoryStore;
    use std::collections::HashMap;
    use std::time::{Duration, SystemTime};

    /// Serialize a `View` and unescape the inner `"` of PromQL label selectors,
    /// so substring checks read like the queries we actually emit.
    fn view_json(view: &View) -> String {
        serde_json::to_string(view).unwrap().replace("\\\"", "\"")
    }

    /// A `MemoryStore` where each metric in `names` exists with `op="read"` and
    /// `op="write"` series across three timestamps one second apart, with
    /// monotonically increasing values. `blockio_operations` is ingested as a
    /// counter (matching production); everything else as a histogram. Several
    /// increasing snapshots -- rather than one -- are needed so `irate()` and
    /// `histogram_mean()` actually evaluate against the fixture.
    fn fixture_with_metrics(names: &[&str]) -> MemoryStore {
        let store = MemoryStore::builder().sampling_interval_ms(1000).build();
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);

        let mut counter_totals: HashMap<(String, &str), u64> = HashMap::new();
        let mut histograms: HashMap<(String, &str), histogram::Histogram> = HashMap::new();

        for step in 0..3u64 {
            let ts = t0 + Duration::from_secs(step);
            let mut counters = Vec::new();
            let mut hist_snapshots = Vec::new();
            for &name in names {
                for op in ["read", "write"] {
                    let mut metadata = HashMap::new();
                    metadata.insert("op".to_string(), op.to_string());
                    if name == "blockio_operations" {
                        let total = counter_totals.entry((name.to_string(), op)).or_insert(0);
                        *total += 100;
                        counters.push(metriken_exposition::Counter::new(
                            name.to_string(),
                            *total,
                            metadata,
                        ));
                    } else {
                        let h = histograms
                            .entry((name.to_string(), op))
                            .or_insert_with(|| histogram::Histogram::new(3, 64).unwrap());
                        h.increment(1_000_000 + step * 50_000).unwrap();
                        hist_snapshots.push(metriken_exposition::Histogram::new(
                            name.to_string(),
                            h.clone(),
                            metadata,
                        ));
                    }
                }
            }
            let snapshot = metriken_exposition::Snapshot::V2(metriken_exposition::SnapshotV2 {
                systemtime: ts,
                duration: Duration::from_secs(0),
                metadata: HashMap::new(),
                counters,
                gauges: vec![],
                histograms: hist_snapshots,
            });
            store.ingest_snapshot(snapshot);
        }
        store
    }

    #[test]
    fn blockio_latency_and_size_get_rate_mean_pairs() {
        let data = fixture_with_metrics(&[
            "blockio_device_latency",
            "blockio_size",
            "blockio_operations",
        ]);
        let json = view_json(&generate(&data, vec![]));
        assert!(json.contains("sum(irate(blockio_operations{op=\"read\"}[5m]))"));
        assert!(json.contains("histogram_mean(blockio_device_latency{op=\"read\"})"));
        assert!(json.contains("histogram_mean(blockio_device_latency{op=\"write\"})"));
        assert!(json.contains("histogram_mean(blockio_size{op=\"read\"})"));
        assert!(json.contains("blockio_size{op=\"write\"}"));
    }

    /// A recording made before the rename must still render its device-latency
    /// charts, under the old metric name.
    #[test]
    fn pre_rename_recording_still_renders_device_latency() {
        let data = fixture_with_metrics(&["blockio_latency", "blockio_size", "blockio_operations"]);
        let json = view_json(&generate(&data, vec![]));
        assert!(json.contains("blockio_latency{op=\"read\"}"));
        assert!(!json.contains("blockio_device_latency"));
        // ...and gets no queue / end-to-end sections, which it has no data for.
        assert!(!json.contains("Queue Wait"));
        assert!(!json.contains("End-to-End"));
    }

    /// A current recording prefers the new name even if both are somehow
    /// present, and gets all three phases.
    #[test]
    fn post_rename_recording_renders_all_three_phases() {
        let data = fixture_with_metrics(&[
            "blockio_device_latency",
            "blockio_queue_latency",
            "blockio_total_latency",
            "blockio_operations",
        ]);
        let json = view_json(&generate(&data, vec![]));
        assert!(json.contains("Queue Wait"));
        assert!(json.contains("End-to-End"));
        assert!(json.contains("blockio_device_latency{op=\"read\"}"));
        assert!(json.contains("blockio_queue_latency{op=\"read\"}"));
        assert!(json.contains("blockio_total_latency{op=\"read\"}"));
        // The queue rate comes from the queue histogram, not the completed-ops
        // counter — otherwise it duplicates the Device plot and reads non-zero
        // where nothing queued.
        assert!(json.contains("sum(histogram_irate(blockio_queue_latency{op=\"read\"}))"));
    }

    /// The negative direction alone cannot tell "correctly gated out" from "the
    /// gate names a metric that never exists" — a typo would render the
    /// subgroups NEVER and still pass. Paired with the positive test above.
    #[test]
    fn queue_and_total_absent_when_the_metrics_are() {
        let data = fixture_with_metrics(&["blockio_device_latency", "blockio_operations"]);
        let json = view_json(&generate(&data, vec![]));
        assert!(!json.contains("blockio_queue_latency"));
        assert!(!json.contains("blockio_total_latency"));
        assert!(!json.contains("Queue Wait"));
        assert!(!json.contains("End-to-End"));
        assert!(json.contains("blockio_device_latency{op=\"read\"}"));
    }
}
