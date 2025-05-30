use super::*;

pub fn generate(data: &Tsdb, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);

    /*
     * CPU
     */

    let mut cpu = Group::new("CPU", "cpu");

    cpu.push(Plot::line(
        "Busy %",
        "cpu-busy",
        Unit::Percentage,
        data.cpu_avg("cpu_usage", ()).map(|v| (v / 1000000000.0)),
    ));

    cpu.push(Plot::heatmap(
        "Busy %",
        "cpu-busy-heatmap",
        Unit::Percentage,
        data.cpu_heatmap("cpu_usage", ()).map(|v| v / 1000000000.0),
    ));

    view.group(cpu);

    /*
     * Network
     */

    let mut network = Group::new("Network", "network");

    network.push(Plot::line(
        "Transmit Bandwidth",
        "network-transmit-bandwidth",
        Unit::Bitrate,
        data.counters("network_bytes", [("direction", "transmit")])
            .map(|v| v.rate().sum())
            .map(|v| v * 8.0),
    ));

    network.push(Plot::line(
        "Receive Bandwidth",
        "network-receive-bandwidth",
        Unit::Bitrate,
        data.counters("network_bytes", [("direction", "receive")])
            .map(|v| v.rate().sum())
            .map(|v| v * 8.0),
    ));

    network.push(Plot::line(
        "Transmit Packets",
        "network-transmit-packets",
        Unit::Rate,
        data.counters("network_packets", [("direction", "transmit")])
            .map(|v| v.rate().sum()),
    ));

    network.push(Plot::line(
        "Receive Packets",
        "network-receive-packets",
        Unit::Rate,
        data.counters("network_packets", [("direction", "receive")])
            .map(|v| v.rate().sum()),
    ));

    network.scatter(
        PlotOpts::scatter("TCP Packet Latency", "tcp-packet-latency", Unit::Time)
            .with_axis_label("Latency")
            .with_unit_system("time")
            .with_log_scale(true),
        data.percentiles("tcp_packet_latency", (), PERCENTILES),
    );

    view.group(network);

    /*
     * Scheduler
     */

    let mut scheduler = Group::new("Scheduler", "scheduler");

    scheduler.scatter(
        PlotOpts::scatter("Runqueue Latency", "scheduler-runqueue-latency", Unit::Time)
            .with_axis_label("Latency")
            .with_unit_system("time")
            .with_log_scale(true),
        data.percentiles("scheduler_runqueue_latency", (), PERCENTILES),
    );

    view.group(scheduler);

    /*
     * Syscall
     */

    let mut syscall = Group::new("Syscall", "syscall");

    syscall.plot(
        PlotOpts::line("Total", "syscall-total", Unit::Rate),
        data.counters("syscall", ()).map(|v| v.rate().sum()),
    );

    syscall.scatter(
        PlotOpts::scatter("Total", "syscall-total-latency", Unit::Time).with_log_scale(true),
        data.percentiles("syscall_latency", (), PERCENTILES),
    );

    view.group(syscall);

    /*
     * Softirq
     */

    let mut softirq = Group::new("Softirq", "softirq");

    softirq.plot(
        PlotOpts::line("Rate", "softirq-total-rate", Unit::Rate),
        data.counters("softirq", ()).map(|v| v.rate().sum()),
    );

    softirq.heatmap_echarts(
        PlotOpts::heatmap("Rate", "softirq-total-rate-heatmap", Unit::Rate),
        data.cpu_heatmap("softirq", ()),
    );

    softirq.plot(
        PlotOpts::line("CPU %", "softirq-total-time", Unit::Percentage),
        data.cpu_avg("softirq_time", ()).map(|v| v / 1000000000.0),
    );

    softirq.heatmap_echarts(
        PlotOpts::heatmap("CPU %", "softirq-total-time-heatmap", Unit::Percentage),
        data.cpu_heatmap("softirq_time", ())
            .map(|v| v / 1000000000.0),
    );

    view.group(softirq);

    /*
     * BlockIO
     */

    let mut blockio = Group::new("BlockIO", "blockio");

    blockio.plot(
        PlotOpts::line("Read Throughput", "blockio-throughput-read", Unit::Datarate),
        data.counters("blockio_bytes", [("op", "read")])
            .map(|v| v.rate().sum()),
    );

    blockio.plot(
        PlotOpts::line(
            "Write Throughput",
            "blockio-throughput-write",
            Unit::Datarate,
        ),
        data.counters("blockio_bytes", [("op", "write")])
            .map(|v| v.rate().sum()),
    );

    blockio.plot(
        PlotOpts::line("Read IOPS", "blockio-iops-read", Unit::Count),
        data.counters("blockio_operations", [("op", "read")])
            .map(|v| v.rate().sum()),
    );

    blockio.plot(
        PlotOpts::line("Write IOPS", "blockio-iops-write", Unit::Count),
        data.counters("blockio_operations", [("op", "write")])
            .map(|v| v.rate().sum()),
    );

    view.group(blockio);

    view
}
