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

// Upper bound on a request phase we are willing to believe.
//
// `insert` is the one stamp a handler may read without having written it in
// this request's lifetime, so it is the one that can be stale. The pre-split
// code was self-healing -- both start hooks did an unconditional update, so a
// leftover stamp from a previous life of a recycled `struct request *` was
// always overwritten before it could be read. The split reads instead of
// writing, so a leaked entry (a request inserted before the complete program
// was attached, or during device teardown) can survive to meet a later request
// at the same recycled address. Its `insert` would then be an age, not a wait
// -- and `value_to_index` has a bucket for it, so it would surface as the max
// of exactly the histogram someone opens to diagnose saturation.
//
// We cannot tell a stale stamp from a real one with a pointer key, so bound it
// instead. The block layer's own request timeout is 30s, so nothing still in
// flight at 60s is going to complete and be reported here anyway; a device
// hung that long shows up in `blockio_errors`, not as a latency sample.
#define MAX_PLAUSIBLE_SPAN_NS (60ULL * 1000000000ULL)

// Both stamps for one in-flight request. `insert` is when the request entered
// the scheduler queue, `issue` when the driver began servicing it; the gap
// between them is queue residency, and it is the component that grows under
// saturation. Keeping both in ONE hash entry (rather than a second map keyed
// by the same pointer) costs one lookup per hook instead of two, and keeps the
// lifetime bookkeeping in one place -- one insert, one delete per request.
//
// A zero stamp means "that phase was never observed":
//   - `insert == 0`: the request bypassed the scheduler (blk-mq with the
//     `none` elevator issues directly), so it had no queue phase to measure.
//     Rather than report a fabricated zero wait, we record nothing.
//   - `issue == 0`: complete fired without an issue, so the only start we have
//     is the insert.
struct rq_stamps {
    u64 insert;
    u64 issue;
};

struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, 65536);
    __type(key, struct request*);
    __type(value, struct rq_stamps);
} start SEC(".maps");

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

// Classify a request's op once, for whichever histogram family is recording.
// A phase we are willing to report: the stamp was actually set, it does not
// post-date the completion, and the span is not absurd. See
// MAX_PLAUSIBLE_SPAN_NS.
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

// The request entered the scheduler queue. Starts a fresh pair of stamps: a
// re-inserted request begins a new queue phase, and any stale `issue` from a
// previous life of this pointer must not survive into it.
static int __always_inline trace_rq_insert(struct request* rq) {
    struct rq_stamps stamps = {
        .insert = bpf_ktime_get_ns(),
        .issue = 0,
    };

    bpf_map_update_elem(&start, &rq, &stamps, 0);
    return 0;
}

// The driver began servicing the request. Closes the queue phase and opens the
// device phase. `insert` is deliberately NOT cleared here -- complete needs it
// to measure the end-to-end phase -- so the "already billed" marker is
// `issue != 0` instead. A requeue that re-inserts resets both fields and so
// starts a genuinely new queue phase; a reissue without a re-insert finds
// `issue != 0` and bills nothing further.
static int __always_inline trace_rq_issue(struct request* rq) {
    u64 insert, ts = bpf_ktime_get_ns();
    struct rq_stamps* stamps = bpf_map_lookup_elem(&start, &rq);

    if (!stamps) {
        // No insert was seen: the request went straight to the driver. Record
        // the device phase only, leaving `insert` zero so complete knows there
        // is no queue phase for this request.
        struct rq_stamps fresh = {
            .insert = 0,
            .issue = ts,
        };

        bpf_map_update_elem(&start, &rq, &fresh, 0);
        return 0;
    }

    // Hoist to a local: the map value is not volatile, and clang otherwise
    // re-loads it after the bpf_probe_read_kernel() inside rq_op(), so the
    // guard below would not cover the value actually subtracted.
    insert = stamps->insert;

    if (stamps->issue == 0 && plausible_span(insert, ts)) {
        record_queue_latency(rq_op(rq), ts - insert);
    }

    stamps->issue = ts;

    return 0;
}

static int __always_inline handle_block_rq_complete(struct request* rq, int error,
                                                    unsigned int nr_bytes) {
    u64 device_begin, total_begin, ts = bpf_ktime_get_ns();
    struct rq_stamps* stamps;
    u32 op;

    stamps = bpf_map_lookup_elem(&start, &rq);
    if (!stamps) {
        return 0;
    }

    // The device phase runs from the issue when we saw one, and from the
    // insert otherwise -- the same value this histogram reported before the
    // phases were split out, when insert and issue shared one stamp and issue
    // simply overwrote it. This equivalence is what lets a reader treat a
    // pre-rename `blockio_latency` series as `blockio_device_latency`.
    device_begin = stamps->issue != 0 ? stamps->issue : stamps->insert;

    // The end-to-end phase runs from the earliest stamp we hold. For a request
    // that never queued, that is the issue, so total == device -- honest, and
    // not a fabricated queue phase.
    total_begin = stamps->insert != 0 ? stamps->insert : stamps->issue;

    op = rq_op(rq);

    if (plausible_span(device_begin, ts)) {
        record_device_latency(op, ts - device_begin);
    }

    if (plausible_span(total_begin, ts)) {
        record_total_latency(op, ts - total_begin);
    }

    bpf_map_delete_elem(&start, &rq);
    return 0;
}

// tp_btf and raw_tp twins share the handlers above; the unused variant is
// disabled at load time based on whether the kernel has its own BTF (see
// disabled_programs in mod.rs). The request pointer goes through
// block_rq_tp_request() because kernels before v5.11 pass a leading
// struct request_queue* argument to insert/issue.

SEC("tp_btf/block_rq_insert")
int BPF_PROG(block_rq_insert_btf) {
    return trace_rq_insert(block_rq_tp_request(ctx));
}

SEC("raw_tp/block_rq_insert")
int BPF_PROG(block_rq_insert_raw) {
    return trace_rq_insert(block_rq_tp_request(ctx));
}

SEC("tp_btf/block_rq_issue")
int BPF_PROG(block_rq_issue_btf) {
    return trace_rq_issue(block_rq_tp_request(ctx));
}

SEC("raw_tp/block_rq_issue")
int BPF_PROG(block_rq_issue_raw) {
    return trace_rq_issue(block_rq_tp_request(ctx));
}

SEC("tp_btf/block_rq_complete")
int BPF_PROG(block_rq_complete_btf, struct request* rq, int error, unsigned int nr_bytes) {
    return handle_block_rq_complete(rq, error, nr_bytes);
}

SEC("raw_tp/block_rq_complete")
int BPF_PROG(block_rq_complete_raw, struct request* rq, int error, unsigned int nr_bytes) {
    return handle_block_rq_complete(rq, error, nr_bytes);
}

char LICENSE[] SEC("license") = "GPL";
