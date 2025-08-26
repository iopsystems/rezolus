use super::*;

pub fn generate(data: &Arc<Tsdb>, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);

    /*
     * CPU
     */

    let mut cpu = Group::new("CPU", "cpu");

    // Average CPU busy percentage
    cpu.plot_promql(
        PlotOpts::line("Busy %", "cpu-busy", Unit::Percentage),
        "sum(irate(cpu_usage[5m])) / cpu_cores / 1000000000".to_string(),
    );

    // Per-CPU busy percentage heatmap
    cpu.plot_promql(
        PlotOpts::heatmap("Busy %", "cpu-busy-heatmap", Unit::Percentage),
        "sum by (id) (irate(cpu_usage[5m])) / 1000000000".to_string(),
    );

    view.group(cpu);

    /*
     * Network
     */

    let mut network = Group::new("Network", "network");

    // Transmit bandwidth
    network.plot_promql(
        PlotOpts::line(
            "Transmit Bandwidth",
            "network-transmit-bandwidth",
            Unit::Bitrate,
        )
        .with_unit_system("bitrate"),
        "sum(irate(network_bytes{direction=\"transmit\"}[5m])) * 8".to_string(),
    );

    // Receive bandwidth
    network.plot_promql(
        PlotOpts::line(
            "Receive Bandwidth",
            "network-receive-bandwidth",
            Unit::Bitrate,
        )
        .with_unit_system("bitrate"),
        "sum(irate(network_bytes{direction=\"receive\"}[5m])) * 8".to_string(),
    );

    // Transmit packets
    network.plot_promql(
        PlotOpts::line("Transmit Packets", "network-transmit-packets", Unit::Rate),
        "sum(irate(network_packets{direction=\"transmit\"}[5m]))".to_string(),
    );

    // Receive packets
    network.plot_promql(
        PlotOpts::line("Receive Packets", "network-receive-packets", Unit::Rate),
        "sum(irate(network_packets{direction=\"receive\"}[5m]))".to_string(),
    );

    // TCP packet latency percentiles
    network.plot_promql(
        PlotOpts::scatter("TCP Packet Latency", "tcp-packet-latency", Unit::Time)
            .with_axis_label("Latency")
            .with_unit_system("time")
            .with_log_scale(true),
        "histogram_percentiles([0.5, 0.9, 0.99, 0.999, 0.9999], tcp_packet_latency)".to_string(),
    );

    view.group(network);

    /*
     * Scheduler
     */

    let mut scheduler = Group::new("Scheduler", "scheduler");

    // Runqueue latency percentiles
    scheduler.plot_promql(
        PlotOpts::scatter("Runqueue Latency", "scheduler-runqueue-latency", Unit::Time)
            .with_axis_label("Latency")
            .with_unit_system("time")
            .with_log_scale(true),
        "histogram_percentiles([0.5, 0.9, 0.99, 0.999, 0.9999], scheduler_runqueue_latency)"
            .to_string(),
    );

    view.group(scheduler);

    /*
     * Syscall
     */

    let mut syscall = Group::new("Syscall", "syscall");

    // Total syscall rate
    syscall.plot_promql(
        PlotOpts::line("Total", "syscall-total", Unit::Rate),
        "sum(irate(syscall[5m]))".to_string(),
    );

    // Syscall latency percentiles
    syscall.plot_promql(
        PlotOpts::scatter("Total", "syscall-total-latency", Unit::Time).with_log_scale(true),
        "histogram_percentiles([0.5, 0.9, 0.99, 0.999, 0.9999], syscall_latency)".to_string(),
    );

    view.group(syscall);

    /*
     * Softirq
     */

    let mut softirq = Group::new("Softirq", "softirq");

    // Total softirq rate
    softirq.plot_promql(
        PlotOpts::line("Rate", "softirq-total-rate", Unit::Rate),
        "sum(irate(softirq[5m]))".to_string(),
    );

    // Per-CPU softirq rate heatmap
    softirq.plot_promql(
        PlotOpts::heatmap("Rate", "softirq-total-rate-heatmap", Unit::Rate),
        "sum by (id) (irate(softirq[5m]))".to_string(),
    );

    // Average CPU % spent in softirq
    softirq.plot_promql(
        PlotOpts::line("CPU %", "softirq-total-time", Unit::Percentage),
        "sum(irate(softirq_time[5m])) / cpu_cores / 1000000000".to_string(),
    );

    // Per-CPU % spent in softirq heatmap
    softirq.plot_promql(
        PlotOpts::heatmap("CPU %", "softirq-total-time-heatmap", Unit::Percentage),
        "sum by (id) (irate(softirq_time[5m])) / 1000000000".to_string(),
    );

    view.group(softirq);

    /*
     * BlockIO
     */

    let mut blockio = Group::new("BlockIO", "blockio");

    // Read throughput
    blockio.plot_promql(
        PlotOpts::line("Read Throughput", "blockio-throughput-read", Unit::Datarate),
        "sum(irate(blockio_bytes{op=\"read\"}[5m]))".to_string(),
    );

    // Write throughput
    blockio.plot_promql(
        PlotOpts::line(
            "Write Throughput",
            "blockio-throughput-write",
            Unit::Datarate,
        ),
        "sum(irate(blockio_bytes{op=\"write\"}[5m]))".to_string(),
    );

    // Read IOPS
    blockio.plot_promql(
        PlotOpts::line("Read IOPS", "blockio-iops-read", Unit::Count),
        "sum(irate(blockio_operations{op=\"read\"}[5m]))".to_string(),
    );

    // Write IOPS
    blockio.plot_promql(
        PlotOpts::line("Write IOPS", "blockio-iops-write", Unit::Count),
        "sum(irate(blockio_operations{op=\"write\"}[5m]))".to_string(),
    );

    view.group(blockio);

    view
}
