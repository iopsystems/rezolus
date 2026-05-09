// SPDX-License-Identifier: GPL-2.0
// Copyright (c) 2024 The Rezolus Authors

// Block IO error / requeue accounting.
//
// Attaches to two tracepoints:
//
//   block:block_rq_complete  — fires on every completion. We early-return
//                              when the status is BLK_STS_OK (the common
//                              case) so the hot path stays cheap. For
//                              terminal errors we bucket the blk_status_t
//                              into one of seven coarse classes and bump
//                              the (cpu, op, class) slot.
//
//   block:block_rq_requeue   — fires when the block layer puts a request
//                              back on the queue (driver couldn't complete
//                              it; SCSI EH / NVMe reset / multipath retry).
//                              Counts as "recovered" rather than terminal,
//                              so it lives in its own metric.
//
// We don't attach block:block_rq_error (kernel ≥5.18) — block_rq_complete
// with the early-return on OK is portable across older kernels and the
// extra branch on the hot path is one cmov.

#include <vmlinux.h>
#include "../../../agent/bpf/helpers.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_tracing.h>

#define MAX_CPUS 1024

#define REQ_OP_BITS 8
#define REQ_OP_MASK ((1 << REQ_OP_BITS) - 1)

#define REQ_OP_READ 0
#define REQ_OP_WRITE 1
#define REQ_OP_FLUSH 2
#define REQ_OP_DISCARD 3

// Number of op buckets we track. Other ops (e.g. zone management,
// driver-private) fall outside [0..3] and are dropped.
#define OP_BUCKETS 4

// Number of error-class buckets. Order matches the userspace metric
// vec — see stats.rs and mod.rs.
//   0 io          — BLK_STS_IOERR, BLK_STS_MEDIUM
//   1 timeout     — BLK_STS_TIMEOUT
//   2 nospc       — BLK_STS_NOSPC
//   3 target      — BLK_STS_TARGET, BLK_STS_NEXUS, BLK_STS_RESV_CONFLICT
//   4 protection  — BLK_STS_PROTECTION
//   5 unsupported — BLK_STS_NOTSUPP
//   6 other       — everything else (transport, resource, zone, offline, …)
#define ERR_BUCKETS 7

// Per-CPU bank widths must be padded to whole 64-byte cachelines so they
// match the userspace counter reader (see bpf/counters.rs). 28 error
// slots round up to 32 (4 cachelines × 8 u64); 4 requeue slots round up
// to 8 (1 cacheline). Slots beyond the live range stay at zero.
#define ERR_BANK_WIDTH 32
#define REQ_BANK_WIDTH 8

// blk_status_t values. From include/linux/blk_types.h. These values are
// kernel-internal but stable across the supported kernel range.
#define BLK_STS_OK 0
#define BLK_STS_NOTSUPP 1
#define BLK_STS_TIMEOUT 2
#define BLK_STS_NOSPC 3
#define BLK_STS_TARGET 5
#define BLK_STS_NEXUS 6
#define BLK_STS_MEDIUM 7
#define BLK_STS_PROTECTION 8
#define BLK_STS_IOERR 10
#define BLK_STS_RESV_CONFLICT 19

// Per-CPU layout: cpu * ERR_BANK_WIDTH + op * ERR_BUCKETS + cls.
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CPUS* ERR_BANK_WIDTH);
} errors SEC(".maps");

// Per-CPU layout: cpu * REQ_BANK_WIDTH + op.
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CPUS* REQ_BANK_WIDTH);
} requeues SEC(".maps");

// Map blk_status_t → coarse error-class index.
static __always_inline int classify_status(int status) {
    switch (status) {
    case BLK_STS_IOERR:
    case BLK_STS_MEDIUM:
        return 0; // io
    case BLK_STS_TIMEOUT:
        return 1; // timeout
    case BLK_STS_NOSPC:
        return 2; // nospc
    case BLK_STS_TARGET:
    case BLK_STS_NEXUS:
    case BLK_STS_RESV_CONFLICT:
        return 3; // target
    case BLK_STS_PROTECTION:
        return 4; // protection
    case BLK_STS_NOTSUPP:
        return 5; // unsupported
    default:
        return 6; // other
    }
}

SEC("raw_tp/block_rq_complete")
int BPF_PROG(block_rq_complete, struct request* rq, int status, unsigned int nr_bytes) {
    // Hot path: early-return on success keeps the cost on healthy
    // workloads to a single comparison.
    if (status == BLK_STS_OK)
        return 0;

    unsigned int cmd_flags = BPF_CORE_READ(rq, cmd_flags);
    u32 op = cmd_flags & REQ_OP_MASK;
    if (op >= OP_BUCKETS)
        return 0;

    int cls = classify_status(status);

    u32 idx = bpf_get_smp_processor_id() * ERR_BANK_WIDTH + op * ERR_BUCKETS + cls;
    array_incr(&errors, idx);
    return 0;
}

SEC("raw_tp/block_rq_requeue")
int BPF_PROG(block_rq_requeue, struct request* rq) {
    unsigned int cmd_flags = BPF_CORE_READ(rq, cmd_flags);
    u32 op = cmd_flags & REQ_OP_MASK;
    if (op >= OP_BUCKETS)
        return 0;

    u32 idx = bpf_get_smp_processor_id() * REQ_BANK_WIDTH + op;
    array_incr(&requeues, idx);
    return 0;
}

char LICENSE[] SEC("license") = "GPL";
