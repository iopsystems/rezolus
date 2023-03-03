// SPDX-License-Identifier: GPL-2.0
// Copyright (c) 2020 Anton Protopopov
// Copyright (c) 2023 IOP Systems, Inc.
//
// Based on syscount(8) from BCC by Sasha Goldshtein

#include "../../../../common/bpf/vmlinux.h"
#include "../../../../common/bpf/histogram.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_tracing.h>
#include <bpf/bpf_core_read.h>

struct {
	__uint(type, BPF_MAP_TYPE_HASH);
	__uint(max_entries, 65536);
	__type(key, u32);
	__type(value, u64);
} start SEC(".maps");

struct {
	__uint(type, BPF_MAP_TYPE_PERCPU_ARRAY);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, 1);
} count SEC(".maps");

struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, 7424);
} latency SEC(".maps");

SEC("tracepoint/syscalls/sys_enter_write")
int sys_enter_write(struct trace_event_sys_enter_write *args)
{
	u64 id = bpf_get_current_pid_tgid();
	u32 tid = id;
	u64 ts;

	ts = bpf_ktime_get_ns();
	bpf_map_update_elem(&start, &tid, &ts, 0);
	return 0;
}

SEC("tracepoint/syscalls/sys_exit_write")
int sys_exit_write(struct trace_event_sys_exit_write *args)
{
	u64 id = bpf_get_current_pid_tgid();
	u32 tid = id;
	u64 *start_ts, lat = 0, idx, *cnt;

	idx = 0;
	cnt = bpf_map_lookup_elem(&count, &idx);

	if (cnt) {
		__sync_fetch_and_add(cnt, 1);
	}

	start_ts = bpf_map_lookup_elem(&start, &tid);
	if (start_ts) {
		lat = bpf_ktime_get_ns() - *start_ts;
		idx = value_to_index(lat);
		cnt = bpf_map_lookup_elem(&latency, &idx);

		if (cnt) {
			__sync_fetch_and_add(cnt, 1);
		}
	}

	return 0;
}

char LICENSE[] SEC("license") = "GPL";
