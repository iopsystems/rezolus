// SPDX-License-Identifier: GPL-2.0
// Copyright (c) 2024 The Rezolus Authors

// This BPF program probes network send and receive paths to get the number of
// packets and bytes transmitted as well as the size distributions.

#include <vmlinux.h>
#include "../../../agent/bpf/helpers.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_tracing.h>
#include <bpf/bpf_endian.h>

#define COUNTER_GROUP_WIDTH 8
#define MAX_CPUS 1024

// counter indices
//
// The first four count every netdev the packet is handed to. The kernel fires
// these tracepoints once per `net_device`, not once per packet leaving the
// host, so on a stacked netdev (bond, VLAN, bridge, tunnel) the same packet is
// counted once per layer it traverses. See the HOST_ counters below.
#define RX_BYTES 0
#define TX_BYTES 1
#define RX_PACKETS 2
#define TX_PACKETS 3

// The same traffic, restricted to the netdev at the bottom of THIS kernel's
// stack — the one handed to a device driver. See `crosses_host_boundary()`.
//
// Deliberately additive rather than a filter applied to the four above:
// principle 9 — summing is cheap and reversible, un-summing is impossible. A
// predicate baked into the probe discards packets permanently, and every
// recording made in the meantime is unfixable if we later decide differently.
// These four fit in the group's existing unused slots, so both totals cost no
// memory over the one.
#define RX_HOST_BYTES 4
#define TX_HOST_BYTES 5
#define RX_HOST_PACKETS 6
#define TX_HOST_PACKETS 7

// counters
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CPUS* COUNTER_GROUP_WIDTH);
} counters SEC(".maps");

/*
 * Does this netdev sit at the boundary of the host's network stack?
 *
 * True when the device has a bus parent — a `struct device` on PCI, USB,
 * platform, virtio, ... — which is set by `SET_NETDEV_DEV()` and is exactly
 * what `/sys/class/net/<if>/device` reflects. A driver bound to a bus device
 * is the last hop before the frame leaves this kernel's networking code.
 *
 * Verified against v6.12: bond, VLAN, bridge, veth, tun/tap, macvlan, ipvlan,
 * vxlan, dummy and loopback call `SET_NETDEV_DEV()` zero times, so all of them
 * read NULL here. `virtio_net` DOES call it (`&vdev->dev`), so a VM guest
 * counts its own egress rather than reporting zero.
 *
 * Note what this is NOT: a claim that the frame reached a wire. In a guest the
 * boundary is the hypervisor, which may hairpin the frame to another VM on the
 * same box. The guest cannot know, and does not pretend to — this counts what
 * left THIS host's stack, which is the only thing this vantage point can
 * honestly report. `network_host_bytes`'s description says so.
 *
 * Deliberately not a `priv_flags` test. `IFF_BONDING` is set on the physical
 * slave as well as the bond master (bond_main.c: `bond_enslave()` and
 * `bond_setup()` both set it), so skipping on it zeroes a bonded host's
 * traffic instead of halving it. A priv_flags allowlist also misses tunnels,
 * which set no distinguishing bit, and ages as new stacking drivers land.
 * `dev->dev.type->name` is likewise unsuitable as a predicate: wireless sets
 * `wiphy_type`, so a wifi NIC would read as virtual.
 */
static __always_inline bool crosses_host_boundary(struct net_device* dev) {
    return dev && BPF_CORE_READ(dev, dev.parent) != NULL;
}

SEC("raw_tp/netif_receive_skb")
int BPF_PROG(netif_receive_skb, struct sk_buff* skb) {
    u64 len;
    u32 idx;

    len = BPF_CORE_READ(skb, len);

    u32 offset = COUNTER_GROUP_WIDTH * bpf_get_smp_processor_id();

    idx = offset + RX_PACKETS;
    array_incr(&counters, idx);

    idx = offset + RX_BYTES;
    array_add(&counters, idx, len);

    // `skb->dev` is still the receiving device here: this tracepoint sits
    // ABOVE the `another_round:` label in `__netif_receive_skb_core()`, and
    // bonding, bridging and VLAN untagging all re-enter below it. So RX is
    // already counted once, at the physical slave — the asymmetry with TX.
    // The predicate is applied anyway so both directions are counted at the
    // same layer: without it, a veth receive (container east-west) would land
    // in the host-scoped RX total while its TX counterpart did not.
    if (crosses_host_boundary(BPF_CORE_READ(skb, dev))) {
        idx = offset + RX_HOST_PACKETS;
        array_incr(&counters, idx);

        idx = offset + RX_HOST_BYTES;
        array_add(&counters, idx, len);
    }

    return 0;
}

SEC("raw_tp/net_dev_start_xmit")
int BPF_PROG(net_dev_start_xmit, struct sk_buff* skb, struct net_device* dev, void* txq,
             bool more) {
    u64 len;
    u32 idx;

    len = BPF_CORE_READ(skb, len);

    u32 offset = COUNTER_GROUP_WIDTH * bpf_get_smp_processor_id();

    idx = offset + TX_PACKETS;
    array_incr(&counters, idx);

    idx = offset + TX_BYTES;
    array_add(&counters, idx, len);

    // `dev` is this layer's netdev, so on a bond this fires for bond0 AND for
    // the slave. Only the slave has a bus parent, so the host-scoped counters
    // see the packet exactly once.
    if (crosses_host_boundary(dev)) {
        idx = offset + TX_HOST_PACKETS;
        array_incr(&counters, idx);

        idx = offset + TX_HOST_BYTES;
        array_add(&counters, idx, len);
    }

    return 0;
}

char LICENSE[] SEC("license") = "GPL";
