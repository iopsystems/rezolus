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

/*
 * Counting, shared by both attach flavors.
 *
 * Split out so the two variants below differ ONLY in how they read the kernel
 * structs — which is the entire point of having two — rather than in what they
 * count.
 */
static __always_inline void account(u32 rx_or_tx_bytes, u32 rx_or_tx_packets,
                                    u32 host_bytes_idx, u32 host_packets_idx, u64 len,
                                    bool crosses_boundary) {
    u32 offset = COUNTER_GROUP_WIDTH * bpf_get_smp_processor_id();

    array_incr(&counters, offset + rx_or_tx_packets);
    array_add(&counters, offset + rx_or_tx_bytes, len);

    if (crosses_boundary) {
        array_incr(&counters, offset + host_packets_idx);
        array_add(&counters, offset + host_bytes_idx, len);
    }
}

/*
 * ---- BTF-typed attach (preferred) -------------------------------------
 *
 * `tp_btf` hands the verifier the tracepoint's argument TYPES, so a field
 * access compiles to a direct load. The `raw_tp` twin below must reach the
 * same fields through `bpf_probe_read_kernel`, which is a helper CALL.
 *
 * That difference is the whole reason both exist. Measured on this sampler,
 * the probe-read path cost 73.6 ns/run against 44.8 before the host-boundary
 * counters were added (#1218) — a baseline where one or two helper calls
 * dominate. The reads here are relocatable all the same: `vmlinux.h` applies
 * `preserve_access_index` to every record, so clang emits CO-RE relocations
 * for direct accesses just as it does inside `BPF_CORE_READ`.
 *
 * Which pair attaches is decided in `mod.rs` by `kernel_has_btf()`; the other
 * is disabled, never loaded. Same two-program shape `cpu_migrations` uses.
 */

SEC("tp_btf/netif_receive_skb")
int BPF_PROG(netif_receive_skb_btf, struct sk_buff* skb) {
    // `skb->dev` is still the RECEIVING device here: this tracepoint sits
    // ABOVE the `another_round:` label in `__netif_receive_skb_core()`, and
    // bonding, bridging and VLAN untagging all re-enter below it. So RX is
    // already counted once, at the physical slave — the asymmetry with TX.
    // The predicate is applied anyway so both directions are counted at the
    // same layer: without it, a veth receive (container east-west) would land
    // in the host-scoped RX total while its TX counterpart did not.
    struct net_device* dev = skb->dev;

    account(RX_BYTES, RX_PACKETS, RX_HOST_BYTES, RX_HOST_PACKETS, skb->len,
            dev && dev->dev.parent);
    return 0;
}

SEC("tp_btf/net_dev_start_xmit")
int BPF_PROG(net_dev_start_xmit_btf, struct sk_buff* skb, struct net_device* dev) {
    // `dev` is this layer's netdev, so on a bond this fires for bond0 AND for
    // the slave. Only the slave has a bus parent, so the host-scoped counters
    // see the packet exactly once.
    account(TX_BYTES, TX_PACKETS, TX_HOST_BYTES, TX_HOST_PACKETS, skb->len,
            dev && dev->dev.parent);
    return 0;
}

/*
 * ---- Probe-read attach (fallback) -------------------------------------
 *
 * For a kernel without BTF. Identical accounting, every field reached through
 * `bpf_probe_read_kernel`.
 */

SEC("raw_tp/netif_receive_skb")
int BPF_PROG(netif_receive_skb_raw, struct sk_buff* skb) {
    struct net_device* dev = BPF_CORE_READ(skb, dev);

    account(RX_BYTES, RX_PACKETS, RX_HOST_BYTES, RX_HOST_PACKETS, BPF_CORE_READ(skb, len),
            crosses_host_boundary(dev));
    return 0;
}

SEC("raw_tp/net_dev_start_xmit")
int BPF_PROG(net_dev_start_xmit_raw, struct sk_buff* skb, struct net_device* dev, void* txq,
             bool more) {
    account(TX_BYTES, TX_PACKETS, TX_HOST_BYTES, TX_HOST_PACKETS, BPF_CORE_READ(skb, len),
            crosses_host_boundary(dev));
    return 0;
}

char LICENSE[] SEC("license") = "GPL";
