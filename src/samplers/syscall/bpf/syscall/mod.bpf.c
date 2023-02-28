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
// #include "syscount.h"
// #include "maps.bpf.h"

// const volatile bool filter_cg = false;
// const volatile bool count_by_process = false;
// const volatile bool measure_latency = false;
// const volatile bool filter_failed = false;
// const volatile int filter_errno = false;
// const volatile pid_t filter_pid = 0;

// struct {
// 	__uint(type, BPF_MAP_TYPE_HASH);
// 	__uint(max_entries, MAX_ENTRIES);
// 	__type(key, u32);
// 	__type(value, u64);
// } start SEC(".maps");

struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, 512);
} counters SEC(".maps");

// SEC("tracepoint/raw_syscalls/sys_enter")
// int sys_enter(struct trace_event_raw_sys_enter *args)
// {
// 	u64 id = bpf_get_current_pid_tgid();
// 	u32 tid = id;
// 	u64 ts;

// 	ts = bpf_ktime_get_ns();
// 	bpf_map_update_elem(&start, &tid, &ts, 0);
// 	return 0;
// }

SEC("tracepoint/raw_syscalls/sys_exit")
int sys_exit(struct trace_event_raw_sys_exit *args)
{
	// u64 id = bpf_get_current_pid_tgid();
	// static const struct data_t zero;
	// pid_t pid = id >> 32;
	// struct data_t *val;
	// u64 *start_ts, lat = 0;
	// u32 tid = id;
	// u32 key;

	u64 *cnt;
	u32 idx;

	/* this happens when there is an interrupt */
	if (args->id == -1)
		return 0;

	// if (filter_pid && pid != filter_pid)
	// 	return 0;
	// if (filter_failed && args->ret >= 0)
	// 	return 0;
	// if (filter_errno && args->ret != -filter_errno)
	// 	return 0;

	// if (measure_latency) {
	// 	start_ts = bpf_map_lookup_elem(&start, &tid);
	// 	if (!start_ts)
	// 		return 0;
	// 	lat = bpf_ktime_get_ns() - *start_ts;
	// }

	// key = (count_by_process) ? pid : args->id;
	// val = bpf_map_lookup_or_try_init(&data, &tid, &zero);
	// if (val) {
	// 	__sync_fetch_and_add(&val->count, 1);
		// if (count_by_process)
		// 	save_proc_name(val);
		// if (measure_latency)
		// 	__sync_fetch_and_add(&val->total_ns, lat);
	// }

	idx = args->id;
	cnt = bpf_map_lookup_elem(&counters, &idx);

	if (cnt) {
		__sync_fetch_and_add(cnt, 1);
	}

	return 0;
}

char LICENSE[] SEC("license") = "GPL";
