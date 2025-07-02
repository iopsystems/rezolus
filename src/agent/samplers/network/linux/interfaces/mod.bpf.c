// SPDX-License-Identifier: GPL-2.0
// Copyright (c) 2025 The Rezolus Authors

#include <vmlinux.h>
#include "../../../agent/bpf/helpers.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_tracing.h>
#include <bpf/bpf_core_read.h>

#define COUNTER_GROUP_WIDTH 8
#define MAX_CPUS 1024

// counter offsets
#define DROP 0
#define TX_BUSY 1
#define TX_COMPLETE 2
#define TX_TIMEOUT 3

// counters
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CPUS* COUNTER_GROUP_WIDTH);
} counters SEC(".maps");

/*
 * rx/tx drop
 */

SEC("tracepoint/skb/kfree_skb")
int kfree_skb(struct trace_event_raw_kfree_skb* ctx) {
    // skip accounting if the reason field doesn't exist (kernel < 5.17)
    if (!bpf_core_field_exists(ctx->reason)) {
        return 0;
    }

    u32 reason = BPF_CORE_READ(ctx, reason);

    switch (reason) {
    // device/hardware issues
    case SKB_DROP_REASON_DEV_HDR:
    case SKB_DROP_REASON_DEV_READY:
    case SKB_DROP_REASON_FULL_RING:

    // memory/resource exhaustion
    case SKB_DROP_REASON_NOMEM:
    case SKB_DROP_REASON_SOCKET_RCVBUFF:
    case SKB_DROP_REASON_PROTO_MEM:
    case SKB_DROP_REASON_CPU_BACKLOG:
    case SKB_DROP_REASON_QDISC_DROP:

    // checksum/corruption errors
    case SKB_DROP_REASON_IP_CSUM:
    case SKB_DROP_REASON_TCP_CSUM:
    case SKB_DROP_REASON_UDP_CSUM:
    case SKB_DROP_REASON_ICMP_CSUM:
    case SKB_DROP_REASON_SKB_CSUM:

    // size/format issues
    case SKB_DROP_REASON_PKT_TOO_BIG:
    case SKB_DROP_REASON_PKT_TOO_SMALL:
    case SKB_DROP_REASON_HDR_TRUNC:
    case SKB_DROP_REASON_IP_INHDR:

    // network infrastructure issues
    case SKB_DROP_REASON_NEIGH_CREATEFAIL:
    case SKB_DROP_REASON_NEIGH_FAILED:
    case SKB_DROP_REASON_NEIGH_QUEUEFULL:
    case SKB_DROP_REASON_NEIGH_DEAD:
    case SKB_DROP_REASON_IP_OUTNOROUTES:
    case SKB_DROP_REASON_IP_INNOROUTES:
        break;

    default:
        return 0; // skip accounting for normal operations
    }

    u32 idx = COUNTER_GROUP_WIDTH * bpf_get_smp_processor_id() + DROP;
    array_incr(&counters, idx);
    return 0;
}

/*
 * transmit busy and complete
 */

SEC("tracepoint/net/net_dev_xmit")
int net_dev_xmit(struct trace_event_raw_net_dev_xmit* args) {
    u32 offset = COUNTER_GROUP_WIDTH * bpf_get_smp_processor_id();

    u32 idx = 0;

    if (args->rc != 0) {
        idx = offset + TX_BUSY;

        array_incr(&counters, idx);
    } else {
        idx = offset + TX_COMPLETE;

        array_incr(&counters, idx);
    }

    return 0;
}

/*
 * transmit timeouts - driver specific probes
 */

// helper function
int tx_timeout() {
    u32 idx = COUNTER_GROUP_WIDTH * bpf_get_smp_processor_id() + TX_TIMEOUT;

    array_incr(&counters, idx);

    return 0;
}

// virt_net - VirtIO
SEC("kprobe/virtnet_tx_timeout")
__attribute__((weak)) int virtio_tx_timeout(struct pt_regs* ctx) {
    return tx_timeout();
}

// ena - AWS Elastic Network Adapter
SEC("kprobe/ena_tx_timeout")
__attribute__((weak)) int ena_tx_timeout(struct pt_regs* ctx) {
    return tx_timeout();
}

// gve - Google Cloud Virtual Ethernet
SEC("kprobe/gve_tx_timeout")
__attribute__((weak)) int gve_tx_timeout(struct pt_regs* ctx) {
    return tx_timeout();
}

// mlx4 - Mellanox ConnectX-3/4
SEC("kprobe/mlx4_en_tx_timeout")
__attribute__((weak)) int mlx4_tx_timeout(struct pt_regs* ctx) {
    return tx_timeout();
}

// mlx5 - Mellanox ConnectX-5
SEC("kprobe/mlx5e_tx_timeout")
__attribute__((weak)) int mlx5_tx_timeout(struct pt_regs* ctx) {
    return tx_timeout();
}

// e1000e - Intel 1GbE
SEC("kprobe/e1000_tx_timeout")
__attribute__((weak)) int e1000_tx_timeout(struct pt_regs* ctx) {
    return tx_timeout();
}

// igb - Intel 1GbE
SEC("kprobe/igb_tx_timeout")
__attribute__((weak)) int igb_tx_timeout(struct pt_regs* ctx) {
    return tx_timeout();
}

// ixgbe - Intel 10GbE
SEC("kprobe/ixgbe_tx_timeout")
__attribute__((weak)) int ixgbe_tx_timeout(struct pt_regs* ctx) {
    return tx_timeout();
}

// i40e - Intel 40GbE
SEC("kprobe/i40e_tx_timeout")
__attribute__((weak)) int i40e_tx_timeout(struct pt_regs* ctx) {
    return tx_timeout();
}

// ice - Intel 25/100GbE
SEC("kprobe/ice_tx_timeout")
__attribute__((weak)) int ice_tx_timeout(struct pt_regs* ctx) {
    return tx_timeout();
}

// bnxt_en - Modern Broadcom NICs
SEC("kprobe/bnxt_tx_timeout")
__attribute__((weak)) int bnxt_tx_timeout(struct pt_regs* ctx) {
    return tx_timeout();
}

// tg3 - Legacy Broadcom
SEC("kprobe/tg3_tx_timeout")
__attribute__((weak)) int tg3_tx_timeout(struct pt_regs* ctx) {
    return tx_timeout();
}

char LICENSE[] SEC("license") = "GPL";
