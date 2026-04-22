use crate::Tsdb;
use crate::plot::*;

pub fn generate(data: &Tsdb, sections: Vec<Section>, throughput_query: Option<&str>) -> View {
    let mut view = View::new(data, sections);

    /*
     * CPU
     */

    let mut cpu = Group::new("CPU", "cpu");

    let busy = cpu.subgroup("CPU Busy");
    busy.describe("Overall CPU utilization and per-core breakdown.");
    busy.plot_promql(
        PlotOpts::counter("Busy %", "cpu-busy", Unit::Percentage).percentage_range(),
        "sum(irate(cpu_usage[5m])) / cpu_cores / 1000000000".to_string(),
    );
    busy.plot_promql(
        PlotOpts::counter("Busy %", "cpu-busy-heatmap", Unit::Percentage).percentage_range(),
        "sum by (id) (irate(cpu_usage[5m])) / 1000000000".to_string(),
    );

    view.group(cpu);

    /*
     * Network
     */

    let mut network = Group::new("Network", "network");

    let bandwidth = network.subgroup("Bandwidth");
    bandwidth.describe("Transmit and receive bit rates on the wire.");
    bandwidth.plot_promql(
        PlotOpts::counter(
            "Transmit Bandwidth",
            "network-transmit-bandwidth",
            Unit::Bitrate,
        )
        .with_unit_system("bitrate"),
        "sum(irate(network_bytes{direction=\"transmit\"}[5m])) * 8".to_string(),
    );
    bandwidth.plot_promql(
        PlotOpts::counter(
            "Receive Bandwidth",
            "network-receive-bandwidth",
            Unit::Bitrate,
        )
        .with_unit_system("bitrate"),
        "sum(irate(network_bytes{direction=\"receive\"}[5m])) * 8".to_string(),
    );

    let packets = network.subgroup("Packets");
    packets.describe("Transmit and receive packet rates.");
    packets.plot_promql(
        PlotOpts::counter("Transmit Packets", "network-transmit-packets", Unit::Rate),
        "sum(irate(network_packets{direction=\"transmit\"}[5m]))".to_string(),
    );
    packets.plot_promql(
        PlotOpts::counter("Receive Packets", "network-receive-packets", Unit::Rate),
        "sum(irate(network_packets{direction=\"receive\"}[5m]))".to_string(),
    );

    let tcp = network.subgroup("TCP Latency");
    tcp.describe("Time from packet received to being processed by the application.");
    tcp.plot_promql_full(
        PlotOpts::histogram_latency("TCP Packet Latency", "tcp-packet-latency")
            .with_axis_label("Latency")
            .with_unit_system("time"),
        "tcp_packet_latency".to_string(),
    );

    view.group(network);

    /*
     * Scheduler
     */

    let mut scheduler = Group::new("Scheduler", "scheduler");

    let queueing = scheduler.subgroup("Runqueue Latency");
    queueing.describe("How long tasks waited on the runqueue before getting CPU time.");
    queueing.plot_promql_full(
        PlotOpts::histogram_latency("Runqueue Latency", "scheduler-runqueue-latency")
            .with_axis_label("Latency")
            .with_unit_system("time"),
        "scheduler_runqueue_latency".to_string(),
    );

    view.group(scheduler);

    /*
     * Syscall
     */

    let mut syscall = Group::new("Syscall", "syscall");

    let total = syscall.subgroup("Rate & Latency");
    total.describe("Aggregate syscall rate and latency across all operation categories.");
    total.plot_promql(
        PlotOpts::counter("Total", "syscall-total", Unit::Rate),
        "sum(irate(syscall[5m]))".to_string(),
    );
    total.plot_promql(
        PlotOpts::histogram_latency("Total", "syscall-total-latency"),
        "syscall_latency".to_string(),
    );

    view.group(syscall);

    /*
     * Softirq
     */

    let mut softirq = Group::new("Softirq", "softirq");

    let rate = softirq.subgroup("Rate");
    rate.describe("Softirqs handled per second, aggregate and per-CPU.");
    rate.plot_promql(
        PlotOpts::counter("Rate", "softirq-total-rate", Unit::Rate),
        "sum(irate(softirq[5m]))".to_string(),
    );
    rate.plot_promql(
        PlotOpts::counter("Rate", "softirq-total-rate-heatmap", Unit::Rate),
        "sum by (id) (irate(softirq[5m]))".to_string(),
    );

    let time = softirq.subgroup("CPU Time");
    time.describe("Fraction of CPU time spent servicing softirqs.");
    time.plot_promql(
        PlotOpts::counter("CPU %", "softirq-total-time", Unit::Percentage).percentage_range(),
        "sum(irate(softirq_time[5m])) / cpu_cores / 1000000000".to_string(),
    );
    time.plot_promql(
        PlotOpts::counter("CPU %", "softirq-total-time-heatmap", Unit::Percentage)
            .percentage_range(),
        "sum by (id) (irate(softirq_time[5m])) / 1000000000".to_string(),
    );

    view.group(softirq);

    /*
     * BlockIO
     */

    let mut blockio = Group::new("BlockIO", "blockio");

    let read = blockio.subgroup("Read");
    read.describe("Read throughput and IOPS across all block devices.");
    read.plot_promql(
        PlotOpts::counter("Read Throughput", "blockio-throughput-read", Unit::Datarate),
        "sum(irate(blockio_bytes{op=\"read\"}[5m]))".to_string(),
    );
    read.plot_promql(
        PlotOpts::counter("Read IOPS", "blockio-iops-read", Unit::Count),
        "sum(irate(blockio_operations{op=\"read\"}[5m]))".to_string(),
    );

    let write = blockio.subgroup("Write");
    write.describe("Write throughput and IOPS across all block devices.");
    write.plot_promql(
        PlotOpts::counter(
            "Write Throughput",
            "blockio-throughput-write",
            Unit::Datarate,
        ),
        "sum(irate(blockio_bytes{op=\"write\"}[5m]))".to_string(),
    );
    write.plot_promql(
        PlotOpts::counter("Write IOPS", "blockio-iops-write", Unit::Count),
        "sum(irate(blockio_operations{op=\"write\"}[5m]))".to_string(),
    );

    view.group(blockio);

    /*
     * Normalized by Throughput (only when service extension provides throughput KPI)
     */
    if let Some(tq) = throughput_query {
        let mut normalized = Group::new("Normalized by Throughput", "normalized-throughput");

        let efficiency = normalized.subgroup("Efficiency Metrics");
        efficiency.describe(
            "Resource consumption normalized by service throughput — lower is more efficient.",
        );
        // CPU time per throughput unit. cpu_usage is a ns counter; irate
        // gives ns/s of CPU consumed, divided by throughput yields
        // ns-of-CPU per request. The viewer's time formatter picks the
        // best scale (ns/µs/ms/s) based on magnitude — for typical RPC
        // workloads (100s of µs per request) this displays as µs, which
        // is more legible than fractional seconds. Scales past one full
        // core on multi-core workloads, unlike the old utilization-
        // fraction formulation.
        efficiency.plot_promql(
            PlotOpts::counter(
                "CPU Time / Throughput",
                "normalized-cpu-busy",
                Unit::Time,
            ),
            format!("sum(irate(cpu_usage[5m])) / ({tq})"),
        );
        efficiency.plot_promql(
            PlotOpts::counter(
                "Network TX / Throughput",
                "normalized-network-tx",
                Unit::Count,
            ),
            format!("(sum(irate(network_bytes{{direction=\"transmit\"}}[5m])) * 8) / ({tq})"),
        );
        efficiency.plot_promql(
            PlotOpts::counter(
                "Network RX / Throughput",
                "normalized-network-rx",
                Unit::Count,
            ),
            format!("(sum(irate(network_bytes{{direction=\"receive\"}}[5m])) * 8) / ({tq})"),
        );

        view.group(normalized);
    }

    view
}
