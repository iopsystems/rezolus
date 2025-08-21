use super::*;

pub fn generate(data: &Arc<Tsdb>, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);

    /*
     * Operations
     */

    let mut operations = Group::new("Operations", "operations");

    // Total throughput (bytes/sec)
    operations.plot_promql(
        PlotOpts::line(
            "Total Throughput",
            "blockio-throughput-total",
            Unit::Datarate,
        ),
        "sum(irate(blockio_bytes[5m]))".to_string(),
    );

    // Total IOPS
    operations.plot_promql(
        PlotOpts::line("Total IOPS", "blockio-iops-total", Unit::Count),
        "sum(irate(blockio_operations[5m]))".to_string(),
    );

    // Per-operation metrics (Read/Write)
    for op in &["Read", "Write"] {
        let op_lower = op.to_lowercase();

        // Throughput for this operation
        operations.plot_promql(
            PlotOpts::line(
                format!("{op} Throughput"),
                format!("throughput-{op_lower}"),
                Unit::Datarate,
            ),
            format!("sum(irate(blockio_bytes{{op=\"{op_lower}\"}}[5m]))"),
        );

        // IOPS for this operation
        operations.plot_promql(
            PlotOpts::line(
                format!("{op} IOPS"),
                format!("iops-{op_lower}"),
                Unit::Count,
            ),
            format!("sum(irate(blockio_operations{{op=\"{op_lower}\"}}[5m]))"),
        );
    }

    view.group(operations);

    /*
     * Latency
     */

    let mut latency = Group::new("Latency", "latency");

    // Latency percentiles for Read and Write operations
    for op in &["Read", "Write"] {
        let op_lower = op.to_lowercase();

        latency.plot_promql(
            PlotOpts::scatter(*op, format!("latency-{op_lower}"), Unit::Time)
                .with_log_scale(true),
            format!("histogram_percentiles([0.5, 0.9, 0.99, 0.999, 0.9999], blockio_latency{{op=\"{op_lower}\"}})"),
        );
    }

    view.group(latency);

    /*
     * IO Size
     */

    let mut size = Group::new("Size", "size");

    // IO size percentiles for Read and Write operations
    for op in &["Read", "Write"] {
        let op_lower = op.to_lowercase();

        size.plot_promql(
            PlotOpts::scatter(*op, format!("size-{op_lower}"), Unit::Bytes).with_log_scale(true),
            format!(
                "histogram_percentiles([0.5, 0.9, 0.99, 0.999, 0.9999], blockio_size{{op=\"{op_lower}\"}})"
            ),
        );
    }

    view.group(size);

    view
}
