use crate::data::DashboardData;
use crate::plot::*;
use crate::sql;

pub fn generate(data: &dyn DashboardData, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);

    let mut traffic = Group::new("Traffic", "traffic");

    let bandwidth = traffic.subgroup("Bandwidth");
    bandwidth.describe("Bits per second on the wire, transmit and receive.");
    // PromQL `* 8` converts bytes/sec → bits/sec; mirror that on the SQL.
    bandwidth.plot_promql_with_sql(
        PlotOpts::counter("Bandwidth Transmit", "bandwidth-tx", Unit::Bitrate)
            .with_unit_system("bitrate"),
        "sum(irate(network_bytes{direction=\"transmit\"}[5m])) * 8".to_string(),
        format!(
            "WITH t AS ({}) SELECT t.t AS t, t.v * 8 AS v FROM t",
            sql::irate_total("^network_bytes/transmit(/[^:]+)?$")
        ),
    );
    bandwidth.plot_promql_with_sql(
        PlotOpts::counter("Bandwidth Receive", "bandwidth-rx", Unit::Bitrate)
            .with_unit_system("bitrate"),
        "sum(irate(network_bytes{direction=\"receive\"}[5m])) * 8".to_string(),
        format!(
            "WITH t AS ({}) SELECT t.t AS t, t.v * 8 AS v FROM t",
            sql::irate_total("^network_bytes/receive(/[^:]+)?$")
        ),
    );

    let packets = traffic.subgroup("Packets");
    packets.describe("Packet rate on the wire, transmit and receive.");
    packets.plot_promql_with_sql(
        PlotOpts::counter("Packets Transmit", "packets-tx", Unit::Rate),
        "sum(irate(network_packets{direction=\"transmit\"}[5m]))".to_string(),
        sql::irate_total("^network_packets/transmit(/[^:]+)?$"),
    );
    packets.plot_promql_with_sql(
        PlotOpts::counter("Packets Receive", "packets-rx", Unit::Rate),
        "sum(irate(network_packets{direction=\"receive\"}[5m]))".to_string(),
        sql::irate_total("^network_packets/receive(/[^:]+)?$"),
    );

    view.group(traffic);

    let mut errors = Group::new("Errors", "errors");

    let health = errors.subgroup("Drops & Retransmits");
    health.describe(
        "Packets dropped at the network layer and TCP-level retransmissions — key health signals.",
    );
    health.plot_promql_with_sql(
        PlotOpts::counter("Packet Drops", "packet-drops", Unit::Rate),
        "sum(irate(network_drop[5m]))".to_string(),
        sql::irate_total("^network_drop(/[^:]+)?$"),
    );
    health.plot_promql_with_sql(
        PlotOpts::counter("TCP Retransmits", "tcp-retransmits", Unit::Rate),
        "sum(irate(tcp_retransmit[5m]))".to_string(),
        sql::irate_total("^tcp_retransmit(/[^:]+)?$"),
    );

    view.group(errors);

    let mut tcp = Group::new("TCP", "tcp");

    let latency = tcp.subgroup("Packet Latency");
    latency.describe("Time from packet received to being processed by the application.");
    latency.plot_promql_with_sql_full(
        PlotOpts::histogram_latency("TCP Packet Latency", "tcp-packet-latency")
            .with_axis_label("Latency")
            .with_unit_system("time"),
        "tcp_packet_latency".to_string(),
        sql::hist_percentile_series("tcp_packet_latency"),
    );

    view.group(tcp);

    view
}
