use metriken::*;

#[metric(
    name = "network/receive/bytes",
    description = "The number of bytes received over the network",
    metadata = { unit = "bytes" }
)]
pub static NETWORK_RX_BYTES: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "network/receive/packets",
    description = "The number of packets received over the network",
    metadata = { unit = "packets" }
)]
pub static NETWORK_RX_PACKETS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "network/transmit/bytes",
    description = "The number of bytes transmitted over the network",
    metadata = { unit = "bytes" }
)]
pub static NETWORK_TX_BYTES: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "network/transmit/packets",
    description = "The number of packets transmitted over the network",
    metadata = { unit = "packets" }
)]
pub static NETWORK_TX_PACKETS: LazyCounter = LazyCounter::new(Counter::default);
