// SPDX-License-Identifier: GPL-2.0
// Copyright (c) 2020 Wenbo Zhang
// Copyright (c) 2024 The Rezolus Authors

// NOTICE: this file is based off `tcpconnlat.bpf.c` from the BCC project
// <https://github.com/iovisor/bcc/> and has been modified for use within
// Rezolus.

// This BPF program probes TCP active connect to measure, per outcome, the
// time from connect() to the end of the handshake: established, refused,
// timed out, failed with another error, or closed before an answer.

#include <vmlinux.h>
#include "../../../agent/bpf/helpers.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_tracing.h>

#define HISTOGRAM_BUCKETS HISTOGRAM_BUCKETS_POW_3
#define HISTOGRAM_POWER 3

#define MAX_ENTRIES 10240
#define AF_INET 2
#define AF_INET6 10
#define NO_EXIST 1

// errno values (include/uapi/asm-generic/errno.h; the same on x86_64 and
// arm64).
#define ETIMEDOUT 110
#define ECONNREFUSED 111

struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, MAX_ENTRIES);
    __type(key, u64);
    __type(value, u64);
} start SEC(".maps");

// One histogram per outcome, each the `tcp_connect_latency` metric with its
// `outcome` label (see stats.rs).
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS * HISTOGRAM_SLICES);
} established SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS * HISTOGRAM_SLICES);
} refused SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS * HISTOGRAM_SLICES);
} timeout SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS * HISTOGRAM_SLICES);
} error SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS * HISTOGRAM_SLICES);
} aborted SEC(".maps");

static __always_inline __u64 get_sock_ident(struct sock* sk) {
    return (__u64)sk;
}

static int trace_connect(struct sock* sk) {
    u64 sock_ident, ts;

    sock_ident = get_sock_ident(sk);
    ts = bpf_ktime_get_ns();

    bpf_map_update_elem(&start, &sock_ident, &ts, NO_EXIST);

    return 0;
}

// The connect's outcome is read where the socket leaves SYN_SENT. Every inet
// socket state change fires the tracepoint, so the programs below pass on
// only TCP transitions from SYN_SENT to ESTABLISHED or CLOSE. A simultaneous
// open leaves SYN_SENT for SYN_RECV and is not counted; tcp_destroy_sock
// removes its start time. A socket with a start time that becomes
// ESTABLISHED completed its handshake, or was connected in TCP_REPAIR mode
// with no handshake. One that becomes CLOSE failed, and sk_err says why:
// tcp_reset() sets ECONNREFUSED for a RST in SYN_SENT, and an ICMP error sets
// its errno (ECONNREFUSED for port unreachable). An ICMP error that arrives
// while a system call holds the socket lock, as during connect(), is kept in
// sk_err_soft instead. When the SYN retries run out, tcp_write_err() sets
// sk_err to sk_err_soft, or to ETIMEDOUT if sk_err_soft is 0. Each sets
// sk_err before tcp_done() closes the socket. A close() by the caller, or a
// failure inside connect() after the SYN_SENT transition, closes the socket
// with sk_err 0.
static __always_inline int handle_set_state(u64 sock_ident, int newstate, int sk_err) {
    u64 now, *tsp;
    void* hist;

    if (newstate == TCP_ESTABLISHED) {
        hist = &established;
    } else if (newstate == TCP_CLOSE) {
        if (sk_err == ECONNREFUSED) {
            hist = &refused;
        } else if (sk_err == ETIMEDOUT) {
            hist = &timeout;
        } else if (sk_err) {
            hist = &error;
        } else {
            hist = &aborted;
        }
    } else {
        return 0;
    }

    tsp = bpf_map_lookup_elem(&start, &sock_ident);
    if (!tsp) {
        return 0;
    }

    now = bpf_ktime_get_ns();

    if (*tsp <= now) {
        histogram_incr(hist, HISTOGRAM_POWER, now - *tsp);
    }

    bpf_map_delete_elem(&start, &sock_ident);
    return 0;
}

// fentry/kprobe twins share trace_connect(); only the attach mechanism
// differs. fentry is the cheaper dispatch (see
// docs/journal/2026-09-04-fentry-vs-kprobe-dispatch.md) but needs BTF, so the
// kprobe twins are the fallback; per target, one is disabled at load time on
// kernel_can_fentry() (see disabled_programs in mod.rs).

SEC("fentry/tcp_v4_connect")
int BPF_PROG(tcp_v4_connect_fentry, struct sock* sk) {
    return trace_connect(sk);
}

SEC("kprobe/tcp_v4_connect")
int BPF_KPROBE(tcp_v4_connect_kprobe, struct sock* sk) {
    return trace_connect(sk);
}

SEC("fentry/tcp_v6_connect")
int BPF_PROG(tcp_v6_connect_fentry, struct sock* sk) {
    return trace_connect(sk);
}

SEC("kprobe/tcp_v6_connect")
int BPF_KPROBE(tcp_v6_connect_kprobe, struct sock* sk) {
    return trace_connect(sk);
}

// tp_btf/raw_tp twins of inet_sock_set_state share handle_set_state(); one is
// disabled at load time on kernel_btf_has_tracepoints() (see mod.rs).

SEC("tp_btf/inet_sock_set_state")
int BPF_PROG(inet_sock_set_state_btf, struct sock* sk, int oldstate, int newstate) {
    u64 sock_ident = 0;

    if (oldstate != TCP_SYN_SENT || (newstate != TCP_ESTABLISHED && newstate != TCP_CLOSE) ||
        sk->sk_protocol != IPPROTO_TCP) {
        return 0;
    }

    // The verifier does not allow arithmetic on the pointer argument itself.
    bpf_probe_read_kernel(&sock_ident, sizeof(sock_ident), &ctx[0]);
    return handle_set_state(sock_ident, newstate, sk->sk_err);
}

SEC("raw_tp/inet_sock_set_state")
int BPF_PROG(inet_sock_set_state_raw, struct sock* sk, int oldstate, int newstate) {
    if (oldstate != TCP_SYN_SENT || (newstate != TCP_ESTABLISHED && newstate != TCP_CLOSE) ||
        BPF_CORE_READ(sk, sk_protocol) != IPPROTO_TCP) {
        return 0;
    }

    return handle_set_state(get_sock_ident(sk), newstate, BPF_CORE_READ(sk, sk_err));
}

SEC("tracepoint/tcp/tcp_destroy_sock")
int tcp_destroy_sock(struct trace_event_raw_tcp_event_sk* ctx) {
    struct sock* sk = (struct sock*)(ctx->skaddr);

    u64 sock_ident = get_sock_ident(sk);
    bpf_map_delete_elem(&start, &sock_ident);

    return 0;
}

char LICENSE[] SEC("license") = "GPL";
