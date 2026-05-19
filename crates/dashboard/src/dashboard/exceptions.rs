use crate::data::DashboardData;
use crate::plot::*;
use crate::sql;

/// Centralized view of true error / fault metrics across subsystems:
/// dropped or failed work, retransmissions, hardware-enforced rate-limit
/// hits, and quota-driven throttling. Excludes ordinary performance
/// counters like cache misses or page faults — those belong in their
/// own subsystem sections.
pub fn generate(data: &dyn DashboardData, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);

    let mut network = Group::new("Network", "network");

    let drops = network.subgroup("Packet Drops & Transmit Errors");
    drops.describe(
        "Packets dropped in the stack and driver-level transmit faults — the \
         canonical signals that the host is shedding or failing network work.",
    );
    drops.plot_sql(
        PlotOpts::counter("Packet Drops", "packet-drops", Unit::Rate),
        sql::irate_total("^network_drop(/[^:]+)?$"),
    );
    drops.plot_sql(
        PlotOpts::counter("Transmit Timeouts", "tx-timeouts", Unit::Rate),
        sql::irate_total("^network_transmit_timeout(/[^:]+)?$"),
    );

    view.group(network);

    let mut tcp = Group::new("TCP", "tcp");

    let retransmits = tcp.subgroup("Retransmits");
    retransmits.describe(
        "TCP segments retransmitted because the peer didn't ack in time — \
         indicates loss or congestion on the path.",
    );
    retransmits.plot_sql_full(
        PlotOpts::counter("TCP Retransmits", "tcp-retransmits", Unit::Rate),
        sql::irate_total("^tcp_retransmit(/[^:]+)?$"),
    );

    view.group(tcp);

    // AWS surfaces these counters when an instance hits a hardware-enforced
    // rate limit. The metrics only appear on ENA-equipped hosts — on
    // everything else the queries resolve to empty series and the panels
    // stay blank, which is the right behavior.
    let mut ena = Group::new("AWS ENA Allowance Exceeded", "ena");

    let bw = ena.subgroup("Bandwidth");
    bw.describe("Packets queued or dropped because the instance hit its bandwidth allowance.");
    bw.plot_sql(
        PlotOpts::counter("Inbound", "ena-bw-rx", Unit::Rate),
        sql::irate_total("^network_ena_bandwidth_allowance_exceeded/receive(/[^:]+)?$"),
    );
    bw.plot_sql(
        PlotOpts::counter("Outbound", "ena-bw-tx", Unit::Rate),
        sql::irate_total("^network_ena_bandwidth_allowance_exceeded/transmit(/[^:]+)?$"),
    );

    let limits = ena.subgroup("Connection & PPS Limits");
    limits.describe(
        "Packets dropped because the instance hit per-second packet, \
         conntrack, or link-local allowances.",
    );
    limits.plot_sql(
        PlotOpts::counter("PPS Allowance", "ena-pps", Unit::Rate),
        sql::irate_total("^network_ena_pps_allowance_exceeded(/[^:]+)?$"),
    );
    limits.plot_sql(
        PlotOpts::counter("Conntrack Allowance", "ena-conntrack", Unit::Rate),
        sql::irate_total("^network_ena_conntrack_allowance_exceeded(/[^:]+)?$"),
    );
    limits.plot_sql(
        PlotOpts::counter("Link-Local Allowance", "ena-linklocal", Unit::Rate),
        sql::irate_total("^network_ena_linklocal_allowance_exceeded(/[^:]+)?$"),
    );

    view.group(ena);

    // CPU Throttling (cgroup CFS bandwidth controller) was historically a
    // PromQL-only group on this section — every plot here was
    // `cgroup_cpu_throttled*`, which is cgroup-keyed and needs the
    // `_cgroup_index` join the cgroups dashboard uses rather than a flat
    // per-id regex. P3 dropped the PromQL fallback (the frontend reads
    // `sql_query` only), so the entire group is omitted until a SQL twin
    // lands. Reinstate when `cgroup_cpu_throttled{,_time}` and
    // `cgroup_cpu_bandwidth_throttled_{periods,time}` have SQL emitters.

    // `blockio_errors` is labeled (op, error) where error buckets coarse
    // blk_status_t classes: io / timeout / nospc / target / protection /
    // unsupported / other. `blockio_requeues` counts requests the block
    // layer put back on the queue (driver couldn't complete; SCSI EH or
    // NVMe controller reset retried under the covers). Pairing them is
    // diagnostic — high requeues with flat errors means recovery is
    // absorbing the fault; both rising means real damage.
    let mut blockio = Group::new("Block IO", "blockio");

    let errors = blockio.subgroup("Errors");
    errors.describe("Terminal block IO failures.");
    // sum by (error): the `error` label is the first path segment of
    // `blockio_errors/<error>/<op>` — extract via regex.
    errors.plot_sql_full(
        PlotOpts::counter("By Class", "blockio-err-by-class", Unit::Rate).with_description(
            "Fault mode: timeout-heavy = controller / transport hang; \
                 nospc = thin pool out of room; protection = data-integrity \
                 check failed.",
        ),
        sql::irate_sum_by_id("^blockio_errors/[^/]+/[^/]+$", "^blockio_errors/([^/]+)/"),
    );
    errors.plot_sql_full(
        PlotOpts::counter("By Op", "blockio-err-by-op", Unit::Rate).with_description(
            "Fault source: a read-only spike points at the media; a \
                 write-only spike points at the controller / target.",
        ),
        sql::irate_sum_by_id(
            "^blockio_errors/[^/]+/[^/]+$",
            "^blockio_errors/[^/]+/([^/]+)$",
        ),
    );

    let requeues = blockio.subgroup("Requeues");
    requeues.describe(
        "Requests the block layer put back on the queue for retry — \
         SCSI error-handler escalation, NVMe controller reset, or \
         multipath path failover. High requeues with flat errors above = \
         transport blip the kernel absorbed cleanly.",
    );
    requeues.plot_sql_full(
        PlotOpts::counter("By Op", "blockio-requeue-by-op", Unit::Rate),
        sql::irate_sum_by_id("^blockio_requeues/[^/]+$", "^blockio_requeues/([^/]+)$"),
    );

    view.group(blockio);

    view
}
