use super::*;

pub fn generate(data: &Arc<Tsdb>, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);

    /*
     * Traffic
     */

    let mut traffic = Group::new("Traffic", "traffic");

    // Bandwidth Transmit (convert bytes/sec to bits/sec)
    traffic.plot_promql(
        PlotOpts::line("Bandwidth Transmit", "bandwidth-tx", Unit::Bitrate)
            .with_unit_system("bitrate"),
        "sum(irate(network_bytes{direction=\"transmit\"}[5m])) * 8".to_string(),
    );

    // Bandwidth Receive (convert bytes/sec to bits/sec)
    traffic.plot_promql(
        PlotOpts::line("Bandwidth Receive", "bandwidth-rx", Unit::Bitrate)
            .with_unit_system("bitrate"),
        "sum(irate(network_bytes{direction=\"receive\"}[5m])) * 8".to_string(),
    );

    // Packets Transmit
    traffic.plot_promql(
        PlotOpts::line("Packets Transmit", "packets-tx", Unit::Rate),
        "sum(irate(network_packets{direction=\"transmit\"}[5m]))".to_string(),
    );

    // Packets Receive
    traffic.plot_promql(
        PlotOpts::line("Packets Receive", "packets-rx", Unit::Rate),
        "sum(irate(network_packets{direction=\"receive\"}[5m]))".to_string(),
    );

    view.group(traffic);

    /*
     * TCP
     */

    let mut tcp = Group::new("TCP", "tcp");

    // TCP Packet Latency percentiles - p50, p90, p99, p99.9
    // Use the efficient histogram_percentiles() function to compute all percentiles in one pass
    // Note: histogram_percentiles works directly on histogram data, not on irate() results
    tcp.plot_promql(
        PlotOpts::scatter("TCP Packet Latency", "tcp-packet-latency", Unit::Time)
            .with_axis_label("Latency")
            .with_unit_system("time")
            .with_log_scale(true),
        "histogram_percentiles([0.5, 0.9, 0.99, 0.999], tcp_packet_latency)".to_string(),
    );

    view.group(tcp);

    view
}
