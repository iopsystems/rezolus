use metriken::*;

#[metric(
    name = "network/carrier_changes",
    description = "The number of times the link has changes between the UP and DOWN states"
)]
pub static NETWORK_CARRIER_CHANGES: LazyCounter = LazyCounter::new(Counter::default);

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
    name = "network/transmit/dropped",
    description = "The number of packets dropped on the transmit path. Usually due to lack of resources.",
    metadata = { unit = "packets" }
)]
pub static NETWORK_TX_DROPPED: LazyCounter = LazyCounter::new(Counter::default);
