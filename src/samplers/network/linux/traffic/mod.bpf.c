// SPDX-License-Identifier: GPL-2.0
// Copyright (c) 2024 The Rezolus Authors

// This BPF program probes network send and receive paths to get the number of
// packets and bytes transmitted as well as the size distributions.

#include <vmlinux.h>
#include "../../../common/bpf/histogram.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_tracing.h>
#include <bpf/bpf_endian.h>

#define COUNTER_GROUP_WIDTH 16
#define MAX_CPUS 1024

// counter indices
#define RX_BYTES 0
#define TX_BYTES 1
#define RX_PACKETS 2
#define TX_PACKETS 3

// counters
struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(map_flags, BPF_F_MMAPABLE);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, MAX_CPUS * COUNTER_GROUP_WIDTH);
} counters SEC(".maps");

SEC("raw_tp/netif_receive_skb")
int BPF_PROG(netif_receive_skb, struct sk_buff *skb)
{
	u64 len;
	u64 *cnt;
	u32 idx;
	struct net_device *dev;
	u8 addr_assign_type;

	dev = BPF_CORE_READ(skb, dev);
	addr_assign_type = BPF_CORE_READ(dev, addr_assign_type);

	if (addr_assign_type != 0) {
		return 0;
	}

	len = BPF_CORE_READ(skb, len);

	idx = 8 * bpf_get_smp_processor_id() + RX_PACKETS;
	cnt = bpf_map_lookup_elem(&counters, &idx);

	if (cnt) {
		__sync_fetch_and_add(cnt, 1);
	}

	idx = COUNTER_GROUP_WIDTH * bpf_get_smp_processor_id() + RX_BYTES;
	cnt = bpf_map_lookup_elem(&counters, &idx);

	if (cnt) {
		__sync_fetch_and_add(cnt, len);
	}

	return 0;
}


SEC("raw_tp/net_dev_start_xmit")
int BPF_PROG(tcp_cleanup_rbuf, struct sk_buff *skb, struct net_device *dev, void *txq, bool more)
{
	u64 len;
	u64 *cnt;
	u32 idx;
	u8 addr_assign_type;

	addr_assign_type = BPF_CORE_READ(dev, addr_assign_type);

	if (addr_assign_type != 0) {
		return 0;
	}

	len = BPF_CORE_READ(skb, len);

	idx = 8 * bpf_get_smp_processor_id() + TX_PACKETS;
	cnt = bpf_map_lookup_elem(&counters, &idx);

	if (cnt) {
		__sync_fetch_and_add(cnt, 1);
	}

	idx = COUNTER_GROUP_WIDTH * bpf_get_smp_processor_id() + TX_BYTES;
	cnt = bpf_map_lookup_elem(&counters, &idx);

	if (cnt) {
		__sync_fetch_and_add(cnt, len);
	}

	return 0;
}

char LICENSE[] SEC("license") = "GPL";