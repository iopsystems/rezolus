use crate::MetricsSource;
use crate::plot::*;

pub fn generate(
    data: &dyn MetricsSource,
    sections: Vec<Section>,
    throughput_query: Option<&str>,
) -> View {
    let mut view = View::new(data, sections);

    // Each chart is drawn only when the recording has its metric, and a
    // group with no chart is left out: a macOS agent publishes no scheduler,
    // syscall, softirq or TCP latency metrics.
    if data.has_counter("cpu_usage") {
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
    }

    let mut network = Group::new("Network", "network");

    // Host boundary above the per-interface pair — see `network.rs` for why.
    if has_host_traffic_counters(data) {
        let host = network.subgroup("Host Bandwidth");
        host.describe(
            "Transmit and receive bit rates crossing the host boundary, counted once at the \
             interface bound to a device driver. Matches the NIC's own counters.",
        );
        host.plot_promql(
            PlotOpts::counter(
                "Host Transmit Bandwidth",
                "network-host-transmit-bandwidth",
                Unit::Bitrate,
            )
            .with_unit_system("bitrate"),
            "sum(irate(network_host_bytes{direction=\"transmit\"}[5m])) * 8".to_string(),
        );
        host.plot_promql(
            PlotOpts::counter(
                "Host Receive Bandwidth",
                "network-host-receive-bandwidth",
                Unit::Bitrate,
            )
            .with_unit_system("bitrate"),
            "sum(irate(network_host_bytes{direction=\"receive\"}[5m])) * 8".to_string(),
        );
    }

    if data.has_counter("network_bytes") {
        let bandwidth = network.subgroup("Interface Bandwidth");
        bandwidth.describe(
            "Transmit and receive bit rates summed over every interface traversed. Counts a \
             packet once per stacked layer (bond, VLAN, bridge), and includes intra-host virtual \
             traffic, so this reads higher than the host bandwidth above.",
        );
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
    }

    if data.has_counter("network_packets") {
        let packets = network.subgroup("Interface Packets");
        packets.describe(
            "Transmit and receive packet rates summed over every interface traversed — see the \
             interface bandwidth note above.",
        );
        packets.plot_promql(
            PlotOpts::counter("Transmit Packets", "network-transmit-packets", Unit::Rate),
            "sum(irate(network_packets{direction=\"transmit\"}[5m]))".to_string(),
        );
        packets.plot_promql(
            PlotOpts::counter("Receive Packets", "network-receive-packets", Unit::Rate),
            "sum(irate(network_packets{direction=\"receive\"}[5m]))".to_string(),
        );
    }

    if has_metric(data, "tcp_packet_latency") {
        let tcp = network.subgroup("TCP Latency");
        tcp.describe("Time from packet received to being processed by the application.");
        tcp.plot_promql_full(
            PlotOpts::histogram_latency("TCP Packet Latency", "tcp-packet-latency")
                .with_axis_label("Latency")
                .with_unit_system("time"),
            "tcp_packet_latency".to_string(),
        );
    }

    if !network.is_empty() {
        view.group(network);
    }

    if has_metric(data, "scheduler_runqueue_latency") {
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
    }

    let syscalls = data.has_counter("syscall");
    let syscall_latency = has_metric(data, "syscall_latency");
    if syscalls || syscall_latency {
        let mut syscall = Group::new("Syscall", "syscall");

        let total = syscall.subgroup("Rate & Latency");
        total.describe("Aggregate syscall rate and latency across all operation categories.");
        if syscalls {
            total.plot_promql(
                PlotOpts::counter("Total", "syscall-total", Unit::Rate),
                "sum(irate(syscall[5m]))".to_string(),
            );
        }
        if syscall_latency {
            total.plot_promql(
                PlotOpts::histogram_latency("Total", "syscall-total-latency"),
                "syscall_latency".to_string(),
            );
        }

        view.group(syscall);
    }

    let mut softirq = Group::new("Softirq", "softirq");

    if data.has_counter("softirq") {
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
    }

    if data.has_counter("softirq_time") {
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
    }

    if !softirq.is_empty() {
        view.group(softirq);
    }

    let bytes = data.has_counter("blockio_bytes");
    let operations = data.has_counter("blockio_operations");
    if bytes || operations {
        let mut blockio = Group::new("BlockIO", "blockio");

        let read = blockio.subgroup("Read");
        read.describe("Read throughput and IOPS across all block devices.");
        if bytes {
            read.plot_promql(
                PlotOpts::counter("Read Throughput", "blockio-throughput-read", Unit::Datarate),
                "sum(irate(blockio_bytes{op=\"read\"}[5m]))".to_string(),
            );
        }
        if operations {
            read.plot_promql(
                PlotOpts::counter("Read IOPS", "blockio-iops-read", Unit::Count),
                "sum(irate(blockio_operations{op=\"read\"}[5m]))".to_string(),
            );
        }

        let write = blockio.subgroup("Write");
        write.describe("Write throughput and IOPS across all block devices.");
        if bytes {
            write.plot_promql(
                PlotOpts::counter(
                    "Write Throughput",
                    "blockio-throughput-write",
                    Unit::Datarate,
                ),
                "sum(irate(blockio_bytes{op=\"write\"}[5m]))".to_string(),
            );
        }
        if operations {
            write.plot_promql(
                PlotOpts::counter("Write IOPS", "blockio-iops-write", Unit::Count),
                "sum(irate(blockio_operations{op=\"write\"}[5m]))".to_string(),
            );
        }

        view.group(blockio);
    }

    // Only rendered when a service extension provides a throughput KPI.
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
            PlotOpts::counter("CPU Time / Throughput", "normalized-cpu-busy", Unit::Time),
            format!("sum(irate(cpu_usage[5m])) / ({tq})"),
        );
        // Bits per unit of work is a wire-efficiency question, so it uses the
        // host-boundary counter when the recording has it. Falling back to
        // `network_bytes` keeps the panel populated for older recordings,
        // where it carries that metric's per-layer inflation.
        let net = if has_host_traffic_counters(data) {
            "network_host_bytes"
        } else {
            "network_bytes"
        };
        efficiency.plot_promql(
            PlotOpts::counter(
                "Network TX / Throughput",
                "normalized-network-tx",
                Unit::Count,
            ),
            format!("(sum(irate({net}{{direction=\"transmit\"}}[5m])) * 8) / ({tq})"),
        );
        efficiency.plot_promql(
            PlotOpts::counter(
                "Network RX / Throughput",
                "normalized-network-rx",
                Unit::Count,
            ),
            format!("(sum(irate({net}{{direction=\"receive\"}}[5m])) * 8) / ({tq})"),
        );

        view.group(normalized);
    }

    view
}
