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

    tcp_mib(data, &mut tcp);

    view.group(tcp);

    view
}

/// One chart of a `tcp_mib` counter or gauge.
struct MibChart {
    title: &'static str,
    id: &'static str,
    metric: &'static str,
    /// The label the chart is split by, for a metric with one.
    by: Option<&'static str>,
    gauge: bool,
}

const fn counter(title: &'static str, id: &'static str, metric: &'static str) -> MibChart {
    MibChart {
        title,
        id,
        metric,
        by: None,
        gauge: false,
    }
}

const fn counter_by(
    title: &'static str,
    id: &'static str,
    metric: &'static str,
    by: &'static str,
) -> MibChart {
    MibChart {
        title,
        id,
        metric,
        by: Some(by),
        gauge: false,
    }
}

/// The `tcp_mib` subgroups: name, description, charts.
const TCP_MIB: &[(&str, &str, &[MibChart])] = &[
    (
        "Retransmission & Loss Recovery",
        "Retransmission timeouts stall a connection for at least the minimum RTO (200 ms by \
         default); tail loss probes and fast recovery repair loss without waiting for one. \
         D-SACKs received are retransmissions the peer did not need.",
        &[
            counter(
                "RTO Expirations",
                "tcp-rto-expirations",
                "tcp_rto_expirations",
            ),
            counter(
                "Segments Retransmitted",
                "tcp-segments-retransmitted",
                "tcp_segments_retransmitted",
            ),
            counter_by(
                "Lost Segments Retransmitted",
                "tcp-loss-retransmits",
                "tcp_loss_retransmits",
                "kind",
            ),
            counter("Tail Loss Probes", "tcp-loss-probes", "tcp_loss_probes"),
            counter(
                "Loss Probe Recoveries",
                "tcp-loss-probe-recoveries",
                "tcp_loss_probe_recoveries",
            ),
            counter_by(
                "Fast Recoveries",
                "tcp-fast-recoveries",
                "tcp_fast_recoveries",
                "kind",
            ),
            counter("Spurious RTOs", "tcp-spurious-rtos", "tcp_spurious_rtos"),
            counter(
                "Retransmits Lost Again",
                "tcp-retransmits-lost",
                "tcp_retransmits_lost",
            ),
            counter_by(
                "D-SACKs Received",
                "tcp-dsacks-received",
                "tcp_dsacks_received",
                "kind",
            ),
            counter_by("D-SACKs Sent", "tcp-dsacks-sent", "tcp_dsacks_sent", "kind"),
        ],
    ),
    (
        "Delayed ACKs & Autocorking",
        "A delayed-ACK firing means this host held an ACK for the delayed-ACK timeout (40 ms \
         minimum) because nothing outgoing carried it; a peer sending with Nagle, or limited by \
         its congestion or receive window, may be waiting for it. Duplicate data segments are \
         the peer retransmitting data already received; on each one the kernel switches the \
         connection to immediate ACKs. \
         Autocorking holds back small sends while earlier data is still queued in the qdisc \
         or device.",
        &[
            counter("Delayed ACKs", "tcp-delayed-acks", "tcp_delayed_acks"),
            counter(
                "Delayed ACKs Deferred",
                "tcp-delayed-ack-deferrals",
                "tcp_delayed_ack_deferrals",
            ),
            counter(
                "Duplicate Data Segments",
                "tcp-duplicate-data-segments",
                "tcp_duplicate_data_segments",
            ),
            counter("Autocorks", "tcp-autocorks", "tcp_autocorks"),
        ],
    ),
    (
        "Receive Window",
        "A receiver advertises a zero window when the application is not reading fast enough \
         to keep its receive buffer from filling; the sender then stops until the window \
         reopens.",
        &[
            counter_by(
                "Zero Window Transitions",
                "tcp-zero-window-transitions",
                "tcp_zero_window_transitions",
                "kind",
            ),
            counter(
                "Zero Window Withheld",
                "tcp-zero-window-withheld",
                "tcp_zero_window_withheld",
            ),
            counter(
                "Zero Window Drops",
                "tcp-zero-window-drops",
                "tcp_zero_window_drops",
            ),
        ],
    ),
    (
        "Receive Memory",
        "Receive-side memory work and drops. Pruning and collapsing run when a socket cannot \
         be charged memory for an incoming segment. A prune failure is usually followed by a \
         receive-buffer drop, which discards a segment the peer has to retransmit, so the two \
         overlap; a prune failure on an empty receive queue queues the segment anyway.",
        &[
            counter("Prune Attempts", "tcp-prune-attempts", "tcp_prune_attempts"),
            counter("Prune Failures", "tcp-prune-failures", "tcp_prune_failures"),
            counter(
                "Out-of-Order Prunes",
                "tcp-out-of-order-prunes",
                "tcp_out_of_order_prunes",
            ),
            counter(
                "Collapsed Buffers",
                "tcp-collapsed-buffers",
                "tcp_collapsed_buffers",
            ),
            counter_by(
                "Receive Buffer Drops",
                "tcp-rcvbuf-drops",
                "tcp_rcvbuf_drops",
                "kind",
            ),
            counter("Backlog Drops", "tcp-backlog-drops", "tcp_backlog_drops"),
            counter(
                "Out-of-Order Queued",
                "tcp-out-of-order-queued",
                "tcp_out_of_order_queued",
            ),
            counter(
                "Memory Pressure Entries",
                "tcp-memory-pressure-entries",
                "tcp_memory_pressure_entries",
            ),
        ],
    ),
    (
        "Connections",
        "Connection state changes, failures and errors in the agent's network namespace.",
        &[
            MibChart {
                title: "Established Connections",
                id: "tcp-established-connections",
                metric: "tcp_established_connections",
                by: None,
                gauge: true,
            },
            counter("Open Failures", "tcp-open-failures", "tcp_open_failures"),
            counter(
                "Established Resets",
                "tcp-established-resets",
                "tcp_established_resets",
            ),
            counter("Resets Sent", "tcp-resets-sent", "tcp_resets_sent"),
            counter_by("Aborts", "tcp-aborts", "tcp_aborts", "reason"),
            counter(
                "Listen Overflows",
                "tcp-listen-overflows",
                "tcp_listen_overflows",
            ),
            counter("Listen Drops", "tcp-listen-drops", "tcp_listen_drops"),
            counter("Input Errors", "tcp-input-errors", "tcp_input_errors"),
            counter(
                "Checksum Errors",
                "tcp-checksum-errors",
                "tcp_checksum_errors",
            ),
        ],
    ),
];

