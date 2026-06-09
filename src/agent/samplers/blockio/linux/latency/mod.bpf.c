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

struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, 65536);
    __type(key, struct request*);
    __type(value, u64);
} start SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS);
} read_latency SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS);
} write_latency SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS);
} flush_latency SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS);
} discard_latency SEC(".maps");

static int __always_inline trace_rq_start(struct request* rq) {
    u64 ts = bpf_ktime_get_ns();

    bpf_map_update_elem(&start, &rq, &ts, 0);
    return 0;
}

static int __always_inline handle_block_rq_complete(struct request* rq, int error,
                                                    unsigned int nr_bytes) {
    u64 delta, *tsp, ts = bpf_ktime_get_ns();
    u32 idx, op;
    unsigned int cmd_flags;

    tsp = bpf_map_lookup_elem(&start, &rq);
    if (!tsp) {
        return 0;
    }

    cmd_flags = BPF_CORE_READ(rq, cmd_flags);
    op = cmd_flags & REQ_OP_MASK;

    if (*tsp <= ts) {
        delta = ts - *tsp;

        idx = value_to_index(delta, HISTOGRAM_POWER);

        switch (op) {
        case REQ_OP_READ:
            array_incr(&read_latency, idx);
            break;
        case REQ_OP_WRITE:
            array_incr(&write_latency, idx);
            break;
        case REQ_OP_FLUSH:
            array_incr(&flush_latency, idx);
            break;
        case REQ_OP_DISCARD:
            array_incr(&discard_latency, idx);
            break;
        }
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
    return trace_rq_start(block_rq_tp_request(ctx));
}

SEC("raw_tp/block_rq_insert")
int BPF_PROG(block_rq_insert_raw) {
    return trace_rq_start(block_rq_tp_request(ctx));
}

SEC("tp_btf/block_rq_issue")
int BPF_PROG(block_rq_issue_btf) {
    return trace_rq_start(block_rq_tp_request(ctx));
}

SEC("raw_tp/block_rq_issue")
int BPF_PROG(block_rq_issue_raw) {
    return trace_rq_start(block_rq_tp_request(ctx));
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
