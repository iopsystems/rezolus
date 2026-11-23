use metriken::*;

use crate::agent::timing::AcquisitionGroup;
use linkme::distributed_slice;

/// Upper bound on interfaces tracked by this sampler.
///
/// Only interfaces that actually expose one of the stats we look for get a
/// slot (see `probe_interface`), so this is not "every netdev on the box" —
/// a container host's veth churn never reaches here. 64 is generous for NICs
/// on a single machine and leaves room for the port counters in #1213.
pub const MAX_INTERFACES: usize = 64;

/// Every ENA allowance counter, one member per tracked interface. Single
/// writer: `refresh` in `mod.rs`, which reads every interface's stats under
/// one acquisition. One group for all five families because one
/// `ETHTOOL_GSTATS` ioctl per interface returns them together: principle 18's
/// device-visit sweep.
///
/// Declared so that a counter at zero is published as zero; the default group
/// skips a zero-valued `CounterGroup` entry (#1727).
pub static ENA_ALLOWANCE_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("network_ethtool"),
    "network_ethtool_interfaces",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static ENA_ALLOWANCE_ACQ_REG: &'static AcquisitionGroup = &ENA_ALLOWANCE_ACQ;

#[metric(
    name = "network_ena_bandwidth_allowance_exceeded",
    description = "Packets queued or dropped due to inbound bandwidth allowance being exceeded on an ENA network interface",
    metadata = { direction = "receive", unit = "packets", acq_group = "network_ethtool_interfaces" }
)]
pub static ENA_BW_IN_ALLOWANCE_EXCEEDED: CounterGroup = CounterGroup::new(MAX_INTERFACES);

#[metric(
    name = "network_ena_bandwidth_allowance_exceeded",
    description = "Packets queued or dropped due to outbound bandwidth allowance being exceeded on an ENA network interface",
    metadata = { direction = "transmit", unit = "packets", acq_group = "network_ethtool_interfaces" }
)]
pub static ENA_BW_OUT_ALLOWANCE_EXCEEDED: CounterGroup = CounterGroup::new(MAX_INTERFACES);

#[metric(
    name = "network_ena_pps_allowance_exceeded",
    description = "Packets queued or dropped due to PPS allowance being exceeded on an ENA network interface",
    metadata = { unit = "packets", acq_group = "network_ethtool_interfaces" }
)]
pub static ENA_PPS_ALLOWANCE_EXCEEDED: CounterGroup = CounterGroup::new(MAX_INTERFACES);

#[metric(
    name = "network_ena_conntrack_allowance_exceeded",
    description = "Packets dropped due to connection tracking allowance being exceeded on an ENA network interface",
    metadata = { unit = "packets", acq_group = "network_ethtool_interfaces" }
)]
pub static ENA_CONNTRACK_ALLOWANCE_EXCEEDED: CounterGroup = CounterGroup::new(MAX_INTERFACES);

#[metric(
    name = "network_ena_linklocal_allowance_exceeded",
    description = "Packets dropped due to link-local PPS allowance being exceeded on an ENA network interface",
    metadata = { unit = "packets", acq_group = "network_ethtool_interfaces" }
)]
pub static ENA_LINKLOCAL_ALLOWANCE_EXCEEDED: CounterGroup = CounterGroup::new(MAX_INTERFACES);
