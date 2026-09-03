// SPDX-License-Identifier: GPL-2.0
// Copyright (c) 2020 Wenbo Zhang
// Copyright (c) 2023 The Rezolus Authors

#include <vmlinux.h>
#include "../../../agent/bpf/core_fixes.h"
#include "../../../agent/bpf/helpers.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_tracing.h>

#define COUNTER_GROUP_WIDTH 8
#define HISTOGRAM_BUCKETS HISTOGRAM_BUCKETS_POW_3
#define HISTOGRAM_POWER 3
#define MAX_CPUS 1024

#define REQ_OP_BITS 8
#define REQ_OP_MASK ((1 << REQ_OP_BITS) - 1)
#define REQ_FLAG_BITS 24

#define REQ_OP_READ 0
#define REQ_OP_WRITE 1
#define REQ_OP_FLUSH 2
#define REQ_OP_DISCARD 3

// Upper bound on a request phase we are willing to believe. The block layer's
// own request timeout is 30s, so nothing still in flight at 60s is going to
// complete and be reported here anyway; a device hung that long shows up in
// `blockio_errors`, not as a latency sample. The stamps we read come from the
// kernel and are set on the request's own timeline, so staleness is not the
// concern it was for the old side-map -- this is a sanity ceiling only.
#define MAX_PLAUSIBLE_SPAN_NS (60ULL * 1000000000ULL)

// Each phase's latency histogram, one per op class. The op label distinguishes
// like entities within a family; the three families (device/queue/total) are
// separate acquisition groups on the userspace side (see stats.rs).
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS);
} read_device_latency SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS);
} write_device_latency SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS);
} flush_device_latency SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS);
} discard_device_latency SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS);
} read_queue_latency SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS);
} write_queue_latency SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS);
} flush_queue_latency SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS);
} discard_queue_latency SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS);
} read_total_latency SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS);
} write_total_latency SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS);
} flush_total_latency SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS);
} discard_total_latency SEC(".maps");

// A phase we are willing to report: the stamp was actually set, it does not
// post-date the completion, and the span is not absurd. See MAX_PLAUSIBLE_SPAN_NS.
static bool __always_inline plausible_span(u64 begin, u64 end) {
    return begin != 0 && begin <= end && (end - begin) < MAX_PLAUSIBLE_SPAN_NS;
}

static u32 __always_inline rq_op(struct request* rq) {
    return BPF_CORE_READ(rq, cmd_flags) & REQ_OP_MASK;
}

static void __always_inline record_device_latency(u32 op, u64 delta) {
    u32 idx = value_to_index(delta, HISTOGRAM_POWER);

    switch (op) {
    case REQ_OP_READ:
        array_incr(&read_device_latency, idx);
        break;
    case REQ_OP_WRITE:
        array_incr(&write_device_latency, idx);
        break;
    case REQ_OP_FLUSH:
        array_incr(&flush_device_latency, idx);
        break;
    case REQ_OP_DISCARD:
        array_incr(&discard_device_latency, idx);
        break;
    }
}

static void __always_inline record_queue_latency(u32 op, u64 delta) {
    u32 idx = value_to_index(delta, HISTOGRAM_POWER);

    switch (op) {
    case REQ_OP_READ:
        array_incr(&read_queue_latency, idx);
        break;
    case REQ_OP_WRITE:
        array_incr(&write_queue_latency, idx);
        break;
    case REQ_OP_FLUSH:
        array_incr(&flush_queue_latency, idx);
        break;
    case REQ_OP_DISCARD:
        array_incr(&discard_queue_latency, idx);
        break;
    }
}

static void __always_inline record_total_latency(u32 op, u64 delta) {
    u32 idx = value_to_index(delta, HISTOGRAM_POWER);

    switch (op) {
    case REQ_OP_READ:
        array_incr(&read_total_latency, idx);
        break;
    case REQ_OP_WRITE:
        array_incr(&write_total_latency, idx);
        break;
    case REQ_OP_FLUSH:
        array_incr(&flush_total_latency, idx);
        break;
    case REQ_OP_DISCARD:
        array_incr(&discard_total_latency, idx);
        break;
    }
}

// All three phases come from the kernel's own per-request timestamps, read at
// completion -- no side map, no insert/issue probes. The block layer already
// stamps each request on its own timeline:
//
//   start_time_ns     request init          (~enters the queue)
//   io_start_time_ns  dispatched to device  (blk_mq_start_request)
//   now (completion)
//
// so device = now - io_start, queue = io_start - start, total = now - start.
// This replaced a hash keyed by `struct request *` whose insert+lookup+delete
// per IO was the sampler's dominant cost -- memory-stall-bound under cross-core
// contention (see docs/journal/2026-09-03-blockio-latency-rq-fields.md).
//
// Two in-handler guards stand in for what the map used to provide implicitly:
//
//   * `nr_bytes == __data_len` -- block_rq_complete can fire once per PARTIAL
//     completion (blk_update_request), and the old map deduped by deleting on
//     the first. At the tracepoint __data_len still holds the bytes remaining,
//     so the FINAL completion is exactly the one whose chunk (nr_bytes) equals
//     the remainder; a partial has nr_bytes < __data_len. Recording only the
//     final completion is the stateless dedup.
//
//   * plausible_span()'s begin != 0 -- io_start_time_ns is populated only when a
//     blk-stat consumer (wbt/iostat/iocost) is active on the queue; that is the
//     default fleet-wide, but a device with none leaves it 0. We then record no
//     device/queue for that request (rather than a bogus now-0 span) while total
//     still lands, since start_time_ns is set far less conditionally. Graceful
//     degradation, self-correcting per request.
static int __always_inline handle_block_rq_complete(struct request* rq, int error,
                                                    unsigned int nr_bytes) {
    u64 start, io_start, ts = bpf_ktime_get_ns();
    u32 op;

    // Only the final completion of a request counts. See the header comment.
    if (nr_bytes != BPF_CORE_READ(rq, __data_len)) {
        return 0;
    }

    start = BPF_CORE_READ(rq, start_time_ns);
    io_start = BPF_CORE_READ(rq, io_start_time_ns);
    op = rq_op(rq);

    // device: dispatch -> completion. Skipped when io_start is unpopulated.
    if (plausible_span(io_start, ts)) {
        record_device_latency(op, ts - io_start);
    }

    // queue: init -> dispatch. Needs both stamps; io_start >= start always
    // holds when both are set (dispatch cannot precede init).
    if (io_start != 0 && plausible_span(start, io_start)) {
        record_queue_latency(op, io_start - start);
    }

    // total: init -> completion. start_time_ns is the least conditional stamp,
    // so this lands even where the device/queue phases cannot.
    if (plausible_span(start, ts)) {
        record_total_latency(op, ts - start);
    }

    return 0;
}

// tp_btf and raw_tp twins share the handler above; the unused variant is
// disabled at load time based on whether the kernel has its own BTF (see
// disabled_programs in mod.rs).

SEC("tp_btf/block_rq_complete")
int BPF_PROG(block_rq_complete_btf, struct request* rq, int error, unsigned int nr_bytes) {
    return handle_block_rq_complete(rq, error, nr_bytes);
}

SEC("raw_tp/block_rq_complete")
int BPF_PROG(block_rq_complete_raw, struct request* rq, int error, unsigned int nr_bytes) {
    return handle_block_rq_complete(rq, error, nr_bytes);
}

char LICENSE[] SEC("license") = "GPL";
