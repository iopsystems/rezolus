use crate::common::HISTOGRAM_GROUPING_POWER;
use metriken::{
    metric, AtomicHistogram, Counter, Format, Gauge, LazyCounter, LazyGauge, MetricEntry,
    RwLockHistogram,
};

#[metric(
    name = "tcp/receive/bytes",
    description = "The number of bytes received over TCP",
    metadata = { unit = "bytes" }
)]
pub static TCP_RX_BYTES: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "tcp/receive/bytes",
    description = "Distribution of the rate of bytes received over TCP from sample to sample",
    metadata = { unit = "bytes/second" }
)]
pub static TCP_RX_BYTES_HISTOGRAM: AtomicHistogram =
    AtomicHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "tcp/receive/packets",
    description = "The number of packets received over TCP",
    metadata = { unit = "packets" }
)]
pub static TCP_RX_PACKETS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "tcp/receive/packets",
    description = "Distribution of the rate of packets received over TCP from sample to sample",
    metadata = { unit = "packets/second" }
)]
pub static TCP_RX_PACKETS_HISTOGRAM: AtomicHistogram =
    AtomicHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "tcp/receive/read",
    description = "The number of reads from the TCP socket buffers after reassembly",
    metadata = { unit = "operations" }
)]
pub static TCP_RX_READ: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "tcp/receive/read",
    description = "Distribution of the rate of reads from the TCP socket buffers after reassembly from sample to sample",
    metadata = { unit = "operations/second" }
)]
pub static TCP_RX_READ_HISTOGRAM: AtomicHistogram =
    AtomicHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "tcp/transmit/bytes",
    description = "The number of bytes transmitted over TCP",
    metadata = { unit = "bytes" }
)]
pub static TCP_TX_BYTES: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "tcp/transmit/bytes",
    description = "Distribution of the rate of bytes transmitted over TCP from sample to sample",
    metadata = { unit = "bytes/second" }
)]
pub static TCP_TX_BYTES_HISTOGRAM: AtomicHistogram =
    AtomicHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "tcp/transmit/packets",
    description = "The number of packets transmitted over TCP",
    metadata = { unit = "packets" }
)]
pub static TCP_TX_PACKETS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "tcp/transmit/packets",
    description = "Distribution of the rate of packets transmitted over TCP from sample to sample",
    metadata = { unit = "packets/second" }
)]
pub static TCP_TX_PACKETS_HISTOGRAM: AtomicHistogram =
    AtomicHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "tcp/transmit/send",
    description = "The number of TCP sends before fragmentation",
    metadata = { unit = "operations" }
)]
pub static TCP_TX_SEND: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "tcp/transmit/send",
    description = "Distribution of the rate of TCP sends before fragmentation from sample to sample",
    metadata = { unit = "operations/second" }
)]
pub static TCP_TX_SEND_HISTOGRAM: AtomicHistogram =
    AtomicHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "tcp/transmit/retransmit",
    description = "The number of TCP packets that were re-transmitted",
    metadata = { unit = "packets" }
)]
pub static TCP_TX_RETRANSMIT: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "tcp/transmit/retransmit",
    description = "Distribution of the rate of TCP packets re-transmitted from sample to sample",
    metadata = { unit = "packets/second" }
)]
pub static TCP_TX_RETRANSMIT_HISTOGRAM: AtomicHistogram =
    AtomicHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

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

#[metric(
    name = "tcp/receive/size",
    description = "Distribution of the size of TCP packets received after reassembly",
    metadata = { unit = "bytes" }
)]
pub static TCP_RX_SIZE: RwLockHistogram = RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "tcp/transmit/size",
    description = "Distribution of the size of TCP packets transmitted before fragmentation",
    metadata = { unit = "bytes" }
)]
pub static TCP_TX_SIZE: RwLockHistogram = RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "tcp/jitter",
    description = "Distribution of TCP latency jitter",
    metadata = { unit = "nanoseconds" }
)]
pub static TCP_JITTER: RwLockHistogram = RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "tcp/srtt",
    description = "Distribution of TCP smoothed round-trip time",
    metadata = { unit = "nanoseconds" }
)]
pub static TCP_SRTT: RwLockHistogram = RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "tcp/packet_latency",
    description = "Distribution of latency from a socket becoming readable until a userspace read",
    metadata = { unit = "nanoseconds" }
)]
pub static TCP_PACKET_LATENCY: RwLockHistogram = RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

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