/// The `tcp_mib` subgroups, each chart only when the recording carries its
/// metric and each subgroup only when it has a chart: a recording made
/// without the sampler, or on a kernel that lacks a column, gets no empty
/// panels.
fn tcp_mib(data: &dyn MetricsSource, tcp: &mut Group) {
    for (name, description, charts) in TCP_MIB {
        let present: Vec<&MibChart> = charts
            .iter()
            .filter(|c| has_metric(data, c.metric))
            .collect();
        if present.is_empty() {
            continue;
        }
        let subgroup = tcp.subgroup(*name);
        subgroup.describe(*description);
        for c in present {
            if c.gauge {
                subgroup.plot_promql(
                    PlotOpts::gauge(c.title, c.id, Unit::Count),
                    format!("sum({})", c.metric),
                );
            } else {
                let query = match c.by {
                    Some(label) => format!("sum by ({label}) (irate({}[5m]))", c.metric),
                    None => format!("sum(irate({}[5m]))", c.metric),
                };
                subgroup.plot_promql(PlotOpts::counter(c.title, c.id, Unit::Rate), query);
            }
        }
    }
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

    /// A recording without the `tcp_mib` sampler gets none of its panels.
    #[test]
    fn tcp_mib_panels_are_absent_without_the_sampler() {
        let json = json_of(&generate(&store_with(&["network_bytes"]), vec![]));
        for (name, _, charts) in TCP_MIB {
            assert!(!json.contains(&format!("\"{name}\"")), "{name} in {json}");
            for c in *charts {
                assert!(!json.contains(c.metric), "{} in {json}", c.metric);
            }
        }
    }

    /// Each `tcp_mib` chart is plotted when its metric is present, a labeled
    /// metric is split by its label, and a metric the recording lacks is not
    /// plotted even when others in its subgroup are.
    #[test]
    fn tcp_mib_panels_follow_the_metrics_present() {
        let json = json_of(&generate(
            &store_with(&[
                "tcp_rto_expirations",
                "tcp_fast_recoveries",
                "tcp_delayed_acks",
            ]),
            vec![],
        ));
        assert!(
            json.contains("sum(irate(tcp_rto_expirations[5m]))"),
            "{json}"
        );
        assert!(
            json.contains("sum by (kind) (irate(tcp_fast_recoveries[5m]))"),
            "{json}"
        );
        assert!(json.contains("sum(irate(tcp_delayed_acks[5m]))"), "{json}");
        assert!(
            json.contains("\"Retransmission & Loss Recovery\""),
            "{json}"
        );
        assert!(!json.contains("tcp_loss_probes"), "{json}");
        assert!(!json.contains("\"Receive Window\""), "{json}");
        assert!(!json.contains("\"Connections\""), "{json}");
    }

    /// Every chart id in the `tcp_mib` table is unique, and every metric is
    /// charted once.
    #[test]
    fn tcp_mib_chart_ids_and_metrics_are_unique() {
        let charts: Vec<&MibChart> = TCP_MIB.iter().flat_map(|(_, _, c)| c.iter()).collect();
        let mut ids: Vec<&str> = charts.iter().map(|c| c.id).collect();
        let mut metrics: Vec<&str> = charts.iter().map(|c| c.metric).collect();
        ids.sort_unstable();
        ids.dedup();
        metrics.sort_unstable();
        metrics.dedup();
        assert_eq!(ids.len(), charts.len());
        assert_eq!(metrics.len(), charts.len());
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
