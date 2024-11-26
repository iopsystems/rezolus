// SPDX-License-Identifier: GPL-2.0
// Copyright (c) 2021 Wenbo Zhang
// Copyright (c) 2023 The Rezolus Authors

// NOTICE: this file is based off `tcprtt.bpf.c` from the BCC project
// <https://github.com/iovisor/bcc/> and has been modified for use within
// Rezolus.

// This BPF program probes TCP receive path to gather statistics for jitter and
// srtt.

#include <vmlinux.h>
#include "../../../common/bpf/helpers.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_tracing.h>
#include <bpf/bpf_endian.h>

#define HISTOGRAM_BUCKETS HISTOGRAM_BUCKETS_POW_3
#define HISTOGRAM_POWER 3

struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(map_flags, BPF_F_MMAPABLE);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, HISTOGRAM_BUCKETS);
} jitter SEC(".maps");

struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(map_flags, BPF_F_MMAPABLE);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, HISTOGRAM_BUCKETS);
} srtt SEC(".maps");

SEC("kprobe/tcp_rcv_established")
int BPF_KPROBE(tcp_rcv_kprobe, struct sock *sk)
{
	const struct inet_sock *inet = (struct inet_sock *)(sk);
	struct tcp_sock *ts;
	u64 key, slot;
	u32 idx, mdev_us, srtt_us;
	u64 mdev_ns, srtt_ns;

	ts = (struct tcp_sock *)(sk);
	bpf_probe_read_kernel(&srtt_us, sizeof(srtt_us), &ts->srtt_us);
	bpf_probe_read_kernel(&mdev_us, sizeof(mdev_us), &ts->mdev_us);

	// NOTE: srtt is stored as 8x the value in microseconds but we want to
	// record nanoseconds.
	srtt_ns = 1000 * (u64) srtt_us >> 3;

	histogram_incr(&srtt, HISTOGRAM_POWER, srtt_ns);

	// NOTE: mdev is stored as 4x the value in microseconds but we want to
	// record nanoseconds.
	mdev_ns = 1000 * (u64) mdev_us >> 2;

	histogram_incr(&jitter, HISTOGRAM_POWER, mdev_ns);

	return 0;
}

char LICENSE[] SEC("license") = "GPL";
