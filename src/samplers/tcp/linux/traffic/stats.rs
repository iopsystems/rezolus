use crate::common::HISTOGRAM_GROUPING_POWER;
use metriken::*;

#[metric(
    name = "tcp_receive_bytes",
    description = "The number of bytes received over TCP",
    metadata = { unit = "bytes" }
)]
pub static TCP_RX_BYTES: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "tcp_receive_packets",
    description = "The number of packets received over TCP",
    metadata = { unit = "packets" }
)]
pub static TCP_RX_PACKETS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "tcp_receive_size",
    description = "Distribution of the size of TCP packets received after reassembly",
    metadata = { unit = "bytes" }
)]
pub static TCP_RX_SIZE: RwLockHistogram = RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "tcp_transmit_bytes",
    description = "The number of bytes transmitted over TCP",
    metadata = { unit = "bytes" }
)]
pub static TCP_TX_BYTES: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "tcp_transmit_packets",
    description = "The number of packets transmitted over TCP",
    metadata = { unit = "packets" }
)]
pub static TCP_TX_PACKETS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "tcp_transmit_size",
    description = "Distribution of the size of TCP packets transmitted before fragmentation",
    metadata = { unit = "bytes" }
)]
pub static TCP_TX_SIZE: RwLockHistogram = RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);
