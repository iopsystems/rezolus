use crate::MetricsSource;
use crate::plot::*;

pub fn generate(data: &dyn MetricsSource, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);

    let mut traffic = Group::new("Traffic", "traffic");

    let bandwidth = traffic.subgroup("Bandwidth");
    bandwidth.describe("Bits per second on the wire, transmit and receive.");
    bandwidth.plot_promql(
        PlotOpts::counter("Bandwidth Transmit", "bandwidth-tx", Unit::Bitrate)
            .with_unit_system("bitrate"),
        "sum(irate(network_bytes{direction=\"transmit\"}[5m])) * 8".to_string(),
    );
    bandwidth.plot_promql(
        PlotOpts::counter("Bandwidth Receive", "bandwidth-rx", Unit::Bitrate)
            .with_unit_system("bitrate"),
        "sum(irate(network_bytes{direction=\"receive\"}[5m])) * 8".to_string(),
    );

    let packets = traffic.subgroup("Packets");
    packets.describe("Packet rate on the wire, transmit and receive.");
    packets.plot_promql(
        PlotOpts::counter("Packets Transmit", "packets-tx", Unit::Rate),
        "sum(irate(network_packets{direction=\"transmit\"}[5m]))".to_string(),
    );
    packets.plot_promql(
        PlotOpts::counter("Packets Receive", "packets-rx", Unit::Rate),
        "sum(irate(network_packets{direction=\"receive\"}[5m]))".to_string(),
    );

    view.group(traffic);

    let mut errors = Group::new("Errors", "errors");

    let health = errors.subgroup("Drops & Retransmits");
    health.describe(
        "Packets dropped at the network layer and TCP-level retransmissions — key health signals.",
    );
    health.plot_promql(
        PlotOpts::counter("Packet Drops", "packet-drops", Unit::Rate),
        "sum(irate(network_drop[5m]))".to_string(),
    );
    health.plot_promql(
        PlotOpts::counter("TCP Retransmits", "tcp-retransmits", Unit::Rate),
        "sum(irate(tcp_retransmit[5m]))".to_string(),
    );

    view.group(errors);

    let mut tcp = Group::new("TCP", "tcp");

    let latency = tcp.subgroup("Packet Latency");
    latency.describe("Time from packet received to being processed by the application.");
    latency.histogram_rate_mean(
        "TCP Packet",
        "tcp-packet-latency",
        "tcp_packet_latency",
        RateSource::FromHistogram,
        Unit::Time,
    );
    latency.plot_promql_full(
        PlotOpts::histogram_latency("TCP Packet Latency", "tcp-packet-latency")
            .with_axis_label("Latency")
            .with_unit_system("time"),
        "tcp_packet_latency".to_string(),
    );

    view.group(tcp);

    view
}

#[cfg(test)]
mod tests {
    use super::*;
    use metriken_query::MemoryStore;

    #[test]
    fn tcp_packet_latency_gets_from_histogram_rate_mean() {
        let view = generate(&MemoryStore::builder().build(), vec![]);
        let json = serde_json::to_string(&view).unwrap().replace("\\\"", "\"");
        assert!(json.contains("sum(histogram_irate(tcp_packet_latency))"));
        assert!(json.contains("histogram_mean(tcp_packet_latency)\""));
        // percentile plot still present
        assert!(json.contains("\"tcp_packet_latency\""));
    }
}
