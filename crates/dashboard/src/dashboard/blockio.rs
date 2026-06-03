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

    let by_op = latency.subgroup("By Operation");
    by_op.describe("Latency percentiles broken down by read vs write.");
    for op in &["Read", "Write"] {
        let op_lower = op.to_lowercase();
        by_op.histogram_rate_mean(
            op,
            &format!("latency-{op_lower}"),
            &format!("blockio_latency{{op=\"{op_lower}\"}}"),
            RateSource::Counter(format!(
                "sum(irate(blockio_operations{{op=\"{op_lower}\"}}[5m]))"
            )),
            Unit::Time,
        );
        by_op.plot_promql(
            PlotOpts::histogram_latency(*op, format!("latency-{op_lower}")),
            format!("blockio_latency{{op=\"{op_lower}\"}}"),
        );
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

    #[test]
    fn blockio_latency_and_size_get_rate_mean_pairs() {
        let view = generate(&MemoryStore::builder().build(), vec![]);
        // serde escapes the inner `"` of PromQL label selectors; unescape
        // so the substring checks read like the queries we actually emit.
        let json = serde_json::to_string(&view).unwrap().replace("\\\"", "\"");
        // Latency per-op rate uses the accurate operations counter; mean from histogram.
        assert!(json.contains("sum(irate(blockio_operations{op=\"read\"}[5m]))"));
        assert!(json.contains("histogram_mean(blockio_latency{op=\"read\"})"));
        assert!(json.contains("histogram_mean(blockio_latency{op=\"write\"})"));
        // Size: rate also from operations count; mean is mean IO size.
        assert!(json.contains("histogram_mean(blockio_size{op=\"read\"})"));
        assert!(json.contains("histogram_mean(blockio_size{op=\"write\"})"));
        // Percentile histograms still present.
        assert!(json.contains("blockio_latency{op=\"read\"}"));
        assert!(json.contains("blockio_size{op=\"write\"}"));
    }
}
