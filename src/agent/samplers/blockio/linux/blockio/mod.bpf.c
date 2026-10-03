// SPDX-License-Identifier: GPL-2.0
// Copyright (c) 2020 Wenbo Zhang
// Copyright (c) 2023 The Rezolus Authors

// One program per block hook for the `blockio` sampler: `block_rq_complete`
// counts the request (ops, bytes, size, errors) and records its latency
// phases from the kernel's own per-request timestamps, and `block_rq_requeue`
// counts requeues. Each part is switched by read-only data written before
// load, so a part that is off is not in the loaded program. Before this the
// counts and the latencies were two samplers, `blockio_requests` and
// `blockio_latency`, with a program each on `block_rq_complete`, and each
// program on a hook pays its own dispatch
// (docs/journal/2026-10-03-one-program-per-hook.md).

#include <vmlinux.h>
#include "../../../agent/bpf/btf_read.h"
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

// The sampler's parts, the config options `requests` and `latency` (both on
// by default). With `requests` off the `block_rq_requeue` programs are not
// loaded either.
const volatile __u8 requests = 0;
const volatile __u8 latency = 0;

// Number of op buckets for the error / requeue paths.
#define OP_BUCKETS 4

// Error-class buckets, indexed in the order the userspace metric vec uses.
//   0 io          — BLK_STS_IOERR, BLK_STS_MEDIUM
//   1 timeout     — BLK_STS_TIMEOUT
//   2 nospc       — BLK_STS_NOSPC
//   3 target      — BLK_STS_TARGET, BLK_STS_NEXUS, BLK_STS_RESV_CONFLICT
//   4 protection  — BLK_STS_PROTECTION
//   5 unsupported — BLK_STS_NOTSUPP
//   6 other       — everything else
#define ERR_BUCKETS 7

// Per-CPU bank widths must be padded to whole 64-byte cachelines so they
// match the userspace counter reader (see bpf/counters.rs). 28 error
// slots round up to 32; 4 requeue slots round up to 8.
#define ERR_BANK_WIDTH 32
#define REQ_BANK_WIDTH 8

// blk_status_t values. From include/linux/blk_types.h.
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

// counters
// 0 - read ops
// 1 - write ops
// 2 - flush ops
// 3 - discard ops
// 4 - read bytes
// 5 - write bytes
// 6 - flush bytes
// 7 - discard bytes
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CPUS* COUNTER_GROUP_WIDTH);
} counters SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS);
} read_size SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS);
} write_size SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS);
} flush_size SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS);
} discard_size SEC(".maps");

// errors[cpu * ERR_BANK_WIDTH + op * ERR_BUCKETS + cls]
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CPUS* ERR_BANK_WIDTH);
} errors SEC(".maps");

// requeues[cpu * REQ_BANK_WIDTH + op]
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CPUS* REQ_BANK_WIDTH);
} requeues SEC(".maps");

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

// The requests part: ops, bytes, size and errors for the request's op.
static void __always_inline count_request(u32 op, int error, unsigned int nr_bytes) {
    u32 idx;

    if (op < COUNTER_GROUP_WIDTH / 2) {
        idx = COUNTER_GROUP_WIDTH * bpf_get_smp_processor_id() + op;
        array_incr(&counters, idx);

        idx = idx + COUNTER_GROUP_WIDTH / 2;
        array_add(&counters, idx, nr_bytes);

        // flush completions transfer no data (nr_bytes is always 0), so a
        // size observation would just pile zeros into the first bucket
        if (nr_bytes > 0) {
            idx = value_to_index(nr_bytes, HISTOGRAM_POWER);

            switch (op) {
            case REQ_OP_READ:
                array_incr(&read_size, idx);
                break;
            case REQ_OP_WRITE:
                array_incr(&write_size, idx);
                break;
            case REQ_OP_FLUSH:
                array_incr(&flush_size, idx);
                break;
            case REQ_OP_DISCARD:
                array_incr(&discard_size, idx);
                break;
            }
        }

        // Error path: bucket non-OK completions by class. Falls inside
        // the same op-range guard so errors and requests stay in lockstep.
        if (error != BLK_STS_OK) {
            int cls = classify_status(error);
            idx = bpf_get_smp_processor_id() * ERR_BANK_WIDTH + op * ERR_BUCKETS + cls;
            array_incr(&errors, idx);
        }
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
//
// `btf` is a compile-time constant: true in the tp_btf program, whose `rq` is
// a BTF pointer, so its fields are direct loads (see BTF_READ in btf_read.h).
static void __always_inline record_latencies(struct request* rq, u32 op, unsigned int nr_bytes,
                                             u64 ts, bool btf) {
    u64 start, io_start;

    // Only the final completion of a request counts. See the comment above.
    if (nr_bytes != BTF_READ(btf, rq, __data_len)) {
        return;
    }

    start = BTF_READ(btf, rq, start_time_ns);
    io_start = BTF_READ(btf, rq, io_start_time_ns);

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
}

static int __always_inline handle_block_rq_complete(struct request* rq, int error,
                                                    unsigned int nr_bytes, bool btf) {
    // the completion time first, as the separate latency program took it
    u64 ts = bpf_ktime_get_ns();
    u32 op = BTF_READ(btf, rq, cmd_flags) & REQ_OP_MASK;

    if (requests) {
        count_request(op, error, nr_bytes);
    }

    if (latency) {
        record_latencies(rq, op, nr_bytes, ts, btf);
    }

    return 0;
}

static int __always_inline handle_block_rq_requeue(struct request* rq) {
    unsigned int cmd_flags = BPF_CORE_READ(rq, cmd_flags);
    u32 op = cmd_flags & REQ_OP_MASK;
    if (op >= OP_BUCKETS)
        return 0;

    u32 idx = bpf_get_smp_processor_id() * REQ_BANK_WIDTH + op;
    array_incr(&requeues, idx);
    return 0;
}

// tp_btf and raw_tp twins share the handlers above; the unused variant is
// disabled at load time based on whether the kernel has its own BTF (see
// disabled_programs in mod.rs). The requeue request pointer goes through
// block_rq_tp_request() because kernels before v5.11 pass a leading
// struct request_queue* argument.

SEC("tp_btf/block_rq_complete")
int BPF_PROG(block_rq_complete_btf, struct request* rq, int error, unsigned int nr_bytes) {
    return handle_block_rq_complete(rq, error, nr_bytes, true);
}

SEC("raw_tp/block_rq_complete")
int BPF_PROG(block_rq_complete_raw, struct request* rq, int error, unsigned int nr_bytes) {
    return handle_block_rq_complete(rq, error, nr_bytes, false);
}

SEC("tp_btf/block_rq_requeue")
int BPF_PROG(block_rq_requeue_btf) {
    return handle_block_rq_requeue(block_rq_tp_request(ctx));
}

SEC("raw_tp/block_rq_requeue")
int BPF_PROG(block_rq_requeue_raw) {
    return handle_block_rq_requeue(block_rq_tp_request(ctx));
}

char LICENSE[] SEC("license") = "GPL";
