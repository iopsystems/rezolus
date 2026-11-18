include!("../../linux/traffic/stats.rs");

/*
 * interface errors and drops (macOS only)
 */

#[metric(
    name = "network_interface_errors",
    description = "Input errors reported by network interfaces, summed over every interface, loopback included, except the software interfaces under IOResources (the vmenet interfaces of virtual machines). macOS only.",
    metadata = { direction = "receive", unit = "packets", acq_group = "network_traffic_counters" }
)]
pub static NETWORK_RX_ERRORS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "network_interface_errors",
    description = "Output errors reported by network interfaces, summed over every interface, loopback included, except the software interfaces under IOResources (the vmenet interfaces of virtual machines). macOS only.",
    metadata = { direction = "transmit", unit = "packets", acq_group = "network_traffic_counters" }
)]
pub static NETWORK_TX_ERRORS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "network_interface_drops",
    description = "Packets dropped because an interface's input queue was full, summed over every interface, loopback included, except the software interfaces under IOResources (the vmenet interfaces of virtual machines). macOS only.",
    metadata = { direction = "receive", unit = "packets", acq_group = "network_traffic_counters" }
)]
pub static NETWORK_RX_DROPS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "network_interface_drops",
    description = "Packets dropped from an interface's send queue, summed over every interface, loopback included, except the software interfaces under IOResources (the vmenet interfaces of virtual machines). macOS only.",
    metadata = { direction = "transmit", unit = "packets", acq_group = "network_traffic_counters" }
)]
pub static NETWORK_TX_DROPS: LazyCounter = LazyCounter::new(Counter::default);
