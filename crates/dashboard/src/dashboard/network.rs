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
            Plot::counter(
                "Host Bandwidth Transmit",
                "host-bandwidth-tx",
                Unit::Bitrate,
            )
            .with_unit_system(Unit::Bitrate),
            "sum(irate(network_host_bytes{direction=\"transmit\"}[5m])) * 8".to_string(),
        );
        host.plot_promql(
            Plot::counter("Host Bandwidth Receive", "host-bandwidth-rx", Unit::Bitrate)
                .with_unit_system(Unit::Bitrate),
            "sum(irate(network_host_bytes{direction=\"receive\"}[5m])) * 8".to_string(),
        );

        let host_packets = traffic.subgroup("Host Packets");
        host_packets.describe(
            "Packet rate crossing the host boundary, counted once per packet rather than once \
             per interface it traverses.",
        );
        host_packets.plot_promql(
            Plot::counter("Host Packets Transmit", "host-packets-tx", Unit::Rate),
            "sum(irate(network_host_packets{direction=\"transmit\"}[5m]))".to_string(),
        );
        host_packets.plot_promql(
            Plot::counter("Host Packets Receive", "host-packets-rx", Unit::Rate),
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
        Plot::counter("Bandwidth Transmit", "bandwidth-tx", Unit::Bitrate)
            .with_unit_system(Unit::Bitrate),
        "sum(irate(network_bytes{direction=\"transmit\"}[5m])) * 8".to_string(),
    );
    bandwidth.plot_promql(
        Plot::counter("Bandwidth Receive", "bandwidth-rx", Unit::Bitrate)
            .with_unit_system(Unit::Bitrate),
        "sum(irate(network_bytes{direction=\"receive\"}[5m])) * 8".to_string(),
    );

    let packets = traffic.subgroup("Interface Packets");
    packets.describe(
        "Packet rate summed over every network interface the traffic traverses — see the \
         interface bandwidth note above.",
    );
    packets.plot_promql(
        Plot::counter("Packets Transmit", "packets-tx", Unit::Rate),
        "sum(irate(network_packets{direction=\"transmit\"}[5m]))".to_string(),
    );
    packets.plot_promql(
        Plot::counter("Packets Receive", "packets-rx", Unit::Rate),
        "sum(irate(network_packets{direction=\"receive\"}[5m]))".to_string(),
    );

    view.group(traffic);

    // Each chart is drawn only when the recording has its metric: a macOS
    // agent records no network_drop or tcp_packet_latency, and records
    // tcp_retransmit only when it can read net.inet.tcp.stats (as root, by
    // default).
    let drops = has_metric(data, "network_drop");
    let retransmits = has_metric(data, "tcp_retransmit");
    let interface_errors = has_metric(data, "network_interface_errors");
    let interface_drops = has_metric(data, "network_interface_drops");
    if drops || retransmits || interface_errors || interface_drops {
        let mut errors = Group::new("Errors", "errors");

        if drops || retransmits {
            let health = errors.subgroup("Drops & Retransmits");
            health.describe(
                "Packets dropped at the network layer and TCP-level retransmissions — key health signals.",
            );
            if drops {
                health.plot_promql(
                    Plot::counter("Packet Drops", "packet-drops", Unit::Rate),
                    "sum(irate(network_drop[5m]))".to_string(),
                );
            }
            if retransmits {
                health.plot_promql(
                    Plot::counter("TCP Retransmits", "tcp-retransmits", Unit::Rate),
                    "sum(irate(tcp_retransmit[5m]))".to_string(),
                );
            }
        }

        if interface_errors || interface_drops {
            let interface = errors.subgroup("Interface Errors & Drops");
            interface.describe(
                "Errors and queue drops the network interfaces report, summed over every \
                 interface, loopback included, except the software interfaces under \
                 IOResources (the vmenet interfaces of virtual machines). macOS only.",
            );
            if interface_errors {
                interface.plot_promql(
                    Plot::counter(
                        "Interface Errors Receive",
                        "interface-errors-rx",
                        Unit::Rate,
                    ),
                    "sum(irate(network_interface_errors{direction=\"receive\"}[5m]))".to_string(),
                );
                interface.plot_promql(
                    Plot::counter(
                        "Interface Errors Transmit",
                        "interface-errors-tx",
                        Unit::Rate,
                    ),
                    "sum(irate(network_interface_errors{direction=\"transmit\"}[5m]))".to_string(),
                );
            }
            if interface_drops {
                interface.plot_promql(
                    Plot::counter("Interface Drops Receive", "interface-drops-rx", Unit::Rate),
                    "sum(irate(network_interface_drops{direction=\"receive\"}[5m]))".to_string(),
                );
                interface.plot_promql(
                    Plot::counter("Interface Drops Transmit", "interface-drops-tx", Unit::Rate),
                    "sum(irate(network_interface_drops{direction=\"transmit\"}[5m]))".to_string(),
                );
            }
        }

        view.group(errors);
    }

    let mut tcp = Group::new("TCP", "tcp");

    if has_metric(data, "tcp_bytes") {
        let traffic = tcp.subgroup("Traffic");
        traffic.describe(
            "TCP payload bits per second, without headers. On Linux, transmit is the bytes passed \
             to send calls, including sends that failed or were retried, and receive is the bytes \
             read from sockets. On macOS, both are data bytes in segments sent and received, \
             without retransmissions sent; while a connection loses or reorders packets, receive \
             leaves out some segments and counts overlapping out-of-order bytes twice.",
        );
        traffic.plot_promql(
            Plot::counter("TCP Bandwidth Transmit", "tcp-bandwidth-tx", Unit::Bitrate)
                .with_unit_system(Unit::Bitrate),
            "sum(irate(tcp_bytes{direction=\"transmit\"}[5m])) * 8".to_string(),
        );
        traffic.plot_promql(
            Plot::counter("TCP Bandwidth Receive", "tcp-bandwidth-rx", Unit::Bitrate)
                .with_unit_system(Unit::Bitrate),
            "sum(irate(tcp_bytes{direction=\"receive\"}[5m])) * 8".to_string(),
        );
    }

    if has_metric(data, "tcp_packet_latency") {
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
            Plot::histogram_latency("TCP Packet Latency", "tcp-packet-latency")
                .with_axis_label("Latency")
                .with_unit_system(Unit::Time),
            "tcp_packet_latency".to_string(),
        );
    }

    if has_metric(data, "tcp_connect_latency") {
        let connect = tcp.subgroup("Connect Latency");
        connect.describe(
            "Time from connect() to the end of the handshake, by outcome: established, refused \
             (a RST or ICMP port unreachable in reply to the SYN), or failed (the SYN retries \
             ran out, another ICMP error, no free local port, or the socket was closed before \
             an answer). Recordings from agents that do not label the outcome have one \
             distribution, which includes refused connects.",
        );
        if data
            .label_values("tcp_connect_latency", "outcome")
            .is_empty()
        {
            connect.plot_promql_full(
                Plot::histogram_latency("Connect Latency", "tcp-connect-latency")
                    .with_axis_label("Latency")
                    .with_unit_system(Unit::Time),
                "tcp_connect_latency".to_string(),
            );
        } else {
            connect.plot_promql(
                Plot::counter("Connects", "tcp-connects", Unit::Rate),
                "sum by (outcome) (histogram_irate(tcp_connect_latency))".to_string(),
            );
            for (title, id, matcher) in [
                (
                    "Established Connect Latency",
                    "tcp-connect-latency-established",
                    "outcome=\"established\"",
                ),
                (
                    "Refused Connect Latency",
                    "tcp-connect-latency-refused",
                    "outcome=\"refused\"",
                ),
                (
                    "Failed Connect Latency",
                    "tcp-connect-latency-failed",
                    "outcome=~\"timeout|error|aborted\"",
                ),
            ] {
                connect.plot_promql(
                    Plot::histogram_latency(title, id)
                        .with_axis_label("Latency")
                        .with_unit_system(Unit::Time),
                    format!("tcp_connect_latency{{{matcher}}}"),
                );
            }
        }
    }

    tcp_mib(data, &mut tcp);

    if !tcp.is_empty() {
        view.group(tcp);
    }

    let mut efa = Group::new("EFA", "efa");
    efa_charts(data, &mut efa);
    if !efa.is_empty() {
        view.group(efa);
    }

    view
}

