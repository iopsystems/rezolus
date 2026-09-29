use metriken::*;

/// Upper bound on interfaces tracked by this sampler.
///
/// Only interfaces that actually expose one of the stats we look for get a
/// slot (see `probe_interface`), so this is not "every netdev on the box" —
/// a container host's veth churn never reaches here. 64 is generous for NICs
/// on a single machine and leaves room for the port counters in #1213.
pub const MAX_INTERFACES: usize = 64;

#[metric(
    name = "network_ena_bandwidth_allowance_exceeded",
    description = "Packets queued or dropped due to inbound bandwidth allowance being exceeded on an ENA network interface",
    metadata = { direction = "receive", unit = "packets" }
)]
pub static ENA_BW_IN_ALLOWANCE_EXCEEDED: CounterGroup = CounterGroup::new(MAX_INTERFACES);

#[metric(
    name = "network_ena_bandwidth_allowance_exceeded",
    description = "Packets queued or dropped due to outbound bandwidth allowance being exceeded on an ENA network interface",
    metadata = { direction = "transmit", unit = "packets" }
)]
pub static ENA_BW_OUT_ALLOWANCE_EXCEEDED: CounterGroup = CounterGroup::new(MAX_INTERFACES);

#[metric(
    name = "network_ena_pps_allowance_exceeded",
    description = "Packets queued or dropped due to PPS allowance being exceeded on an ENA network interface",
    metadata = { unit = "packets" }
)]
pub static ENA_PPS_ALLOWANCE_EXCEEDED: CounterGroup = CounterGroup::new(MAX_INTERFACES);

#[metric(
    name = "network_ena_conntrack_allowance_exceeded",
    description = "Packets dropped due to connection tracking allowance being exceeded on an ENA network interface",
    metadata = { unit = "packets" }
)]
pub static ENA_CONNTRACK_ALLOWANCE_EXCEEDED: CounterGroup = CounterGroup::new(MAX_INTERFACES);

#[metric(
    name = "network_ena_linklocal_allowance_exceeded",
    description = "Packets dropped due to link-local PPS allowance being exceeded on an ENA network interface",
    metadata = { unit = "packets" }
)]
pub static ENA_LINKLOCAL_ALLOWANCE_EXCEEDED: CounterGroup = CounterGroup::new(MAX_INTERFACES);
