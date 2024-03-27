use crate::common::HISTOGRAM_GROUPING_POWER;
use crate::*;
use metriken::{metric, AtomicHistogram, Counter, Gauge, LazyCounter, LazyGauge};

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
