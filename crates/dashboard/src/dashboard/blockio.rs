use crate::data::DashboardData;
use crate::plot::*;
use crate::sql;

pub fn generate(data: &dyn DashboardData, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);

    let mut operations = Group::new("Operations", "operations");

    let totals = operations.subgroup("Totals");
    totals.describe("Throughput and operation rate aggregated across all block devices.");
    totals.plot_promql_with_sql(
        PlotOpts::counter(
            "Total Throughput",
            "blockio-throughput-total",
            Unit::Datarate,
        ),
        "sum(irate(blockio_bytes[5m]))".to_string(),
        sql::irate_total("^blockio_bytes(/[^:]+)?$"),
    );
    totals.plot_promql_with_sql(
        PlotOpts::counter("Total IOPS", "blockio-iops-total", Unit::Count),
        "sum(irate(blockio_operations[5m]))".to_string(),
        sql::irate_total("^blockio_operations(/[^:]+)?$"),
    );

    for op in &["Read", "Write"] {
        let op_lower = op.to_lowercase();
        let sg = operations.subgroup(*op);
        sg.plot_promql_with_sql(
            PlotOpts::counter(
                format!("{op} Throughput"),
                format!("throughput-{op_lower}"),
                Unit::Datarate,
            ),
            format!("sum(irate(blockio_bytes{{op=\"{op_lower}\"}}[5m]))"),
            sql::irate_total(&format!("^blockio_bytes/{op_lower}(/[^:]+)?$")),
        );
        sg.plot_promql_with_sql(
            PlotOpts::counter(
                format!("{op} IOPS"),
                format!("iops-{op_lower}"),
                Unit::Count,
            ),
            format!("sum(irate(blockio_operations{{op=\"{op_lower}\"}}[5m]))"),
            sql::irate_total(&format!("^blockio_operations/{op_lower}(/[^:]+)?$")),
        );
    }

    view.group(operations);

    let mut latency = Group::new("Latency", "latency");

    let by_op = latency.subgroup("By Operation");
    by_op.describe("Latency percentiles broken down by read vs write.");
    for op in &["Read", "Write"] {
        let op_lower = op.to_lowercase();
        by_op.plot_promql_with_sql(
            PlotOpts::histogram_latency(*op, format!("latency-{op_lower}")),
            format!("blockio_latency{{op=\"{op_lower}\"}}"),
            sql::hist_percentile_series(&format!("blockio_latency/{op_lower}")),
        );
    }

    view.group(latency);

    let mut size = Group::new("Size", "size");

    let by_op = size.subgroup("By Operation");
    by_op.describe("IO size distribution percentiles broken down by read vs write.");
    for op in &["Read", "Write"] {
        let op_lower = op.to_lowercase();
        by_op.plot_promql_with_sql(
            PlotOpts::histogram(*op, format!("size-{op_lower}"), Unit::Bytes, "percentiles")
                .with_log_scale(true),
            format!("blockio_size{{op=\"{op_lower}\"}}"),
            sql::hist_percentile_series(&format!("blockio_size/{op_lower}")),
        );
    }

    view.group(size);

    view
}
