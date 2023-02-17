// SPDX-License-Identifier: GPL-2.0
// Copyright (c) 2021 Wenbo Zhang
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
	__uint(max_entries, 496);
} srtt SEC(".maps");

SEC("fentry/tcp_rcv_established")
int BPF_PROG(tcp_rcv, struct sock *sk)
{
	const struct inet_sock *inet = (struct inet_sock *)(sk);
	struct tcp_sock *ts;
	u64 key, slot, *cnt;
	u32 srtt_us;
	u64 srtt_ns;

	ts = (struct tcp_sock *)(sk);

	// NOTE: srtt is stored as 8x the value in microseconds
	// but we want to record nanoseconds.
	srtt_ns = 1000 * (u64) BPF_CORE_READ(ts, srtt_us) >> 3;

	u32 idx = value_to_index(srtt_ns);
	cnt = bpf_map_lookup_elem(&srtt, &idx);

	if (cnt) {
		__sync_fetch_and_add(cnt, 1);
	}

	return 0;
}

SEC("kprobe/tcp_rcv_established")
int BPF_KPROBE(tcp_rcv_kprobe, struct sock *sk)
{
	const struct inet_sock *inet = (struct inet_sock *)(sk);
	struct tcp_sock *ts;
	u64 key, slot, *cnt;
	u32 srtt_us;
	u64 srtt_ns;

	ts = (struct tcp_sock *)(sk);
	bpf_probe_read_kernel(&srtt_us, sizeof(srtt_us), &ts->srtt_us);

	// NOTE: srtt is stored as 8x the value in microseconds
	// but we want to record nanoseconds.
	srtt_ns = 1000 * (u64) srtt_us >> 3;

	u32 idx = value_to_index(srtt_ns);
	cnt = bpf_map_lookup_elem(&srtt, &idx);

	if (cnt) {
		__sync_fetch_and_add(cnt, 1);
	}

	return 0;
}

char LICENSE[] SEC("license") = "GPL";