// SPDX-License-Identifier: GPL-2.0
// Copyright (c) 2020 Wenbo Zhang
// Copyright (c) 2023 The Rezolus Authors

#include <vmlinux.h>
#include "../../../agent/bpf/helpers.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_tracing.h>

extern int LINUX_KERNEL_VERSION __kconfig;

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

static int handle_block_rq_complete(struct request* rq, int error, unsigned int nr_bytes) {
    u64 delta, *tsp;
    u32 idx, op;
    unsigned int cmd_flags;

    cmd_flags = BPF_CORE_READ(rq, cmd_flags);

    op = cmd_flags & REQ_OP_MASK;

    if (op < COUNTER_GROUP_WIDTH / 2) {
        idx = COUNTER_GROUP_WIDTH * bpf_get_smp_processor_id() + op;
        array_incr(&counters, idx);

        idx = idx + COUNTER_GROUP_WIDTH / 2;
        array_add(&counters, idx, nr_bytes);

        idx = value_to_index(nr_bytes, HISTOGRAM_POWER);

        // increment per-operation size histogram
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

    return 0;
}

SEC("raw_tp/block_rq_complete")
int BPF_PROG(block_rq_complete, struct request* rq, int error, unsigned int nr_bytes) {
    return handle_block_rq_complete(rq, error, nr_bytes);
}

char LICENSE[] SEC("license") = "GPL";
