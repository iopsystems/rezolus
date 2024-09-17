// SPDX-License-Identifier: GPL-2.0
// Copyright (c) 2020 Wenbo Zhang
// Copyright (c) 2024 The Rezolus Authors

// NOTICE: this file is based off `tcpconnlat.bpf.c` from the BCC project
// <https://github.com/iovisor/bcc/> and has been modified for use within
// Rezolus.

// This BPF program probes TCP active connect to measure the latency
// distribution for establishing connections to hosts.

#include <vmlinux.h>
#include "../../../common/bpf/histogram.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_tracing.h>

#define HISTOGRAM_BUCKETS HISTOGRAM_BUCKETS_POW_3
#define HISTOGRAM_POWER 3

#define MAX_ENTRIES	10240
#define AF_INET 2
#define AF_INET6 10
#define NO_EXIST 1

struct {
	__uint(type, BPF_MAP_TYPE_HASH);
	__uint(max_entries, MAX_ENTRIES);
	__type(key, u64);
	__type(value, u64);
} start SEC(".maps");

struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(map_flags, BPF_F_MMAPABLE);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, HISTOGRAM_BUCKETS);
} latency SEC(".maps");

static __always_inline __u64 get_sock_ident(struct sock *sk)
{
	return (__u64)sk;
}

static int trace_connect(struct sock *sk)
{
	u64 sock_ident, ts;

	sock_ident = get_sock_ident(sk);
	ts = bpf_ktime_get_ns();

	bpf_map_update_elem(&start, &sock_ident, &ts, NO_EXIST);

	return 0;
}

static int handle_tcp_rcv_state_process(void *ctx, struct sock *sk)
{
	u32 idx;
	u64 sock_ident, now, delta_ns, *cnt, *tsp;

	if (BPF_CORE_READ(sk, __sk_common.skc_state) != TCP_SYN_SENT)
		return 0;

	sock_ident = get_sock_ident(sk);

	tsp = bpf_map_lookup_elem(&start, &sock_ident);
	if (!tsp) {
		return 0;
	}

	now = bpf_ktime_get_ns();

	if (*tsp > now) {
		goto cleanup;
	}

	delta_ns = (now - *tsp);

	idx = value_to_index(delta_ns, HISTOGRAM_POWER);
	cnt = bpf_map_lookup_elem(&latency, &idx);

	if (cnt) {
		__atomic_fetch_add(cnt, 1, __ATOMIC_RELAXED);
	}

cleanup:
	bpf_map_delete_elem(&start, &sock_ident);
	return 0;
}

SEC("kprobe/tcp_v4_connect")
int BPF_KPROBE(tcp_v4_connect, struct sock *sk)
{
	return trace_connect(sk);
}

SEC("kprobe/tcp_v6_connect")
int BPF_KPROBE(tcp_v6_connect, struct sock *sk)
{
	return trace_connect(sk);
}

SEC("kprobe/tcp_rcv_state_process")
int BPF_KPROBE(tcp_rcv_state_process, struct sock *sk)
{
	return handle_tcp_rcv_state_process(ctx, sk);
}

SEC("tracepoint/tcp/tcp_destroy_sock")
int tcp_destroy_sock(struct trace_event_raw_tcp_event_sk *ctx)
{
	struct sock *sk = ctx->skaddr;

	u64 sock_ident = get_sock_ident(sk);
	bpf_map_delete_elem(&start, &sock_ident);

	return 0;
}

char LICENSE[] SEC("license") = "GPL";
