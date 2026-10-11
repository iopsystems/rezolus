// SPDX-License-Identifier: GPL-2.0
// Copyright (c) 2023 Wenbo Zhang
// Copyright (c) 2023 The Rezolus Authors

// NOTICE: this file is based off `tcppktlat.bpf.c` from the BCC project
// <https://github.com/iovisor/bcc/> and has been modified for use within
// Rezolus.

// This BPF program measures the latency from TCP data being received to the
// application reading it.
//
// A sample is the time from an in-order data segment (sequence number
// `rcv_nxt`, so its data is readable once queued) to the next
// `tcp_rcv_space_adjust` on that socket. The kernel calls that after each
// chunk a read or peek copies, and at the end of some sockmap and `read_sock`
// reads. A segment that arrives while the socket already has a start time
// leaves it, so the sample covers the oldest data since the last read. On a
// sockmap socket with a stream parser the sample ends at the parser's read
// (`tcp_read_sock`), whatever the verdict; with a verdict program alone it
// ends at the verdict for redirected or dropped data.
//
// The start time is kept in a fixed array of 2^19 slots indexed by a hash of
// the socket's address. A slot is one 64-bit word: a valid bit, a 19-bit
// fingerprint of the socket (other bits of the same hash) and the low 44 bits
// of the arrival time. Each step is one load and one store of that word, so a
// slot is never seen half written:
//
// - `tcp_probe`: an in-order data segment writes the slot if it is empty. A
//   slot that holds this socket's fingerprint keeps its earlier time; a slot
//   held by another socket is left alone, and this socket's segment is not
//   timed.
// - `tcp_rcv_space_adjust`: if the slot holds this socket's fingerprint,
//   record now minus the arrival time and empty the slot.
// - `tcp_destroy_sock`: empty the slot if it holds this socket's fingerprint.
//
// Data that is not timed:
//
// - A segment whose slot is held by another socket, including when two
//   sockets that share a slot write it at once and one claim is lost. This is
//   about K / 2^19 of segments when K sockets hold a start time at once;
//   against an exact reference, 0.20-0.22% of samples were missed at 2000
//   concurrent request/response clients.
// - The new data in a partial duplicate (a retransmission that starts before
//   `rcv_nxt` and ends after it).
// - Data that does not pass `tcp_probe`, which fires only in ESTABLISHED:
//   TFO data on a SYN or SYN-ACK, data on the handshake's final ACK, and data
//   received in FIN_WAIT1 or FIN_WAIT2.
//
// When untimed data is followed by an in-order segment before the read, the
// sample starts at that segment and is shorter than the data's wait.
//
// Samples that are wrong in other ways:
//
// - An in-order segment that started a sample and was then dropped (checksum
//   or PAWS failure, zero receive window, receive memory exhausted) leaves
//   its start time, and the next read records the time since the dropped
//   segment, which includes the retransmission delay. This is the same if
//   the socket has moved to FIN_WAIT1 or FIN_WAIT2 before the retransmission
//   arrives.
// - Two sockets share both the slot and the fingerprint.
// - The wait exceeds 2^44 ns (4.9 hours).
//
// An MPTCP subflow, whose data the MPTCP socket consumes without
// `tcp_rcv_space_adjust`, holds its slot until it is destroyed.
//
// The `tp_btf` and `raw_tp` programs run the same logic; they differ in how
// they read kernel structures.

#include <vmlinux.h>
#include "../../../agent/bpf/helpers.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_endian.h>
#include <bpf/bpf_tracing.h>

#define HISTOGRAM_BUCKETS HISTOGRAM_BUCKETS_POW_3
#define HISTOGRAM_POWER 3

// 2^19 slots x 8 bytes = 4 MiB.
#define SLOT_BITS 19
#define SLOTS (1 << SLOT_BITS)

#define SLOT_VALID (1ULL << 63)
#define FP_BITS 19
#define FP_SHIFT 44
#define FP_MASK ((1ULL << FP_BITS) - 1)
#define TS_MASK ((1ULL << FP_SHIFT) - 1)

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(max_entries, SLOTS);
    __type(key, u32);
    __type(value, u64);
} slots SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS);
} latency SEC(".maps");

// The murmur3 64-bit finalizer. Socket addresses are slab objects at a fixed
// stride, and this spreads them over all 64 bits; the slot index is the top
// SLOT_BITS and the fingerprint the FP_BITS below them.
static __always_inline u64 mix(u64 x) {
    x ^= x >> 33;
    x *= 0xff51afd7ed558ccdULL;
    x ^= x >> 33;
    x *= 0xc4ceb9fe1a85ec53ULL;
    x ^= x >> 33;
    return x;
}

struct slot_ref {
    volatile u64* word;
    u64 fp;
};

