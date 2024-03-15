use crate::*;
use metriken::metric;
use metriken::Format;
use metriken::Gauge;
use metriken::LazyGauge;
use metriken::MetricEntry;

counter_with_histogram!(
    TCP_RX_BYTES,
    TCP_RX_BYTES_HISTOGRAM,
    "tcp/receive/bytes",
    "number of bytes received over TCP"
);
counter_with_histogram!(
    TCP_RX_READ,
    TCP_RX_READ_HISTOGRAM,
    "tcp/receive/read",
    "number of reads from the TCP socket buffers after reassembly"
);
counter_with_histogram!(
    TCP_RX_SEGMENTS,
    TCP_RX_SEGMENTS_HISTOGRAM,
    "tcp/receive/segments",
    "number of TCP segments received"
);

counter_with_histogram!(
    TCP_TX_BYTES,
    TCP_TX_BYTES_HISTOGRAM,
    "tcp/transmit/bytes",
    "number of bytes transmitted over TCP"
);
counter_with_histogram!(
    TCP_TX_SEND,
    TCP_TX_SEND_HISTOGRAM,
    "tcp/transmit/send",
    "number of TCP sends before fragmentation"
);
counter_with_histogram!(
    TCP_TX_SEGMENTS,
    TCP_TX_SEGMENTS_HISTOGRAM,
    "tcp/transmit/segments",
    "number of TCP segments transmitted"
);
counter_with_histogram!(
    TCP_TX_RETRANSMIT,
    TCP_TX_RETRANSMIT_HISTOGRAM,
    "tcp/transmit/retransmit",
    "number of TCP segments retransmitted"
);

#[metric(
    name = "tcp/connection/state",
    description = "The current number of TCP connections in the ESTABLISHED state",
    formatter = conn_state_formatter,
    metadata = { state = "established" }
)]
pub static TCP_CONN_STATE_ESTABLISHED: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "tcp/connection/state",
    description = "The current number of TCP connections in the SYN_SENT state",
    formatter = conn_state_formatter,
    metadata = { state = "syn_sent" }
)]
pub static TCP_CONN_STATE_SYN_SENT: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "tcp/connection/state",
    description = "The current number of TCP connections in the SYN_RECV state",
    formatter = conn_state_formatter,
    metadata = { state = "syn_recv" }
)]
pub static TCP_CONN_STATE_SYN_RECV: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "tcp/connection/state",
    description = "The current number of TCP connections in the FIN_WAIT1 state",
    formatter = conn_state_formatter,
    metadata = { state = "fin_wait1" }
)]
pub static TCP_CONN_STATE_FIN_WAIT1: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "tcp/connection/state",
    description = "The current number of TCP connections in the FIN_WAIT2 state",
    formatter = conn_state_formatter,
    metadata = { state = "fin_wait2" }
)]
pub static TCP_CONN_STATE_FIN_WAIT2: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "tcp/connection/state",
    description = "The current number of TCP connections in the TIME_WAIT state",
    formatter = conn_state_formatter,
    metadata = { state = "time_wait" }
)]
pub static TCP_CONN_STATE_TIME_WAIT: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "tcp/connection/state",
    description = "The current number of TCP connections in the CLOSE state",
    formatter = conn_state_formatter,
    metadata = { state = "close" }
)]
pub static TCP_CONN_STATE_CLOSE: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "tcp/connection/state",
    description = "The current number of TCP connections in the CLOSE_WAIT state",
    formatter = conn_state_formatter,
    metadata = { state = "close_wait" }
)]
pub static TCP_CONN_STATE_CLOSE_WAIT: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "tcp/connection/state",
    description = "The current number of TCP connections in the LAST_ACK state",
    formatter = conn_state_formatter,
    metadata = { state = "last_ack" }
)]
pub static TCP_CONN_STATE_LAST_ACK: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "tcp/connection/state",
    description = "The current number of TCP connections in the LISTEN state",
    formatter = conn_state_formatter,
    metadata = { state = "listen" }
)]
pub static TCP_CONN_STATE_LISTEN: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "tcp/connection/state",
    description = "The current number of TCP connections in the CLOSING state",
    formatter = conn_state_formatter,
    metadata = { state = "closing" }
)]
pub static TCP_CONN_STATE_CLOSING: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "tcp/connection/state",
    description = "The current number of TCP connections in the NEW_SYN_RECV state",
    formatter = conn_state_formatter,
    metadata = { state = "new_syn_recv" }
)]
pub static TCP_CONN_STATE_NEW_SYN_RECV: LazyGauge = LazyGauge::new(Gauge::default);

bpfhistogram!(
    TCP_RX_SIZE,
    "tcp/receive/size",
    "distribution of logical receive sizes after reassembly"
);
bpfhistogram!(
    TCP_TX_SIZE,
    "tcp/transmit/size",
    "distribution of logical send sizes before fragmentation"
);
bpfhistogram!(TCP_JITTER, "tcp/jitter");
bpfhistogram!(TCP_SRTT, "tcp/srtt");

bpfhistogram!(TCP_PACKET_LATENCY, "tcp/packet_latency");

/// A function to format the tcp connection state metrics.
///
/// For the `Simple` format, the metrics will be formatted according to the
/// a pattern which depends on the metric metadata:
/// `{name}/{state}` eg: `cpu/connection/state/listen`
///
/// For the `Prometheus` format, the state is supplied as a label. Note: we rely
/// on the exposition logic to convert the `/`s to `_`s in the metric name.
pub fn conn_state_formatter(metric: &MetricEntry, format: Format) -> String {
    match format {
        Format::Simple => {
            if let Some(state) = metric.metadata().get("state") {
                format!("{}/{state}", metric.name())
            } else {
                metric.name().to_string()
            }
        }
        Format::Prometheus => {
            let metadata: Vec<String> = metric
                .metadata()
                .iter()
                .map(|(key, value)| format!("{key}=\"{value}\""))
                .collect();
            let metadata = metadata.join(", ");

            if metadata.is_empty() {
                metric.name().to_string()
            } else {
                format!("{}{{{metadata}}}", metric.name())
            }
        }
        _ => metriken::default_formatter(metric, format),
    }
}
