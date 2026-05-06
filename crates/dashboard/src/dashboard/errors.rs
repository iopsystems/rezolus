use crate::Tsdb;
use crate::plot::*;

/// Centralized view of error / fault / health-degradation metrics
/// captured across subsystems. Each group answers a single question:
/// "is this layer dropping work, throttling, or missing?"
pub fn generate(data: &Tsdb, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);

    /*
     * Network
     */

    let mut network = Group::new("Network", "network");

    let drops = network.subgroup("Packet Drops & Transmit Errors");
    drops.describe(
        "Packets dropped in the stack and transmit-side faults — the canonical \
         signals that the host is shedding network work.",
    );
    drops.plot_promql(
        PlotOpts::counter("Packet Drops", "packet-drops", Unit::Rate),
        "sum(irate(network_drop[5m]))".to_string(),
    );
    drops.plot_promql(
        PlotOpts::counter("Transmit Timeouts", "tx-timeouts", Unit::Rate),
        "sum(irate(network_transmit_timeout[5m]))".to_string(),
    );
    drops.plot_promql(
        PlotOpts::counter("Transmit Busy", "tx-busy", Unit::Rate),
        "sum(irate(network_transmit_busy[5m]))".to_string(),
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

    /*
     * CPU misses (cache, TLB, branch)
     *
     * Not "errors" in the textbook sense, but they're the closest thing
     * the CPU has to exception counters: each one is work the pipeline
     * had to redo or stall on. Mispredict / miss rates are normalized
     * against their denominators because absolute counts scale with load.
     */

    let mut misses = Group::new("Cache & Prediction Misses", "cpu-misses");

    let l3 = misses.subgroup("L3 Cache Miss Rate");
    l3.describe("Fraction of last-level cache accesses that missed.");
    l3.plot_promql(
        PlotOpts::counter("L3 Miss %", "l3-miss-rate", Unit::Percentage).percentage_range(),
        "sum(irate(cpu_l3_miss[5m])) / sum(irate(cpu_l3_access[5m]))".to_string(),
    );
    l3.plot_promql(
        PlotOpts::counter("L3 Misses", "l3-misses", Unit::Rate),
        "sum(irate(cpu_l3_miss[5m]))".to_string(),
    );

    let dtlb = misses.subgroup("DTLB Misses");
    dtlb.describe("Data-TLB miss rate — page-table walks the CPU had to take.");
    dtlb.plot_promql(
        PlotOpts::counter("DTLB Misses", "dtlb-misses", Unit::Rate),
        "sum(irate(cpu_dtlb_miss[5m]))".to_string(),
    );

    let branch = misses.subgroup("Branch Mispredictions");
    branch.describe(
        "Fraction of branches the predictor got wrong. Each miss is a \
         pipeline flush.",
    );
    branch.plot_promql(
        PlotOpts::counter("Misprediction %", "branch-miss-rate", Unit::Percentage)
            .percentage_range(),
        "sum(irate(cpu_branch_misses[5m])) / sum(irate(cpu_branch_instructions[5m]))".to_string(),
    );
    branch.plot_promql(
        PlotOpts::counter("Mispredictions", "branch-misses", Unit::Rate),
        "sum(irate(cpu_branch_misses[5m]))".to_string(),
    );

    view.group(misses);

    /*
     * Memory faults & NUMA placement
     */

    let mut memory = Group::new("Memory", "memory");

    let faults = memory.subgroup("Page Faults");
    faults.describe(
        "Major page faults required disk I/O to satisfy. Sustained rates \
         indicate memory pressure on the Rezolus process itself; for \
         broader process coverage, run with /usr/bin/time -v or eBPF.",
    );
    faults.plot_promql_full(
        PlotOpts::counter("Major Page Faults", "rezolus-major-faults", Unit::Rate),
        "sum(irate(rezolus_memory_page_faults[5m]))".to_string(),
    );

    let numa = memory.subgroup("NUMA Misses");
    numa.describe(
        "NUMA allocations that didn't land on the intended node. \
         High values mean cross-socket memory traffic.",
    );
    numa.plot_promql(
        PlotOpts::counter("NUMA Miss", "numa-miss", Unit::Rate),
        "sum(irate(memory_numa_miss[5m]))".to_string(),
    );
    numa.plot_promql(
        PlotOpts::counter("NUMA Foreign", "numa-foreign", Unit::Rate),
        "sum(irate(memory_numa_foreign[5m]))".to_string(),
    );

    view.group(memory);

    view
}