static __always_inline struct slot_ref slot_of(u64 addr) {
    struct slot_ref ref = {};
    u64 h = mix(addr);
    u32 idx = h >> (64 - SLOT_BITS);

    ref.word = bpf_map_lookup_elem(&slots, &idx);
    ref.fp = (h >> (64 - SLOT_BITS - FP_BITS)) & FP_MASK;

    return ref;
}

// The socket's address as an integer, in a `tp_btf` program, where the
// verifier does not allow arithmetic on the pointer argument itself. The
// socket is the first argument of all three tracepoints.
static __always_inline u64 sock_addr_btf(u64* ctx) {
    u64 addr = 0;

    bpf_probe_read_kernel(&addr, sizeof(addr), &ctx[0]);
    return addr;
}

static __always_inline bool owns(u64 w, u64 fp) {
    return (w & SLOT_VALID) && ((w >> FP_SHIFT) & FP_MASK) == fp;
}

static __always_inline void claim(struct slot_ref ref) {
    if (!ref.word) {
        return;
    }

    if (*ref.word & SLOT_VALID) {
        return;
    }

    *ref.word = SLOT_VALID | (ref.fp << FP_SHIFT) | (bpf_ktime_get_ns() & TS_MASK);
}

static __always_inline void finish(struct slot_ref ref) {
    if (!ref.word) {
        return;
    }

    u64 w = *ref.word;

    if (!owns(w, ref.fp)) {
        return;
    }

    u64 wait = (bpf_ktime_get_ns() - w) & TS_MASK;

    histogram_incr(&latency, HISTOGRAM_POWER, wait);

    *ref.word = 0;
}

static __always_inline void release(struct slot_ref ref) {
    if (ref.word && owns(*ref.word, ref.fp)) {
        *ref.word = 0;
    }
}

// The first 16 bytes of a TCP header, in network byte order (RFC 9293).
struct tcp_head {
    __be16 source;
    __be16 dest;
    __be32 seq;
    __be32 ack_seq;
    u8 doff_res;
    u8 flags;
    __be16 window;
};

// Whether the segment carries data and is the next in order. A pure ACK has
// no data for the application to read. A segment whose sequence number is not
// `rcv_nxt` is a whole or partial duplicate, out of order, or beyond the
// window; a partial duplicate's new data is not timed. An in-order segment
// the kernel then drops passes this test.
static __always_inline bool starts_sample(u32 len, unsigned char* data, u32 rcv_nxt) {
    struct tcp_head th = {};

    if (bpf_probe_read_kernel(&th, sizeof(th), data)) {
        return false;
    }

    /* `doff * 4` means `__tcp_hdrlen` */
    return len > (u32)(th.doff_res >> 4) * 4 && bpf_ntohl(th.seq) == rcv_nxt;
}

/*
 * tp_btf (5.9+ for bpf_skc_to_tcp_sock)
 */

SEC("tp_btf/tcp_probe")
int BPF_PROG(tcp_probe_btf, struct sock* sk, struct sk_buff* skb) {
    struct tcp_sock* tp = bpf_skc_to_tcp_sock(sk);

    // tcp_probe fires before the segment is processed, so `rcv_nxt` is still
    // the sequence number the next in-order segment carries.
    if (!tp || !starts_sample(skb->len, skb->data, tp->rcv_nxt)) {
        return 0;
    }

    claim(slot_of(sock_addr_btf(ctx)));
    return 0;
}

SEC("tp_btf/tcp_rcv_space_adjust")
int BPF_PROG(tcp_rcv_space_adjust_btf, struct sock* sk) {
    finish(slot_of(sock_addr_btf(ctx)));
    return 0;
}

SEC("tp_btf/tcp_destroy_sock")
int BPF_PROG(tcp_destroy_sock_btf, struct sock* sk) {
    release(slot_of(sock_addr_btf(ctx)));
    return 0;
}

/*
 * raw_tp
 */

SEC("raw_tp/tcp_probe")
int BPF_PROG(tcp_probe_raw, struct sock* sk, struct sk_buff* skb) {
    struct tcp_sock* tp = (struct tcp_sock*)sk;

    if (!starts_sample(BPF_CORE_READ(skb, len), BPF_CORE_READ(skb, data),
                       BPF_CORE_READ(tp, rcv_nxt))) {
        return 0;
    }

    claim(slot_of((u64)sk));
    return 0;
}

SEC("raw_tp/tcp_rcv_space_adjust")
int BPF_PROG(tcp_rcv_space_adjust_raw, struct sock* sk) {
    finish(slot_of((u64)sk));
    return 0;
}

SEC("raw_tp/tcp_destroy_sock")
int BPF_PROG(tcp_destroy_sock_raw, struct sock* sk) {
    release(slot_of((u64)sk));
    return 0;
}

char LICENSE[] SEC("license") = "GPL";
