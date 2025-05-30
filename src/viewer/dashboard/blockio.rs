use super::*;

pub fn generate(data: &Tsdb, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);

    /*
     * Operations
     */

    let mut operations = Group::new("Operations", "operations");

    operations.plot(
        PlotOpts::line(
            "Total Throughput",
            "blockio-throughput-total",
            Unit::Datarate,
        ),
        data.counters("blockio_bytes", ()).map(|v| v.rate().sum()),
    );

    operations.plot(
        PlotOpts::line("Total IOPS", "blockio-iops-total", Unit::Count),
        data.counters("blockio_operations", ())
            .map(|v| v.rate().sum()),
    );

    for op in &["Read", "Write"] {
        operations.plot(
            PlotOpts::line(
                &format!("{op} Throughput"),
                format!("throughput-{}", op.to_lowercase()),
                Unit::Datarate,
            ),
            data.counters("blockio_bytes", [("op", op.to_lowercase())])
                .map(|v| v.rate().sum()),
        );

        operations.plot(
            PlotOpts::line(&format!("{op} IOPS"), format!("iops-{}", op.to_lowercase()), Unit::Count),
            data.counters("blockio_operations", [("op", op.to_lowercase())])
                .map(|v| v.rate().sum()),
        );
    }

    view.group(operations);

    /*
     * Latency
     */

    let mut latency = Group::new("Latency", "latency");

    for op in &["Read", "Write"] {
        latency.scatter(
            PlotOpts::scatter(*op, format!("latency-{}", op.to_lowercase()), Unit::Time),
            data.percentiles("blockio_latency", [("op", op.to_lowercase())], PERCENTILES),
        );
    }

    view.group(latency);

    /*
     * IO Size
     */

    let mut size = Group::new("Size", "size");

    for op in &["Read", "Write"] {
        size.scatter(
            PlotOpts::scatter(*op, format!("size-{}", op.to_lowercase()), Unit::Bytes),
            data.percentiles("blockio_size", [("op", op.to_lowercase())], PERCENTILES),
        );
    }

    view.group(size);

    view
}
