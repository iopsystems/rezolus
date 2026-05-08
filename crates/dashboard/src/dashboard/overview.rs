use crate::data::DashboardData;
use crate::plot::*;
use crate::sql;
use crate::sql::Arg;

pub fn generate(
    data: &dyn DashboardData,
    sections: Vec<Section>,
    throughput_query: Option<&str>,
) -> View {
    let mut view = View::new(data, sections);

    /*
     * CPU
     */

    let mut cpu = Group::new("CPU", "cpu");

    let busy = cpu.subgroup("CPU Busy");
    busy.describe("Overall CPU utilization and per-core breakdown.");
    busy.plot_promql_with_sql(
        PlotOpts::counter("Busy %", "cpu-busy", Unit::Percentage).percentage_range(),
        "sum(irate(cpu_usage[5m])) / cpu_cores / 1000000000".to_string(),
        sql::concept_total(
            "cpu_busy_pct",
            &[
                ("usage", Arg::Sum("^cpu_usage(/[^:]+)?$")),
                ("cores", Arg::Col("cpu_cores")),
            ],
        ),
    );
    busy.plot_promql_with_sql(
        PlotOpts::counter("Busy %", "cpu-busy-heatmap", Unit::Percentage).percentage_range(),
        "sum by (id) (irate(cpu_usage[5m])) / 1000000000".to_string(),
        sql::cpu_pct_by_id("^cpu_usage/[a-z]+/[0-9]+$", "/([0-9]+)$"),
    );

    view.group(cpu);

    /*
     * Network
     */

    let mut network = Group::new("Network", "network");

    let bandwidth = network.subgroup("Bandwidth");
    bandwidth.describe("Transmit and receive bit rates on the wire.");
    bandwidth.plot_promql_with_sql(
        PlotOpts::counter(
            "Transmit Bandwidth",
            "network-transmit-bandwidth",
            Unit::Bitrate,
        )
        .with_unit_system("bitrate"),
        "sum(irate(network_bytes{direction=\"transmit\"}[5m])) * 8".to_string(),
        format!(
            "WITH t AS ({}) SELECT t.t AS t, t.v * 8 AS v FROM t",
            sql::irate_total("^network_bytes/transmit(/[^:]+)?$")
        ),
    );
    bandwidth.plot_promql_with_sql(
        PlotOpts::counter(
            "Receive Bandwidth",
            "network-receive-bandwidth",
            Unit::Bitrate,
        )
        .with_unit_system("bitrate"),
        "sum(irate(network_bytes{direction=\"receive\"}[5m])) * 8".to_string(),
        format!(
            "WITH t AS ({}) SELECT t.t AS t, t.v * 8 AS v FROM t",
            sql::irate_total("^network_bytes/receive(/[^:]+)?$")
        ),
    );

    let packets = network.subgroup("Packets");
    packets.describe("Transmit and receive packet rates.");
    packets.plot_promql_with_sql(
        PlotOpts::counter("Transmit Packets", "network-transmit-packets", Unit::Rate),
        "sum(irate(network_packets{direction=\"transmit\"}[5m]))".to_string(),
        sql::irate_total("^network_packets/transmit(/[^:]+)?$"),
    );
    packets.plot_promql_with_sql(
        PlotOpts::counter("Receive Packets", "network-receive-packets", Unit::Rate),
        "sum(irate(network_packets{direction=\"receive\"}[5m]))".to_string(),
        sql::irate_total("^network_packets/receive(/[^:]+)?$"),
    );

    let tcp = network.subgroup("TCP Latency");
    tcp.describe("Time from packet received to being processed by the application.");
    tcp.plot_promql_with_sql_full(
        PlotOpts::histogram_latency("TCP Packet Latency", "tcp-packet-latency")
            .with_axis_label("Latency")
            .with_unit_system("time"),
        "tcp_packet_latency".to_string(),
        sql::hist_percentile_series("tcp_packet_latency"),
    );

    view.group(network);

    /*
     * Scheduler
     */

    let mut scheduler = Group::new("Scheduler", "scheduler");

    let queueing = scheduler.subgroup("Runqueue Latency");
    queueing.describe("How long tasks waited on the runqueue before getting CPU time.");
    queueing.plot_promql_with_sql_full(
        PlotOpts::histogram_latency("Runqueue Latency", "scheduler-runqueue-latency")
            .with_axis_label("Latency")
            .with_unit_system("time"),
        "scheduler_runqueue_latency".to_string(),
        sql::hist_percentile_series("scheduler_runqueue_latency"),
    );

    view.group(scheduler);

    /*
     * Syscall
     */

    let mut syscall = Group::new("Syscall", "syscall");

    let total = syscall.subgroup("Rate & Latency");
    total.describe("Aggregate syscall rate and latency across all operation categories.");
    total.plot_promql_with_sql(
        PlotOpts::counter("Total", "syscall-total", Unit::Rate),
        "sum(irate(syscall[5m]))".to_string(),
        sql::irate_total("^syscall/[a-z_]+(/[0-9]+)?$"),
    );
    total.plot_promql_with_sql(
        PlotOpts::histogram_latency("Total", "syscall-total-latency"),
        "syscall_latency".to_string(),
        sql::hist_percentile_series_combined("^syscall_latency/[a-z]+:buckets$"),
    );

    view.group(syscall);

    /*
     * Softirq
     */

    let mut softirq = Group::new("Softirq", "softirq");

    let rate = softirq.subgroup("Rate");
    rate.describe("Softirqs handled per second, aggregate and per-CPU.");
    rate.plot_promql_with_sql(
        PlotOpts::counter("Rate", "softirq-total-rate", Unit::Rate),
        "sum(irate(softirq[5m]))".to_string(),
        sql::irate_total("^softirq/[a-z_]+/[0-9]+$"),
    );
    rate.plot_promql_with_sql(
        PlotOpts::counter("Rate", "softirq-total-rate-heatmap", Unit::Rate),
        "sum by (id) (irate(softirq[5m]))".to_string(),
        // Same aggregate-by-id-then-rate pattern as softirq.rs uses for
        // its total per-CPU plot — collapse the kind dimension by id.
        r#"WITH unp AS (
              UNPIVOT (SELECT timestamp, COLUMNS('^softirq/[a-z_]+/[0-9]+$') FROM _src)
                  ON COLUMNS('^softirq/[a-z_]+/[0-9]+$')
                  INTO NAME col VALUE v
           ),
           by_id AS (
              SELECT timestamp,
                     regexp_extract(col, '/([0-9]+)$', 1) AS id,
                     SUM(v) AS s
              FROM unp GROUP BY timestamp, id
           )
           SELECT timestamp::DOUBLE/1e9 AS t, id,
                  irate_lag(
                      s,
                      LAG(s) OVER (PARTITION BY id ORDER BY timestamp),
                      timestamp - LAG(timestamp) OVER (PARTITION BY id ORDER BY timestamp)
                  ) AS v
           FROM by_id"#.to_string(),
    );

    let time = softirq.subgroup("CPU Time");
    time.describe("Fraction of CPU time spent servicing softirqs.");
    time.plot_promql_with_sql(
        PlotOpts::counter("CPU %", "softirq-total-time", Unit::Percentage).percentage_range(),
        "sum(irate(softirq_time[5m])) / cpu_cores / 1000000000".to_string(),
        sql::cpu_pct_total("^softirq_time/[a-z_]+/[0-9]+$"),
    );
    time.plot_promql_with_sql(
        PlotOpts::counter("CPU %", "softirq-total-time-heatmap", Unit::Percentage)
            .percentage_range(),
        "sum by (id) (irate(softirq_time[5m])) / 1000000000".to_string(),
        r#"WITH unp AS (
              UNPIVOT (SELECT timestamp, COLUMNS('^softirq_time/[a-z_]+/[0-9]+$') FROM _src)
                  ON COLUMNS('^softirq_time/[a-z_]+/[0-9]+$')
                  INTO NAME col VALUE v
           ),
           by_id AS (
              SELECT timestamp,
                     regexp_extract(col, '/([0-9]+)$', 1) AS id,
                     SUM(v) AS s
              FROM unp GROUP BY timestamp, id
           )
           SELECT timestamp::DOUBLE/1e9 AS t, id,
                  irate_lag(
                      s,
                      LAG(s) OVER (PARTITION BY id ORDER BY timestamp),
                      timestamp - LAG(timestamp) OVER (PARTITION BY id ORDER BY timestamp)
                  ) / 1e9 AS v
           FROM by_id"#.to_string(),
    );

    view.group(softirq);

    /*
     * BlockIO
     */

    let mut blockio = Group::new("BlockIO", "blockio");

    let read = blockio.subgroup("Read");
    read.describe("Read throughput and IOPS across all block devices.");
    read.plot_promql_with_sql(
        PlotOpts::counter("Read Throughput", "blockio-throughput-read", Unit::Datarate),
        "sum(irate(blockio_bytes{op=\"read\"}[5m]))".to_string(),
        sql::irate_total("^blockio_bytes/read(/[^:]+)?$"),
    );
    read.plot_promql_with_sql(
        PlotOpts::counter("Read IOPS", "blockio-iops-read", Unit::Count),
        "sum(irate(blockio_operations{op=\"read\"}[5m]))".to_string(),
        sql::irate_total("^blockio_operations/read(/[^:]+)?$"),
    );

    let write = blockio.subgroup("Write");
    write.describe("Write throughput and IOPS across all block devices.");
    write.plot_promql_with_sql(
        PlotOpts::counter(
            "Write Throughput",
            "blockio-throughput-write",
            Unit::Datarate,
        ),
        "sum(irate(blockio_bytes{op=\"write\"}[5m]))".to_string(),
        sql::irate_total("^blockio_bytes/write(/[^:]+)?$"),
    );
    write.plot_promql_with_sql(
        PlotOpts::counter("Write IOPS", "blockio-iops-write", Unit::Count),
        "sum(irate(blockio_operations{op=\"write\"}[5m]))".to_string(),
        sql::irate_total("^blockio_operations/write(/[^:]+)?$"),
    );

    view.group(blockio);

    /*
     * Normalized by Throughput (only when service extension provides throughput KPI)
     *
     * The throughput query `{tq}` is user-authored PromQL embedded in
     * parquet metadata. Translating arbitrary PromQL → SQL belongs in
     * `parquet annotate` (see Phase E2 in the plan); these plots stay
     * PromQL-only for now and the legacy viewer renders them. viewer-sql
     * sees no `sql_query` and skips these plots.
     */
    if let Some(tq) = throughput_query {
        let mut normalized = Group::new("Normalized by Throughput", "normalized-throughput");

        let efficiency = normalized.subgroup("Efficiency Metrics");
        efficiency.describe(
            "Resource consumption normalized by service throughput — lower is more efficient.",
        );
        efficiency.plot_promql(
            PlotOpts::counter("CPU Time / Throughput", "normalized-cpu-busy", Unit::Time),
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
