// SPDX-License-Identifier: GPL-2.0
// Copyright (c) 2023 IOP Systems, Inc.

// This BPF program probes TCP retransmit path to gather statistics.

#include "../../../../common/bpf/vmlinux.h"
#include "../../../../common/bpf/histogram.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_tracing.h>
#include <bpf/bpf_endian.h>

#define RETRANSMITS 0

struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, 1);
} counters SEC(".maps");

SEC("fentry/tcp_retransmit_timer")
int BPF_PROG(tcp_retransmit, struct sock *sk)
{
	u64 *cnt;

	u32 idx = RETRANSMITS;
	cnt = bpf_map_lookup_elem(&counters, &idx);

	if (cnt) {
		__sync_fetch_and_add(cnt, 1);
	}

	return 0;
}

SEC("kprobe/tcp_retransmit_timer")
int BPF_KPROBE(tcp_retransmit_kprobe, struct sock *sk)
{
	u64 *cnt;

	u32 idx = RETRANSMITS;
	cnt = bpf_map_lookup_elem(&counters, &idx);

	if (cnt) {
		__sync_fetch_and_add(cnt, 1);
	}

	return 0;
}

char LICENSE[] SEC("license") = "GPL";