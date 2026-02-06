use metriken::*;

#[metric(
    name = "network_ena_bandwidth_allowance_exceeded",
    description = "Packets queued or dropped due to inbound bandwidth allowance being exceeded on an ENA network interface",
    metadata = { direction = "receive", unit = "packets" }
)]
pub static ENA_BW_IN_ALLOWANCE_EXCEEDED: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "network_ena_bandwidth_allowance_exceeded",
    description = "Packets queued or dropped due to outbound bandwidth allowance being exceeded on an ENA network interface",
    metadata = { direction = "transmit", unit = "packets" }
)]
pub static ENA_BW_OUT_ALLOWANCE_EXCEEDED: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "network_ena_pps_allowance_exceeded",
    description = "Packets queued or dropped due to PPS allowance being exceeded on an ENA network interface",
    metadata = { unit = "packets" }
)]
pub static ENA_PPS_ALLOWANCE_EXCEEDED: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "network_ena_conntrack_allowance_exceeded",
    description = "Packets dropped due to connection tracking allowance being exceeded on an ENA network interface",
    metadata = { unit = "packets" }
)]
pub static ENA_CONNTRACK_ALLOWANCE_EXCEEDED: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "network_ena_linklocal_allowance_exceeded",
    description = "Packets dropped due to link-local PPS allowance being exceeded on an ENA network interface",
    metadata = { unit = "packets" }
)]
pub static ENA_LINKLOCAL_ALLOWANCE_EXCEEDED: LazyCounter = LazyCounter::new(Counter::default);
