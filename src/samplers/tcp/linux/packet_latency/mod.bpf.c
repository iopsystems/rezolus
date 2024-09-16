// SPDX-License-Identifier: GPL-2.0
// Copyright (c) 2023 Wenbo Zhang
// Copyright (c) 2023 The Rezolus Authors

// NOTICE: this file is based off `tcppktlat.bpf.c` from the BCC project
// <https://github.com/iovisor/bcc/> and has been modified for use within
// Rezolus.

// This BPF program probes TCP receive path gather statistics about the latency
// from a packet being received to it being processed by the userspace
// application.

#include <vmlinux.h>
#include "../../../common/bpf/histogram.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_tracing.h>

#define HISTOGRAM_BUCKETS HISTOGRAM_BUCKETS_POW_3
#define HISTOGRAM_POWER 3

#define MAX_ENTRIES	10240
#define AF_INET		2
#define NO_EXIST    1

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

static int handle_tcp_probe(struct sock *sk, struct sk_buff *skb)
{
	const struct inet_sock *inet = (struct inet_sock *)(sk);
	u64 sock_ident, ts, len, doff;
	const struct tcphdr *th;

	th = (const struct tcphdr*)BPF_CORE_READ(skb, data);
	doff = BPF_CORE_READ_BITFIELD_PROBED(th, doff);
	len = BPF_CORE_READ(skb, len);
	/* `doff * 4` means `__tcp_hdrlen` */
	if (len <= doff * 4) {
		return 0;
	}
	sock_ident = get_sock_ident(sk);
	ts = bpf_ktime_get_ns();
	bpf_map_update_elem(&start, &sock_ident, &ts, NO_EXIST);
	return 0;
}

static int handle_tcp_rcv_space_adjust(void *ctx, struct sock *sk)
{
	const struct inet_sock *inet = (struct inet_sock *)(sk);
	u64 sock_ident = get_sock_ident(sk);
	u64 id = bpf_get_current_pid_tgid(), *tsp;
	u32 idx;
	u64 now, delta_ns, *cnt;
	u32 pid = id >> 32, tid = id;
	struct event *eventp;
	u16 family;

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

static int handle_tcp_destroy_sock(void *ctx, struct sock *sk)
{
	u64 sock_ident = get_sock_ident(sk);

	bpf_map_delete_elem(&start, &sock_ident);
	return 0;
}

SEC("raw_tp/tcp_probe")
int BPF_PROG(tcp_probe, struct sock *sk, struct sk_buff *skb) {
	return handle_tcp_probe(sk, skb);
}

SEC("raw_tp/tcp_rcv_space_adjust")
int BPF_PROG(tcp_rcv_space_adjust, struct sock *sk)
{
	return handle_tcp_rcv_space_adjust(ctx, sk);
}

SEC("raw_tp/tcp_destroy_sock")
int BPF_PROG(tcp_destroy_sock, struct sock *sk)
{
	return handle_tcp_destroy_sock(ctx, sk);
}

char LICENSE[] SEC("license") = "GPL";
