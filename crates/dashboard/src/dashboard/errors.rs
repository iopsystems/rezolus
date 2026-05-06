use crate::Tsdb;
use crate::plot::*;

/// Centralized view of true error / fault metrics across subsystems:
/// dropped or failed work, retransmissions, hardware-enforced rate-limit
/// hits, and quota-driven throttling. Excludes ordinary performance
/// counters like cache misses or page faults — those belong in their
/// own subsystem sections.
pub fn generate(data: &Tsdb, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);

    /*
     * Network
     */

    let mut network = Group::new("Network", "network");

    let drops = network.subgroup("Packet Drops & Transmit Errors");
    drops.describe(
        "Packets dropped in the stack and driver-level transmit faults — the \
         canonical signals that the host is shedding or failing network work.",
    );
    drops.plot_promql(
        PlotOpts::counter("Packet Drops", "packet-drops", Unit::Rate),
        "sum(irate(network_drop[5m]))".to_string(),
    );
    drops.plot_promql(
        PlotOpts::counter("Transmit Timeouts", "tx-timeouts", Unit::Rate),
        "sum(irate(network_transmit_timeout[5m]))".to_string(),
    );

    view.group(network);

    /*
     * TCP
     */

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

    /*
     * Cloud (ENA) allowance limits
     *
     * AWS surfaces these counters when an instance hits a hardware-enforced
     * rate limit. The metrics only appear on ENA-equipped hosts — on
     * everything else the queries resolve to empty series and the panels
     * stay blank, which is the right behavior.
     */

    let mut ena = Group::new("AWS ENA Allowance Exceeded", "ena");

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

    let limits = ena.subgroup("Connection & PPS Limits");
    limits.describe(
        "Packets dropped because the instance hit per-second packet, \
         conntrack, or link-local allowances.",
    );
    limits.plot_promql(
        PlotOpts::counter("PPS Allowance", "ena-pps", Unit::Rate),
        "sum(irate(network_ena_pps_allowance_exceeded[5m]))".to_string(),
    );
    limits.plot_promql(
        PlotOpts::counter("Conntrack Allowance", "ena-conntrack", Unit::Rate),
        "sum(irate(network_ena_conntrack_allowance_exceeded[5m]))".to_string(),
    );
    limits.plot_promql(
        PlotOpts::counter("Link-Local Allowance", "ena-linklocal", Unit::Rate),
        "sum(irate(network_ena_linklocal_allowance_exceeded[5m]))".to_string(),
    );

    view.group(ena);

    /*
     * CPU throttling
     */

    let mut throttle = Group::new("CPU Throttling", "cpu-throttling");

    let events = throttle.subgroup("Throttle Events");
    events.describe(
        "Cgroups whose CPU runqueues were throttled by the CFS bandwidth \
         controller — workload was ready to run but blocked by quota.",
    );
    events.plot_promql(
        PlotOpts::counter("Throttle Events", "cgroup-throttle-events", Unit::Rate),
        "sum(irate(cgroup_cpu_throttled[5m]))".to_string(),
    );
    events.plot_promql(
        PlotOpts::counter("Throttled Periods", "cgroup-throttled-periods", Unit::Rate),
        "sum(irate(cgroup_cpu_bandwidth_throttled_periods[5m]))".to_string(),
    );

    let time = throttle.subgroup("Throttled Time");
    time.describe(
        "Time cgroups spent waiting on the CFS bandwidth quota. \
         Sustained non-zero values mean quota is the bottleneck.",
    );
    time.plot_promql(
        PlotOpts::counter("Throttled Time", "cgroup-throttled-time", Unit::Percentage)
            .with_unit_system("percentage"),
        "sum(irate(cgroup_cpu_throttled_time[5m])) / 1000000000".to_string(),
    );
    time.plot_promql(
        PlotOpts::counter(
            "Bandwidth Throttled Time",
            "cgroup-bw-throttled-time",
            Unit::Percentage,
        )
        .with_unit_system("percentage"),
        "sum(irate(cgroup_cpu_bandwidth_throttled_time[5m])) / 1000000000".to_string(),
    );

    view.group(throttle);

    view
}
