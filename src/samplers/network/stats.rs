use crate::common::HISTOGRAM_GROUPING_POWER;
use metriken::{metric, AtomicHistogram, Counter, LazyCounter};

#[metric(
    name = "network/receive/bytes",
    description = "The number of bytes received over the network",
    metadata = { unit = "bytes" }
)]
pub static NETWORK_RX_BYTES: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "network/receive/bytes",
    description = "Distribution of network receive throughput from sample to sample",
    metadata = { unit = "bytes/second" }
)]
pub static NETWORK_RX_BYTES_HISTOGRAM: AtomicHistogram =
    AtomicHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "network/receive/errors/crc",
    description = "The number of packets received which had CRC errors",
    metadata = { unit = "packets" }
)]
pub static NETWORK_RX_CRC_ERRORS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "network/receive/dropped",
    description = "The number of packets received but not processed. Usually due to lack of resources or unsupported protocol. Does not include hardware interface buffer exhaustion.",
    metadata = { unit = "packets" }
)]
pub static NETWORK_RX_DROPPED: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "network/receive/errors/missed",
    description = "The number of packets missed due to buffer exhaustion.",
    metadata = { unit = "packets" }
)]
pub static NETWORK_RX_MISSED_ERRORS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "network/receive/packets",
    description = "The number of packets received over the network",
    metadata = { unit = "packets" }
)]
pub static NETWORK_RX_PACKETS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "network/receive/packets",
    description = "Distribution of network receive packet rate from sample to sample",
    metadata = { unit = "packets/second" }
)]
pub static NETWORK_RX_PACKETS_HISTOGRAM: AtomicHistogram =
    AtomicHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "network/transmit/bytes",
    description = "The number of bytes transmitted over the network",
    metadata = { unit = "bytes" }
)]
pub static NETWORK_TX_BYTES: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "network/transmit/bytes",
    description = "Distribution of network transmit throughput from sample to sample",
    metadata = { unit = "bytes/second" }
)]
pub static NETWORK_TX_BYTES_HISTOGRAM: AtomicHistogram =
    AtomicHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "network/transmit/dropped",
    description = "The number of packets dropped on the transmit path. Usually due to lack of resources.",
    metadata = { unit = "packets" }
)]
pub static NETWORK_TX_DROPPED: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "network/transmit/packets",
    description = "The number of packets transmitted over the network",
    metadata = { unit = "packets" }
)]
pub static NETWORK_TX_PACKETS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "network/transmit/packets",
    description = "Distribution of network transmit packet rate from sample to sample",
    metadata = { unit = "packets/second" }
)]
pub static NETWORK_TX_PACKETS_HISTOGRAM: AtomicHistogram =
    AtomicHistogram::new(HISTOGRAM_GROUPING_POWER, 64);
