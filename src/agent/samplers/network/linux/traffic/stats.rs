use metriken::*;

#[metric(
    name = "network_bytes",
    description = "The number of bytes received over the network",
    metadata = { direction = "receive", unit = "bytes" }
)]
pub static NETWORK_RX_BYTES: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "network_packets",
    description = "The number of packets received over the network",
    metadata = { direction = "receive", unit = "packets" }
)]
pub static NETWORK_RX_PACKETS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "network_bytes",
    description = "The number of bytes transmitted over the network",
    metadata = { direction = "transmit", unit = "bytes" }
)]
pub static NETWORK_TX_BYTES: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "network_packets",
    description = "The number of packets transmitted over the network",
    metadata = { direction = "transmit", unit = "packets" }
)]
pub static NETWORK_TX_PACKETS: LazyCounter = LazyCounter::new(Counter::default);
