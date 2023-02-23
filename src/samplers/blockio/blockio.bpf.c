// SPDX-License-Identifier: GPL-2.0
// Copyright (c) 2020 Wenbo Zhang
// Copyright (c) 2023 IOP Systems, Inc.

#include "../../../vmlinux.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_tracing.h>
#include "../../common/bpf.h"

extern int LINUX_KERNEL_VERSION __kconfig;

struct {
	__uint(type, BPF_MAP_TYPE_HASH);
	__uint(max_entries, 65536);
	__type(key, struct request *);
	__type(value, u64);
} start SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, 496);
} latency SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, 496);
} size SEC(".maps");

static int __always_inline trace_rq_start(struct request *rq, int issue)
{
	u64 ts;

	ts = bpf_ktime_get_ns();

	bpf_map_update_elem(&start, &rq, &ts, 0);
	return 0;
}

static int handle_block_rq_insert(__u64 *ctx)
{
	// TODO(bmartin): kernel version detection does not seem
	// to be super reliable beyond minor version.

	// tracepoint argument list changed in 5.10.137
	if (LINUX_KERNEL_VERSION >= KERNEL_VERSION(5, 10, 0)) {
		return trace_rq_start((void *)ctx[0], false);
	} else {
		return trace_rq_start((void *)ctx[1], false);
	}
}

static int handle_block_rq_issue(__u64 *ctx)
{
	// TODO(bmartin): kernel version detection does not seem
	// to be super reliable beyond minor version.

	// tracepoint argument list changed in 5.10.137
	if (LINUX_KERNEL_VERSION >= KERNEL_VERSION(5, 10, 0)) {
		return trace_rq_start((void *)ctx[0], true);
	} else {
		return trace_rq_start((void *)ctx[1], true);
	}
}

static int handle_block_rq_complete(struct request *rq, int error, unsigned int nr_bytes)
{
	u64 slot, *tsp, ts = bpf_ktime_get_ns();
	u64 delta_ns, *cnt;

	tsp = bpf_map_lookup_elem(&start, &rq);
	if (!tsp)
		return 0;

	if (*tsp >= ts) {
		bpf_map_delete_elem(&start, &rq);
		return 0;
	}

	delta_ns = ts - *tsp;

	// update latency histogram
	u32 idx = value_to_index(delta_ns);
	cnt = bpf_map_lookup_elem(&latency, &idx);

	if (cnt) {
		__sync_fetch_and_add(cnt, 1);
    }

    // update size histogram
    idx = value_to_index(nr_bytes);
    cnt = bpf_map_lookup_elem(&size, &idx);

    if (cnt) {
		__sync_fetch_and_add(cnt, 1);
    }

    bpf_map_delete_elem(&start, &rq);
}

SEC("tp_btf/block_rq_insert")
int block_rq_insert_btf(u64 *ctx)
{
	return handle_block_rq_insert(ctx);
}

SEC("tp_btf/block_rq_issue")
int block_rq_issue_btf(u64 *ctx)
{
	return handle_block_rq_issue(ctx);
}

SEC("tp_btf/block_rq_complete")
int BPF_PROG(block_rq_complete_btf, struct request *rq, int error, unsigned int nr_bytes)
{
	return handle_block_rq_complete(rq, error, nr_bytes);
}

SEC("raw_tp/block_rq_insert")
int BPF_PROG(block_rq_insert)
{
	return handle_block_rq_insert(ctx);
}

SEC("raw_tp/block_rq_issue")
int BPF_PROG(block_rq_issue)
{
	return handle_block_rq_issue(ctx);
}

SEC("raw_tp/block_rq_complete")
int BPF_PROG(block_rq_complete, struct request *rq, int error, unsigned int nr_bytes)
{
	return handle_block_rq_complete(rq, error, nr_bytes);
}

char LICENSE[] SEC("license") = "GPL";