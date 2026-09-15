use crate::MetricsSource;
use crate::plot::*;

pub fn generate(data: &dyn MetricsSource, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);

    let mut traffic = Group::new("Traffic", "traffic");

    // Host boundary FIRST: this is the number that matches the NIC's own
    // counters, and it is what somebody opening a "network traffic" view is
    // almost always after. The per-interface-traversal pair below stays, with
    // its own description saying why it can read higher.
    if has_host_traffic_counters(data) {
        let host = traffic.subgroup("Host Bandwidth");
        host.describe(
            "Bits per second crossing the host boundary, counted once at the interface bound to \
             a device driver — the wire on bare metal, the hypervisor boundary in a VM guest. \
             Matches the NIC's own counters. Excludes intra-host traffic between virtual \
             interfaces (container east-west, loopback).",
        );
        host.plot_promql(
            PlotOpts::counter(
                "Host Bandwidth Transmit",
                "host-bandwidth-tx",
                Unit::Bitrate,
            )
            .with_unit_system("bitrate"),
            "sum(irate(network_host_bytes{direction=\"transmit\"}[5m])) * 8".to_string(),
        );
        host.plot_promql(
            PlotOpts::counter("Host Bandwidth Receive", "host-bandwidth-rx", Unit::Bitrate)
                .with_unit_system("bitrate"),
            "sum(irate(network_host_bytes{direction=\"receive\"}[5m])) * 8".to_string(),
        );

        let host_packets = traffic.subgroup("Host Packets");
        host_packets.describe(
            "Packet rate crossing the host boundary, counted once per packet rather than once \
             per interface it traverses.",
        );
        host_packets.plot_promql(
            PlotOpts::counter("Host Packets Transmit", "host-packets-tx", Unit::Rate),
            "sum(irate(network_host_packets{direction=\"transmit\"}[5m]))".to_string(),
        );
        host_packets.plot_promql(
            PlotOpts::counter("Host Packets Receive", "host-packets-rx", Unit::Rate),
            "sum(irate(network_host_packets{direction=\"receive\"}[5m]))".to_string(),
        );
    }

    let bandwidth = traffic.subgroup("Interface Bandwidth");
    bandwidth.describe(
        "Bits per second summed over every network interface the traffic traverses. A packet \
         crossing a stacked interface (bond, VLAN, bridge, tunnel) is counted once per layer, so \
         this reads higher than the host bandwidth above — roughly 2x on a bonded host, 3x for \
         VLAN-over-bond. It also includes loopback and intra-host virtual traffic that never \
         reaches a NIC, which on a container or VM host can dominate.",
    );
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

    let packets = traffic.subgroup("Interface Packets");
    packets.describe(
        "Packet rate summed over every network interface the traffic traverses — see the \
         interface bandwidth note above.",
    );
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

    /// Build a store whose counter set is exactly `metrics`.
    fn store_with(metrics: &[&str]) -> MemoryStore {
        use std::collections::HashMap;
        use std::time::{Duration, SystemTime};
        let store = MemoryStore::builder().sampling_interval_ms(1000).build();
        let counters = metrics
            .iter()
            .map(|name| metriken_exposition::Counter::new(name.to_string(), 1, HashMap::new()))
            .collect();
        store.ingest_snapshot(metriken_exposition::Snapshot::V2(
            metriken_exposition::SnapshotV2 {
                systemtime: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
                duration: Duration::from_secs(0),
                metadata: HashMap::new(),
                counters,
                gauges: vec![],
                histograms: vec![],
            },
        ));
        store
    }

    fn json_of(view: &View) -> String {
        serde_json::to_string(view).unwrap().replace("\\\"", "\"")
    }

    /// A recording made before the host-boundary counters existed must not get
    /// blank panels at the top of its network view.
    #[test]
    fn host_panels_are_absent_when_the_recording_predates_them() {
        let json = json_of(&generate(&store_with(&["network_bytes"]), vec![]));
        assert!(!json.contains("network_host_bytes"), "{json}");
        // the per-interface pair is still plotted, so the view is not empty
        assert!(json.contains("network_bytes{direction=\"transmit\"}"));
    }

    /// And when the recording HAS them, they are plotted, and plotted FIRST —
    /// the host-boundary number is the one that matches the NIC, so it is what
    /// someone opening a traffic view should read before the inflated pair.
    #[test]
    fn host_panels_are_present_and_come_before_the_interface_panels() {
        let json = json_of(&generate(
            &store_with(&[
                "network_bytes",
                "network_host_bytes",
                "network_host_packets",
            ]),
            vec![],
        ));
        let host = json
            .find("network_host_bytes{direction=\"transmit\"}")
            .expect("host bandwidth panel missing");
        let iface = json
            .find("network_bytes{direction=\"transmit\"}")
            .expect("interface bandwidth panel missing");
        assert!(
            host < iface,
            "host panels must precede the per-interface panels (host@{host}, iface@{iface})"
        );
    }

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
