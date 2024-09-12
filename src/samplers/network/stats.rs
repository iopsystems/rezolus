use metriken::*;

#[metric(
    name = "metadata/network_interfaces/collected_at",
    description = "The offset from the Unix epoch when network_interfaces sampler was last run",
    metadata = { unit = "nanoseconds" }
)]
pub static METADATA_NETWORK_INTERFACES_COLLECTED_AT: LazyCounter =
    LazyCounter::new(Counter::default);

#[metric(
    name = "metadata/network_interfaces/runtime",
    description = "The total runtime of the network_interfaces sampler",
    metadata = { unit = "nanoseconds" }
)]
pub static METADATA_NETWORK_INTERFACES_RUNTIME: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "metadata/network_interfaces/runtime",
    description = "Distribution of sampling runtime of the network_interfaces sampler",
    metadata = { unit = "nanoseconds/second" }
)]
pub static METADATA_NETWORK_INTERFACES_RUNTIME_HISTOGRAM: AtomicHistogram =
    AtomicHistogram::new(4, 32);

#[metric(
    name = "metadata/network_traffic/collected_at",
    description = "The offset from the Unix epoch when network_traffic sampler was last run",
    metadata = { unit = "nanoseconds" }
)]
pub static METADATA_NETWORK_TRAFFIC_COLLECTED_AT: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "metadata/network_traffic/runtime",
    description = "The total runtime of the network_traffic sampler",
    metadata = { unit = "nanoseconds" }
)]
pub static METADATA_NETWORK_TRAFFIC_RUNTIME: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "metadata/network_traffic/runtime",
    description = "Distribution of sampling runtime of the network_traffic sampler",
    metadata = { unit = "nanoseconds/second" }
)]
pub static METADATA_NETWORK_TRAFFIC_RUNTIME_HISTOGRAM: AtomicHistogram =
    AtomicHistogram::new(4, 32);

#[metric(
    name = "network/carrier_changes",
    description = "The number of times the link has changes between the UP and DOWN states"
)]
pub static NETWORK_CARRIER_CHANGES: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "network/receive/bytes",
    description = "The number of bytes received over the network",
    metadata = { unit = "bytes" }
)]
pub static NETWORK_RX_BYTES: LazyCounter = LazyCounter::new(Counter::default);

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
    name = "network/transmit/bytes",
    description = "The number of bytes transmitted over the network",
    metadata = { unit = "bytes" }
)]
pub static NETWORK_TX_BYTES: LazyCounter = LazyCounter::new(Counter::default);

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
