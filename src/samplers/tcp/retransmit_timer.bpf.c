// SPDX-License-Identifier: GPL-2.0
// Copyright (c) 2023 IOP Systems, Inc.

#include "../../../vmlinux.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_tracing.h>
#include <bpf/bpf_endian.h>
#include "../../common/bpf.h"

struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, 1);
} rto SEC(".maps");

SEC("fentry/tcp_retransmit_timer")
int BPF_PROG(tcp_retransmit, struct sock *sk)
{
	u64 *cnt;

	u32 idx = 0;
	cnt = bpf_map_lookup_elem(&rto, &idx);

	if (cnt) {
		__sync_fetch_and_add(cnt, 1);
	}

	return 0;
}

SEC("kprobe/tcp_retransmit_timer")
int BPF_KPROBE(tcp_retransmit_kprobe, struct sock *sk)
{
	u64 *cnt;

	u32 idx = 0;
	cnt = bpf_map_lookup_elem(&rto, &idx);

	if (cnt) {
		__sync_fetch_and_add(cnt, 1);
	}

	return 0;
}

char LICENSE[] SEC("license") = "GPL";