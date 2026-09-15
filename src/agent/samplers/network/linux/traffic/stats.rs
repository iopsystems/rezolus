use metriken::*;

use crate::agent::timing::AcquisitionGroup;
use linkme::distributed_slice;

// Registered here (not in mod.rs) because this file is also `include!`d
// directly on non-Linux platforms (see `network/mod.rs`'s
// `#[cfg(not(target_os = "linux"))] mod stats` fallback) to keep metric
// identity stable across platforms, while `mod.rs`'s BPF sampler code is
// Linux-only.
//
/// Brackets the `counters` map's refresh (single writer: this sampler's
/// own BPF refresh path).
pub static COUNTERS_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("network_traffic"),
    "network_traffic_counters",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static COUNTERS_ACQ_REG: &'static AcquisitionGroup = &COUNTERS_ACQ;

/*
 * bpf prog stats
 */

#[metric(
    name = "rezolus_bpf_run_count",
    description = "The number of times Rezolus BPF programs have been run",
    metadata = { sampler = "network_traffic"}
)]
pub static BPF_RUN_COUNT: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "rezolus_bpf_run_time",
    description = "The amount of time Rezolus BPF programs have been executing",
    metadata = { unit = "nanoseconds", sampler = "network_traffic"}
)]
pub static BPF_RUN_TIME: LazyCounter = LazyCounter::new(Counter::default);

/*
 * system-wide
 */

#[metric(
    name = "network_bytes",
    description = "The number of bytes transferred over the network. Counted once per network interface the packet is handed to, so a packet crossing a stacked interface (bond, VLAN, bridge, tunnel) is counted once per layer — a bonded host reads roughly 2x its real egress on transmit, 3x for VLAN-over-bond. Includes loopback and intra-host virtual traffic that never reaches a NIC. Use network_host_bytes for traffic crossing the host boundary",
    metadata = { direction = "receive", unit = "bytes", acq_group = "network_traffic_counters" }
)]
pub static NETWORK_RX_BYTES: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "network_packets",
    description = "The number of packets transferred over the network. Counted once per network interface the packet is handed to, so a packet crossing a stacked interface (bond, VLAN, bridge, tunnel) is counted once per layer — a bonded host reads roughly 2x its real egress on transmit, 3x for VLAN-over-bond. Includes loopback and intra-host virtual traffic that never reaches a NIC. Use network_host_packets for traffic crossing the host boundary",
    metadata = { direction = "receive", unit = "packets", acq_group = "network_traffic_counters" }
)]
pub static NETWORK_RX_PACKETS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "network_bytes",
    description = "The number of bytes transferred over the network. Counted once per network interface the packet is handed to, so a packet crossing a stacked interface (bond, VLAN, bridge, tunnel) is counted once per layer — a bonded host reads roughly 2x its real egress on transmit, 3x for VLAN-over-bond. Includes loopback and intra-host virtual traffic that never reaches a NIC. Use network_host_bytes for traffic crossing the host boundary",
    metadata = { direction = "transmit", unit = "bytes", acq_group = "network_traffic_counters" }
)]
pub static NETWORK_TX_BYTES: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "network_packets",
    description = "The number of packets transferred over the network. Counted once per network interface the packet is handed to, so a packet crossing a stacked interface (bond, VLAN, bridge, tunnel) is counted once per layer — a bonded host reads roughly 2x its real egress on transmit, 3x for VLAN-over-bond. Includes loopback and intra-host virtual traffic that never reaches a NIC. Use network_host_packets for traffic crossing the host boundary",
    metadata = { direction = "transmit", unit = "packets", acq_group = "network_traffic_counters" }
)]
pub static NETWORK_TX_PACKETS: LazyCounter = LazyCounter::new(Counter::default);

/*
 * host boundary (north/south)
 *
 * The same traffic as the four counters above, counted once at the netdev
 * handed to a device driver rather than once per netdev it traverses. On a
 * bonded or VLAN'd host the pair above reads ~2x (3x for VLAN-over-bond); this
 * pair reads once. See `crosses_host_boundary()` in `mod.bpf.c` for the
 * predicate and why it is a bus-parent test rather than a `priv_flags` one.
 *
 * "Host boundary" and not "wire" on purpose. On bare metal the two coincide.
 * In a VM guest the boundary is the hypervisor: `virtio_net` has a virtio bus
 * parent, so a guest counts its own egress here, but the hypervisor may
 * hairpin that frame to another guest without it ever reaching a link. The
 * guest cannot distinguish the two, so the metric claims only what it can
 * support.
 *
 * Known scope, stated in `docs/metrics.md` as well: intra-host traffic between
 * virtual interfaces is absent. Container east-west (veth -> bridge -> veth)
 * touches no bus-parented device, so it is counted by the pair above and not
 * by this one. VM east-west is NOT a gap in the same way — each guest counts
 * its own side at `virtio_net`, so a fleet running the agent in guests still
 * sees it. Traffic through an SR-IOV VF passed into a guest is invisible to
 * the hypervisor entirely (no netdev exists for it there); that is a property
 * of SR-IOV, not of this predicate, and needs PF-side hardware counters.
 */

#[metric(
    name = "network_host_bytes",
    description = "The number of bytes crossing this host's network boundary, counted once at the interface bound to a device driver (the wire on bare metal; the hypervisor boundary in a VM guest). Excludes intra-host traffic between virtual interfaces",
    metadata = { direction = "receive", unit = "bytes", acq_group = "network_traffic_counters" }
)]
pub static NETWORK_RX_HOST_BYTES: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "network_host_packets",
    description = "The number of packets crossing this host's network boundary, counted once at the interface bound to a device driver (the wire on bare metal; the hypervisor boundary in a VM guest). Excludes intra-host traffic between virtual interfaces",
    metadata = { direction = "receive", unit = "packets", acq_group = "network_traffic_counters" }
)]
pub static NETWORK_RX_HOST_PACKETS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "network_host_bytes",
    description = "The number of bytes crossing this host's network boundary, counted once at the interface bound to a device driver (the wire on bare metal; the hypervisor boundary in a VM guest). Excludes intra-host traffic between virtual interfaces",
    metadata = { direction = "transmit", unit = "bytes", acq_group = "network_traffic_counters" }
)]
pub static NETWORK_TX_HOST_BYTES: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "network_host_packets",
    description = "The number of packets crossing this host's network boundary, counted once at the interface bound to a device driver (the wire on bare metal; the hypervisor boundary in a VM guest). Excludes intra-host traffic between virtual interfaces",
    metadata = { direction = "transmit", unit = "packets", acq_group = "network_traffic_counters" }
)]
pub static NETWORK_TX_HOST_PACKETS: LazyCounter = LazyCounter::new(Counter::default);
