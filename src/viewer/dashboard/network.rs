use super::*;

pub fn generate(data: &Tsdb, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);

    /*
     * Traffic
     */

    let mut traffic = Group::new("Traffic", "traffic");

    traffic.plot(
        PlotOpts::line("Bandwidth Transmit", "bandwidth-tx", Unit::Bitrate)
            .with_unit_system("bitrate"),
        data.counters("network_bytes", [("direction", "transmit")])
            .map(|v| v.rate().sum())
            .map(|v| v * 8.0),
    );

    traffic.plot(
        PlotOpts::line("Bandwidth Receive", "bandwidth-rx", Unit::Bitrate)
            .with_unit_system("bitrate"),
        data.counters("network_bytes", [("direction", "receive")])
            .map(|v| v.rate().sum())
            .map(|v| v * 8.0),
    );

    traffic.plot(
        PlotOpts::line("Packets Transmit", "packets-tx", Unit::Rate),
        data.counters("network_packets", [("direction", "transmit")])
            .map(|v| v.rate().sum()),
    );

    traffic.plot(
        PlotOpts::line("Packets Receive", "packets-rx", Unit::Rate),
        data.counters("network_packets", [("direction", "receive")])
            .map(|v| v.rate().sum()),
    );

    view.group(traffic);

    /*
     * TCP
     */

    let mut tcp = Group::new("TCP", "tcp");

    tcp.scatter(
        PlotOpts::scatter("Packet Latency", "tcp-packet-latency", Unit::Time)
            .with_axis_label("Latency")
            .with_unit_system("time")
            .with_log_scale(true),
        data.percentiles("tcp_packet_latency", (), PERCENTILES),
    );

    view.group(tcp);

    view
}