/// What an EFA chart plots: bits per second of a byte counter, or the rate
/// of a counter.
#[derive(Clone, Copy)]
enum EfaKind {
    Bits,
    Rate,
}

/// One EFA chart: title, id, metric and kind.
type EfaChart = (&'static str, &'static str, &'static str, EfaKind);

/// One EFA subgroup: name, description and charts.
type EfaSubgroup = (&'static str, &'static str, &'static [EfaChart]);

/// The subgroup of the EFA device counters, which are per device and so
/// plotted `sum by (device)`; every other EFA chart is per port.
const EFA_DEVICE_SUBGROUP: &str = "EFA Device Errors";

/// The `network_efa` subgroups: name, description, and each chart's title,
/// id, metric and kind. Every chart is one line per EFA port,
/// `sum by (device, port)`, or per device in [`EFA_DEVICE_SUBGROUP`].
const EFA: &[EfaSubgroup] = &[
    (
        "EFA Traffic",
        "Bits and packets per second through each EFA port, counted by the device. \
         Retransmissions are shown in EFA Retransmits & Errors.",
        &[
            (
                "EFA Transmit",
                "efa-tx",
                "network_efa_tx_bytes",
                EfaKind::Bits,
            ),
            (
                "EFA Receive",
                "efa-rx",
                "network_efa_rx_bytes",
                EfaKind::Bits,
            ),
            (
                "EFA Packets Transmit",
                "efa-tx-packets",
                "network_efa_tx_pkts",
                EfaKind::Rate,
            ),
            (
                "EFA Packets Receive",
                "efa-rx-packets",
                "network_efa_rx_pkts",
                EfaKind::Rate,
            ),
        ],
    ),
    (
        "EFA RDMA",
        "One-sided RDMA on each EFA port. Read and Write are the operations this host issued; \
         Read Responses and Write Received are the bytes moved by operations remote hosts \
         issued against this host's memory.",
        &[
            (
                "RDMA Read",
                "efa-rdma-read",
                "network_efa_rdma_read_bytes",
                EfaKind::Bits,
            ),
            (
                "RDMA Write",
                "efa-rdma-write",
                "network_efa_rdma_write_bytes",
                EfaKind::Bits,
            ),
            (
                "RDMA Read Responses",
                "efa-rdma-read-resp",
                "network_efa_rdma_read_resp_bytes",
                EfaKind::Bits,
            ),
            (
                "RDMA Write Received",
                "efa-rdma-write-recv",
                "network_efa_rdma_write_recv_bytes",
                EfaKind::Bits,
            ),
            (
                "RDMA Reads",
                "efa-rdma-read-wrs",
                "network_efa_rdma_read_wrs",
                EfaKind::Rate,
            ),
            (
                "RDMA Writes",
                "efa-rdma-write-wrs",
                "network_efa_rdma_write_wrs",
                EfaKind::Rate,
            ),
        ],
    ),
    (
        "EFA Messages",
        "Two-sided send and receive work requests on each EFA port, and the bits they carried.",
        &[
            ("Send", "efa-send", "network_efa_send_bytes", EfaKind::Bits),
            (
                "Receive",
                "efa-recv",
                "network_efa_recv_bytes",
                EfaKind::Bits,
            ),
            (
                "Sends",
                "efa-send-wrs",
                "network_efa_send_wrs",
                EfaKind::Rate,
            ),
            (
                "Receives",
                "efa-recv-wrs",
                "network_efa_recv_wrs",
                EfaKind::Rate,
            ),
        ],
    ),
    (
        "EFA Retransmits & Errors",
        "Retransmissions, receive drops, remote hosts that did not respond or whose connection \
         the device counted as impaired, and RDMA work requests that completed with an error.",
        &[
            (
                "Retransmitted",
                "efa-retrans",
                "network_efa_retrans_bytes",
                EfaKind::Bits,
            ),
            (
                "Retransmitted Packets",
                "efa-retrans-packets",
                "network_efa_retrans_pkts",
                EfaKind::Rate,
            ),
            (
                "Retransmit Timeouts",
                "efa-retrans-timeouts",
                "network_efa_retrans_timeout_events",
                EfaKind::Rate,
            ),
            (
                "Receive Drops",
                "efa-rx-drops",
                "network_efa_rx_drops",
                EfaKind::Rate,
            ),
            (
                "Unresponsive Remote",
                "efa-unresponsive-remote",
                "network_efa_unresponsive_remote_events",
                EfaKind::Rate,
            ),
            (
                "Impaired Connections",
                "efa-impaired-remote",
                "network_efa_impaired_remote_conn_events",
                EfaKind::Rate,
            ),
            (
                "RDMA Read Errors",
                "efa-rdma-read-err",
                "network_efa_rdma_read_wr_err",
                EfaKind::Rate,
            ),
            (
                "RDMA Write Errors",
                "efa-rdma-write-err",
                "network_efa_rdma_write_wr_err",
                EfaKind::Rate,
            ),
        ],
    ),
    (
        EFA_DEVICE_SUBGROUP,
        "Failed admin commands and verbs setup calls (protection domain, queue pair, completion \
         queue, memory region, address handle, user context, mmap) on each EFA device.",
        &[
            (
                "Admin Command Errors",
                "efa-cmds-err",
                "network_efa_cmds_err",
                EfaKind::Rate,
            ),
            (
                "Admin Commands Without Completion",
                "efa-no-completion-cmds",
                "network_efa_no_completion_cmds",
                EfaKind::Rate,
            ),
            (
                "Protection Domain Errors",
                "efa-alloc-pd-err",
                "network_efa_alloc_pd_err",
                EfaKind::Rate,
            ),
            (
                "Memory Region Allocation Errors",
                "efa-alloc-mr-err",
                "network_efa_alloc_mr_err",
                EfaKind::Rate,
            ),
            (
                "Memory Region Registration Errors",
                "efa-reg-mr-err",
                "network_efa_reg_mr_err",
                EfaKind::Rate,
            ),
            (
                "DMA Memory Region Errors",
                "efa-get-dma-mr-err",
                "network_efa_get_dma_mr_err",
                EfaKind::Rate,
            ),
            (
                "User Context Errors",
                "efa-alloc-ucontext-err",
                "network_efa_alloc_ucontext_err",
                EfaKind::Rate,
            ),
            (
                "Queue Pair Errors",
                "efa-create-qp-err",
                "network_efa_create_qp_err",
                EfaKind::Rate,
            ),
            (
                "Completion Queue Errors",
                "efa-create-cq-err",
                "network_efa_create_cq_err",
                EfaKind::Rate,
            ),
            (
                "Address Handle Errors",
                "efa-create-ah-err",
                "network_efa_create_ah_err",
                EfaKind::Rate,
            ),
            (
                "Mapping Errors",
                "efa-mmap-err",
                "network_efa_mmap_err",
                EfaKind::Rate,
            ),
        ],
    ),
];

/// The `network_efa` subgroups, each chart only when the recording carries
/// its metric and each subgroup only when it has a chart.
fn efa_charts(data: &dyn MetricsSource, efa: &mut Group) {
    for (name, description, charts) in EFA {
        let present: Vec<_> = charts
            .iter()
            .filter(|(_, _, metric, _)| has_metric(data, metric))
            .collect();
        if present.is_empty() {
            continue;
        }
        let subgroup = efa.subgroup(*name);
        subgroup.describe(*description);
        for &&(title, id, metric, kind) in &present {
            let by = if *name == EFA_DEVICE_SUBGROUP {
                "device"
            } else {
                "device, port"
            };
            match kind {
                EfaKind::Bits => subgroup.plot_promql(
                    Plot::counter(title, id, Unit::Bitrate).with_unit_system(Unit::Bitrate),
                    format!("sum by ({by}) (irate({metric}[5m])) * 8"),
                ),
                EfaKind::Rate => subgroup.plot_promql(
                    Plot::counter(title, id, Unit::Rate),
                    format!("sum by ({by}) (irate({metric}[5m]))"),
                ),
            }
        }
    }
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
         usually the peer retransmitting data already received; on each one the kernel sends \
         its next ACKs immediately, up to 16. \
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
                    Plot::gauge(c.title, c.id, Unit::Count),
                    format!("sum({})", c.metric),
                );
            } else {
                let query = match c.by {
                    Some(label) => format!("sum by ({label}) (irate({}[5m]))", c.metric),
                    None => format!("sum(irate({}[5m]))", c.metric),
                };
                subgroup.plot_promql(Plot::counter(c.title, c.id, Unit::Rate), query);
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
        store_with_histograms(metrics, &[])
    }

    /// Build a store whose counter set is exactly `counters` and whose
    /// histogram set is exactly `histograms`.
    fn store_with_histograms(counters: &[&str], histograms: &[&str]) -> MemoryStore {
        use std::collections::HashMap;
        use std::time::{Duration, SystemTime};
        let store = MemoryStore::builder().sampling_interval_ms(1000).build();
        let counters = counters
            .iter()
            .map(|name| metriken_exposition::Counter::new(name.to_string(), 1, HashMap::new()))
            .collect();
        let histograms = histograms
            .iter()
            .map(|name| {
                metriken_exposition::Histogram::new(
                    name.to_string(),
                    histogram::Histogram::new(3, 64).unwrap(),
                    HashMap::new(),
                )
            })
            .collect();
        store.ingest_snapshot(metriken_exposition::Snapshot::V2(
            metriken_exposition::SnapshotV2 {
                systemtime: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
                duration: Duration::from_secs(0),
                metadata: HashMap::new(),
                counters,
                gauges: vec![],
                histograms,
                gauge_histograms: Vec::new(),
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
        let view = generate(&store_with_histograms(&[], &["tcp_packet_latency"]), vec![]);
        let json = json_of(&view);
        assert!(json.contains("sum(histogram_irate(tcp_packet_latency))"));
        assert!(json.contains("histogram_mean(tcp_packet_latency)\""));
        // percentile plot still present
        assert!(json.contains("\"tcp_packet_latency\""));
    }

    /// A recording without network_drop, tcp_retransmit or
    /// tcp_packet_latency, as from a macOS agent not running as root, gets
    /// neither the Errors group nor the Packet Latency charts, and no TCP
    /// group when no tcp_mib metric is present either.
    #[test]
    fn errors_and_latency_are_absent_without_their_metrics() {
        let json = json_of(&generate(
            &store_with(&["network_bytes", "network_packets"]),
            vec![],
        ));
        assert!(!json.contains("\"Errors\""), "{json}");
        assert!(!json.contains("network_drop"), "{json}");
        assert!(!json.contains("tcp_retransmit"), "{json}");
        assert!(!json.contains("tcp_packet_latency"), "{json}");
        assert!(!json.contains("\"TCP\""), "{json}");
        assert!(json.contains("network_bytes{direction=\"transmit\"}"));
    }

    /// The counters a macOS agent running as root records: TCP Retransmits
    /// without Packet Drops, and the tcp_mib charts without Packet Latency.
    #[test]
    fn retransmits_and_tcp_mib_are_drawn_without_drops_or_latency() {
        let json = json_of(&generate(
            &store_with(&[
                "network_bytes",
                "network_packets",
                "tcp_retransmit",
                "tcp_rto_expirations",
                "tcp_delayed_acks",
            ]),
            vec![],
        ));
        assert!(json.contains("\"Errors\""), "{json}");
        assert!(json.contains("sum(irate(tcp_retransmit[5m]))"), "{json}");
        assert!(!json.contains("network_drop"), "{json}");
        assert!(json.contains("\"TCP\""), "{json}");
        assert!(!json.contains("\"Packet Latency\""), "{json}");
        assert!(!json.contains("tcp_packet_latency"), "{json}");
        assert!(
            json.contains("sum(irate(tcp_rto_expirations[5m]))"),
            "{json}"
        );
    }

    /// TCP Bandwidth is drawn first in the TCP group when the recording has
    /// tcp_bytes, as bits per second, and not at all without it.
    #[test]
    fn tcp_bandwidth_follows_tcp_bytes() {
        let json = json_of(&generate(
            &store_with_histograms(
                &["tcp_bytes", "tcp_rto_expirations"],
                &["tcp_packet_latency"],
            ),
            vec![],
        ));
        let bandwidth = json
            .find("sum(irate(tcp_bytes{direction=\"transmit\"}[5m])) * 8")
            .expect("TCP bandwidth transmit missing");
        assert!(json.contains("sum(irate(tcp_bytes{direction=\"receive\"}[5m])) * 8"));
        let latency = json.find("tcp_packet_latency").unwrap();
        let rto = json.find("tcp_rto_expirations").unwrap();
        assert!(
            bandwidth < latency && bandwidth < rto,
            "TCP bandwidth comes before packet latency and the tcp_mib charts"
        );

        let json = json_of(&generate(&store_with(&["tcp_rto_expirations"]), vec![]));
        assert!(!json.contains("tcp_bytes"), "{json}");
    }

    /// Packet Drops is drawn on its own when the recording has no
    /// tcp_retransmit.
    #[test]
    fn drops_are_drawn_without_retransmits() {
        let json = json_of(&generate(&store_with(&["network_drop"]), vec![]));
        assert!(json.contains("sum(irate(network_drop[5m]))"), "{json}");
        assert!(!json.contains("tcp_retransmit"), "{json}");
    }
}
