use crate::data::DashboardData;
use crate::plot::*;
use crate::sql;
use crate::sql::Arg;

pub fn generate(data: &dyn DashboardData, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);

    let mut cpu = Group::new("CPU", "cpu");

    let busy = cpu.subgroup("CPU Busy");
    busy.describe("Overall CPU utilization and per-core breakdown.");
    busy.plot_sql(
        PlotOpts::counter("Busy %", "cpu-busy", Unit::Percentage).percentage_range(),
        sql::concept_total(
            "cpu_busy_pct",
            &[
                ("usage", Arg::Sum("^cpu_usage(/[^:]+)?$")),
                ("cores", Arg::Col("cpu_cores")),
            ],
        ),
    );
    busy.plot_sql(
        PlotOpts::counter("Busy %", "cpu-busy-heatmap", Unit::Percentage).percentage_range(),
        sql::cpu_pct_by_id("^cpu_usage/[a-z]+/[0-9]+$", "/([0-9]+)$"),
    );

    view.group(cpu);

    let mut network = Group::new("Network", "network");

    let bandwidth = network.subgroup("Bandwidth");
    bandwidth.describe("Transmit and receive bit rates on the wire.");
    bandwidth.plot_sql(
        PlotOpts::counter(
            "Transmit Bandwidth",
            "network-transmit-bandwidth",
            Unit::Bitrate,
        )
        .with_unit_system("bitrate"),
        format!(
            "WITH t AS ({}) SELECT t.t AS t, t.v * 8 AS v FROM t",
            sql::irate_total("^network_bytes/transmit(/[^:]+)?$")
        ),
    );
    bandwidth.plot_sql(
        PlotOpts::counter(
            "Receive Bandwidth",
            "network-receive-bandwidth",
            Unit::Bitrate,
        )
        .with_unit_system("bitrate"),
        format!(
            "WITH t AS ({}) SELECT t.t AS t, t.v * 8 AS v FROM t",
            sql::irate_total("^network_bytes/receive(/[^:]+)?$")
        ),
    );

    let packets = network.subgroup("Packets");
    packets.describe("Transmit and receive packet rates.");
    packets.plot_sql(
        PlotOpts::counter("Transmit Packets", "network-transmit-packets", Unit::Rate),
        sql::irate_total("^network_packets/transmit(/[^:]+)?$"),
    );
    packets.plot_sql(
        PlotOpts::counter("Receive Packets", "network-receive-packets", Unit::Rate),
        sql::irate_total("^network_packets/receive(/[^:]+)?$"),
    );

    let tcp = network.subgroup("TCP Latency");
    tcp.describe("Time from packet received to being processed by the application.");
    tcp.plot_sql_full(
        PlotOpts::histogram_latency("TCP Packet Latency", "tcp-packet-latency")
            .with_metric("tcp_packet_latency")
            .with_axis_label("Latency")
            .with_unit_system("time"),
        sql::hist_percentile_series("tcp_packet_latency"),
    );

    view.group(network);

    let mut scheduler = Group::new("Scheduler", "scheduler");

    let queueing = scheduler.subgroup("Runqueue Latency");
    queueing.describe("How long tasks waited on the runqueue before getting CPU time.");
    queueing.plot_sql_full(
        PlotOpts::histogram_latency("Runqueue Latency", "scheduler-runqueue-latency")
            .with_metric("scheduler_runqueue_latency")
            .with_axis_label("Latency")
            .with_unit_system("time"),
        sql::hist_percentile_series("scheduler_runqueue_latency"),
    );

    view.group(scheduler);

    let mut syscall = Group::new("Syscall", "syscall");

    let total = syscall.subgroup("Rate & Latency");
    total.describe("Aggregate syscall rate and latency across all operation categories.");
    total.plot_sql(
        PlotOpts::counter("Total", "syscall-total", Unit::Rate),
        sql::irate_total("^syscall/[a-z_]+(/[0-9]+)?$"),
    );
    total.plot_sql(
        PlotOpts::histogram_latency("Total", "syscall-total-latency"),
        sql::hist_percentile_series_combined("^syscall_latency/[a-z]+:buckets$"),
    );

    view.group(syscall);

    let mut softirq = Group::new("Softirq", "softirq");

    let rate = softirq.subgroup("Rate");
    rate.describe("Softirqs handled per second, aggregate and per-CPU.");
    rate.plot_sql(
        PlotOpts::counter("Rate", "softirq-total-rate", Unit::Rate),
        sql::irate_total("^softirq/[a-z_]+/[0-9]+$"),
    );
    rate.plot_sql(
        PlotOpts::counter("Rate", "softirq-total-rate-heatmap", Unit::Rate),
        sql::irate_sum_by_id("^softirq/[a-z_]+/[0-9]+$", "/([0-9]+)$"),
    );

    let time = softirq.subgroup("CPU Time");
    time.describe("Fraction of CPU time spent servicing softirqs.");
    time.plot_sql(
        PlotOpts::counter("CPU %", "softirq-total-time", Unit::Percentage).percentage_range(),
        sql::cpu_pct_total("^softirq_time/[a-z_]+/[0-9]+$"),
    );
    time.plot_sql(
        PlotOpts::counter("CPU %", "softirq-total-time-heatmap", Unit::Percentage)
            .percentage_range(),
        sql::scale_v(
            sql::irate_sum_by_id("^softirq_time/[a-z_]+/[0-9]+$", "/([0-9]+)$"),
            1e9,
        ),
    );

    view.group(softirq);

    let mut blockio = Group::new("BlockIO", "blockio");

    let read = blockio.subgroup("Read");
    read.describe("Read throughput and IOPS across all block devices.");
    read.plot_sql(
        PlotOpts::counter("Read Throughput", "blockio-throughput-read", Unit::Datarate),
        sql::irate_total("^blockio_bytes/read(/[^:]+)?$"),
    );
    read.plot_sql(
        PlotOpts::counter("Read IOPS", "blockio-iops-read", Unit::Count),
        sql::irate_total("^blockio_operations/read(/[^:]+)?$"),
    );

    let write = blockio.subgroup("Write");
    write.describe("Write throughput and IOPS across all block devices.");
    write.plot_sql(
        PlotOpts::counter(
            "Write Throughput",
            "blockio-throughput-write",
            Unit::Datarate,
        ),
        sql::irate_total("^blockio_bytes/write(/[^:]+)?$"),
    );
    write.plot_sql(
        PlotOpts::counter("Write IOPS", "blockio-iops-write", Unit::Count),
        sql::irate_total("^blockio_operations/write(/[^:]+)?$"),
    );

    view.group(blockio);

    view
}
