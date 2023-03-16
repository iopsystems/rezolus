// SPDX-License-Identifier: GPL-2.0
// Copyright (c) 2021 Wenbo Zhang
// Copyright (c) 2023 IOP Systems, Inc.

#include "../../../common/bpf/vmlinux.h"
#include "../../../common/bpf/histogram.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_tracing.h>

struct {
	__uint(type, BPF_MAP_TYPE_PERCPU_ARRAY);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, 1);
} total SEC(".maps");

struct {
	__uint(type, BPF_MAP_TYPE_PERCPU_ARRAY);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, 1);
} miss SEC(".maps");

struct {
	__uint(type, BPF_MAP_TYPE_PERCPU_ARRAY);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, 1);
} mbd SEC(".maps");

struct {
	__uint(type, BPF_MAP_TYPE_PERCPU_ARRAY);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, 1);
} dirtied SEC(".maps");

SEC("fentry/add_to_page_cache_lru")
int BPF_PROG(fentry_add_to_page_cache_lru)
{
	u64 *cnt;
	u32 idx = 0;
	cnt = bpf_map_lookup_elem(&miss, &idx);

	if (cnt) {
		__sync_fetch_and_add(cnt, 1);
	}

	return 0;
}

SEC("fentry/mark_page_accessed")
int BPF_PROG(fentry_mark_page_accessed)
{
	u64 *cnt;
	u32 idx = 0;
	cnt = bpf_map_lookup_elem(&total, &idx);

	if (cnt) {
		__sync_fetch_and_add(cnt, 1);
	}

	return 0;
}

SEC("fentry/account_page_dirtied")
int BPF_PROG(fentry_account_page_dirtied)
{
	u64 *cnt;
	u32 idx = 0;
	cnt = bpf_map_lookup_elem(&dirtied, &idx);

	if (cnt) {
		__sync_fetch_and_add(cnt, 1);
	}

	return 0;
}

SEC("fentry/mark_buffer_dirty")
int BPF_PROG(fentry_mark_buffer_dirty)
{
	u64 *cnt;
	u32 idx = 0;
	cnt = bpf_map_lookup_elem(&mbd, &idx);

	if (cnt) {
		__sync_fetch_and_add(cnt, 1);
	}

	return 0;
}

SEC("kprobe/add_to_page_cache_lru")
int BPF_KPROBE(kprobe_add_to_page_cache_lru)
{
	u64 *cnt;
	u32 idx = 0;
	cnt = bpf_map_lookup_elem(&miss, &idx);

	if (cnt) {
		__sync_fetch_and_add(cnt, 1);
	}

	return 0;
}

SEC("kprobe/mark_page_accessed")
int BPF_KPROBE(kprobe_mark_page_accessed)
{
	u64 *cnt;
	u32 idx = 0;
	cnt = bpf_map_lookup_elem(&total, &idx);

	if (cnt) {
		__sync_fetch_and_add(cnt, 1);
	}

	return 0;
}

SEC("kprobe/account_page_dirtied")
int BPF_KPROBE(kprobe_account_page_dirtied)
{
	u64 *cnt;
	u32 idx = 0;
	cnt = bpf_map_lookup_elem(&dirtied, &idx);

	if (cnt) {
		__sync_fetch_and_add(cnt, 1);
	}

	return 0;
}

SEC("kprobe/folio_account_dirtied")
int BPF_KPROBE(kprobe_folio_account_dirtied)
{
	u64 *cnt;
	u32 idx = 0;
	cnt = bpf_map_lookup_elem(&dirtied, &idx);

	if (cnt) {
		__sync_fetch_and_add(cnt, 1);
	}

	return 0;
}

SEC("kprobe/mark_buffer_dirty")
int BPF_KPROBE(kprobe_mark_buffer_dirty)
{
	u64 *cnt;
	u32 idx = 0;
	cnt = bpf_map_lookup_elem(&mbd, &idx);

	if (cnt) {
		__sync_fetch_and_add(cnt, 1);
	}

	return 0;
}

char LICENSE[] SEC("license") = "GPL";