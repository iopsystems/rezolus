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

static int handle_block_rq_complete(struct request* rq, int error, unsigned int nr_bytes) {
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

        // Error path: bucket non-OK completions by class. Falls inside
        // the same op-range guard so errors and requests stay in lockstep.
        if (error != BLK_STS_OK) {
            int cls = classify_status(error);
            idx = bpf_get_smp_processor_id() * ERR_BANK_WIDTH + op * ERR_BUCKETS + cls;
            array_incr(&errors, idx);
        }
    }

    return 0;
}

SEC("raw_tp/block_rq_complete")
int BPF_PROG(block_rq_complete, struct request* rq, int error, unsigned int nr_bytes) {
    return handle_block_rq_complete(rq, error, nr_bytes);
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
