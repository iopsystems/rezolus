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
	__uint(max_entries, MAX_CPUS * COUNTER_GROUP_WIDTH);
} counters SEC(".maps");

/*
 * rx/tx drop
 */

SEC("tracepoint/skb/kfree_skb")
int skb_drop_counter(struct trace_event_raw_kfree_skb *ctx) {
    u32 idx = COUNTER_GROUP_WIDTH * bpf_get_smp_processor_id() + DROP;

    array_incr(&counters, idx);

    return 0;
}

/*
 * transmit busy and complete
 */

SEC("tracepoint/net/net_dev_xmit")
int net_dev_xmit(struct trace_event_raw_net_dev_xmit *args)
{
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
int virtio_tx_timeout(struct pt_regs *ctx) {
	tx_timeout()
}

// ena - AWS Elastic Network Adapter
SEC("kprobe/ena_tx_timeout")
int ena_tx_timeout(struct pt_regs *ctx) {
    tx_timeout()
}

// gve - Google Cloud Virtual Ethernet
SEC("kprobe/gve_tx_timeout")
int gve_tx_timeout(struct pt_regs *ctx) {
    tx_timeout()
}

// mlx4 - Mellanox ConnectX-3/4
SEC("kprobe/mlx4_en_tx_timeout")
int mlx4_tx_timeout(struct pt_regs *ctx) {
    tx_timeout()
}

// mlx5 - Mellanox ConnectX-5
SEC("kprobe/mlx5e_tx_timeout")
int mlx5_tx_timeout(struct pt_regs *ctx) {
    tx_timeout()
}

// e1000e - Intel 1GbE
SEC("kprobe/e1000_tx_timeout")
int e1000_tx_timeout(struct pt_regs *ctx) {
    tx_timeout()
}

// igb - Intel 1GbE
SEC("kprobe/igb_tx_timeout")
int igb_tx_timeout(struct pt_regs *ctx) {
    tx_timeout()
}

// ixgbe - Intel 10GbE
SEC("kprobe/ixgbe_tx_timeout")
int ixgbe_tx_timeout(struct pt_regs *ctx) {
    tx_timeout()
}

// i40e - Intel 40GbE
SEC("kprobe/i40e_tx_timeout")
int i40e_tx_timeout(struct pt_regs *ctx) {
    tx_timeout()
}

// ice - Intel 25/100GbE
SEC("kprobe/ice_tx_timeout")
int ice_tx_timeout(struct pt_regs *ctx) {
    tx_timeout()
}

// bnxt_en - Modern Broadcom NICs
SEC("kprobe/bnxt_tx_timeout")
int bnxt_tx_timeout(struct pt_regs *ctx) {
    tx_timeout()
}

// tg3 - Legacy Broadcom
SEC("kprobe/tg3_tx_timeout")
int tg3_tx_timeout(struct pt_regs *ctx) {
    tx_timeout()
}

char LICENSE[] SEC("license") = "GPL";
