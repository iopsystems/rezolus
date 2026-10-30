use crate::MetricsSource;
use crate::plot::*;

/// Centralized view of true error / fault metrics across subsystems:
/// dropped or failed work, retransmissions, hardware-enforced rate-limit
/// hits, and quota-driven throttling. Excludes ordinary performance
/// counters like cache misses or page faults — those belong in their
/// own subsystem sections.
pub fn generate(data: &dyn MetricsSource, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);

    // Each chart is drawn only when the recording has its metric, and a
    // group with no chart is left out. A quiet counter is still drawn, as a
    // flat line; a missing one means nothing on this host measured it, such
    // as the ENA counters on a host without ENA interfaces, or everything but
    // tcp_retransmit on a Mac.
    let drops = data.has_counter("network_drop");
    let timeouts = data.has_counter("network_transmit_timeout");
    if drops || timeouts {
        let mut network = Group::new("Network", "network");

        let errors = network.subgroup("Packet Drops & Transmit Errors");
        errors.describe(
            "Packets dropped in the stack and driver-level transmit faults — the \
             canonical signals that the host is shedding or failing network work.",
        );
        if drops {
            errors.plot_promql(
                PlotOpts::counter("Packet Drops", "packet-drops", Unit::Rate),
                "sum(irate(network_drop[5m]))".to_string(),
            );
        }
        if timeouts {
            errors.plot_promql(
                PlotOpts::counter("Transmit Timeouts", "tx-timeouts", Unit::Rate),
                "sum(irate(network_transmit_timeout[5m]))".to_string(),
            );
        }

        view.group(network);
    }

    if data.has_counter("tcp_retransmit") {
        let mut tcp = Group::new("TCP", "tcp");

        let retransmits = tcp.subgroup("Retransmits");
        retransmits.describe(
            "TCP segments retransmitted because the peer didn't ack in time — \
             indicates loss or congestion on the path.",
        );
        retransmits.plot_promql_full(
            PlotOpts::counter("TCP Retransmits", "tcp-retransmits", Unit::Rate),
            "sum(irate(tcp_retransmit[5m]))".to_string(),
        );

        view.group(tcp);
    }

    // AWS surfaces these counters when an instance hits a hardware-enforced
    // rate limit. The metrics only appear on ENA-equipped hosts.
    let mut ena = Group::new("AWS ENA Allowance Exceeded", "ena");

    if data.has_counter("network_ena_bandwidth_allowance_exceeded") {
        let bw = ena.subgroup("Bandwidth");
        bw.describe("Packets queued or dropped because the instance hit its bandwidth allowance.");
        bw.plot_promql(
            PlotOpts::counter("Inbound", "ena-bw-rx", Unit::Rate),
            "sum(irate(network_ena_bandwidth_allowance_exceeded{direction=\"receive\"}[5m]))"
                .to_string(),
        );
        bw.plot_promql(
            PlotOpts::counter("Outbound", "ena-bw-tx", Unit::Rate),
            "sum(irate(network_ena_bandwidth_allowance_exceeded{direction=\"transmit\"}[5m]))"
                .to_string(),
        );
    }

    let limits = [
        (
            "PPS Allowance",
            "ena-pps",
            "network_ena_pps_allowance_exceeded",
        ),
        (
            "Conntrack Allowance",
            "ena-conntrack",
            "network_ena_conntrack_allowance_exceeded",
        ),
        (
            "Link-Local Allowance",
            "ena-linklocal",
            "network_ena_linklocal_allowance_exceeded",
        ),
    ];
    if limits.iter().any(|(_, _, m)| data.has_counter(m)) {
        let subgroup = ena.subgroup("Connection & PPS Limits");
        subgroup.describe(
            "Packets dropped because the instance hit per-second packet, \
             conntrack, or link-local allowances.",
        );
        for (title, id, metric) in limits {
            if data.has_counter(metric) {
                subgroup.plot_promql(
                    PlotOpts::counter(title, id, Unit::Rate),
                    format!("sum(irate({metric}[5m]))"),
                );
            }
        }
    }

    if !ena.is_empty() {
        view.group(ena);
    }

    let mut throttle = Group::new("CPU Throttling", "cpu-throttling");

    let throttled = data.has_counter("cgroup_cpu_throttled");
    let periods = data.has_counter("cgroup_cpu_bandwidth_throttled_periods");
    if throttled || periods {
        let events = throttle.subgroup("Throttle Events");
        events.describe(
            "Cgroups whose CPU runqueues were throttled by the CFS bandwidth \
             controller — workload was ready to run but blocked by quota.",
        );
        if throttled {
            events.plot_promql(
                PlotOpts::counter("Throttle Events", "cgroup-throttle-events", Unit::Rate),
                "sum(irate(cgroup_cpu_throttled[5m]))".to_string(),
            );
        }
        if periods {
            events.plot_promql(
                PlotOpts::counter("Throttled Periods", "cgroup-throttled-periods", Unit::Rate),
                "sum(irate(cgroup_cpu_bandwidth_throttled_periods[5m]))".to_string(),
            );
        }
    }

    let throttled_time = data.has_counter("cgroup_cpu_throttled_time");
    let bandwidth_time = data.has_counter("cgroup_cpu_bandwidth_throttled_time");
    if throttled_time || bandwidth_time {
        let time = throttle.subgroup("Throttled Time");
        time.describe(
            "Time cgroups spent waiting on the CFS bandwidth quota. \
             Sustained non-zero values mean quota is the bottleneck.",
        );
        if throttled_time {
            time.plot_promql(
                PlotOpts::counter("Throttled Time", "cgroup-throttled-time", Unit::Percentage)
                    .with_unit_system("percentage"),
                "sum(irate(cgroup_cpu_throttled_time[5m])) / 1000000000".to_string(),
            );
        }
        if bandwidth_time {
            time.plot_promql(
                PlotOpts::counter(
                    "Bandwidth Throttled Time",
                    "cgroup-bw-throttled-time",
                    Unit::Percentage,
                )
                .with_unit_system("percentage"),
                "sum(irate(cgroup_cpu_bandwidth_throttled_time[5m])) / 1000000000".to_string(),
            );
        }
    }

    if !throttle.is_empty() {
        view.group(throttle);
    }

    // `blockio_errors` is labeled (op, error) where error buckets coarse
    // blk_status_t classes: io / timeout / nospc / target / protection /
    // unsupported / other. `blockio_requeues` counts requests the block
    // layer put back on the queue (driver couldn't complete; SCSI EH or
    // NVMe controller reset retried under the covers). Pairing them is
    // diagnostic — high requeues with flat errors means recovery is
    // absorbing the fault; both rising means real damage.
    let mut blockio = Group::new("Block IO", "blockio");

    if data.has_counter("blockio_errors") {
        let errors = blockio.subgroup("Errors");
        errors.describe("Terminal block IO failures.");
        errors.plot_promql_full(
            PlotOpts::counter("By Class", "blockio-err-by-class", Unit::Rate).with_description(
                "Fault mode: timeout-heavy = controller / transport hang; \
                     nospc = thin pool out of room; protection = data-integrity \
                     check failed.",
            ),
            "sum by (error) (irate(blockio_errors[5m]))".to_string(),
        );
        errors.plot_promql_full(
            PlotOpts::counter("By Op", "blockio-err-by-op", Unit::Rate).with_description(
                "Fault source: a read-only spike points at the media; a \
                     write-only spike points at the controller / target.",
            ),
            "sum by (op) (irate(blockio_errors[5m]))".to_string(),
        );
    }

    if data.has_counter("blockio_requeues") {
        let requeues = blockio.subgroup("Requeues");
        requeues.describe(
            "Requests the block layer put back on the queue for retry — \
             SCSI error-handler escalation, NVMe controller reset, or \
             multipath path failover. High requeues with flat errors above = \
             transport blip the kernel absorbed cleanly.",
        );
        requeues.plot_promql_full(
            PlotOpts::counter("By Op", "blockio-requeue-by-op", Unit::Rate),
            "sum by (op) (irate(blockio_requeues[5m]))".to_string(),
        );
    }

    if !blockio.is_empty() {
        view.group(blockio);
    }

    view
}
