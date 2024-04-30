// SPDX-License-Identifier: GPL-2.0
// Copyright (c) 2020 Anton Protopopov
// Copyright (c) 2023 The Rezolus Authors
//
// Based on syscount(8) from BCC by Sasha Goldshtein

// NOTICE: this file is based off `syscount.bpf.c` from the BCC project
// <https://github.com/iovisor/bcc/> and has been modified for use within
// Rezolus.

// This BPF program tracks syscall enter and exit to provide metrics about
// syscall counts and latencies.

#include <vmlinux.h>
#include "../../../common/bpf/histogram.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_tracing.h>
#include <bpf/bpf_core_read.h>

#define COUNTER_GROUP_WIDTH 8
#define HISTOGRAM_POWER 7
#define MAX_CPUS 1024
#define MAX_SYSCALL_ID 1024
#define MAX_PID 4194304

struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(max_entries, MAX_PID);
	__type(key, u32);
	__type(value, u64);
} start SEC(".maps");

// counters for syscalls
// 0 - total
// 1..COUNTER_GROUP_WIDTH - grouped syscalls defined in userspace in the
//                          `syscall_lut` map
struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(map_flags, BPF_F_MMAPABLE);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, MAX_CPUS * COUNTER_GROUP_WIDTH);
} counters SEC(".maps");

// tracks the latency distribution of all syscalls
struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(map_flags, BPF_F_MMAPABLE);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, HISTOGRAM_BUCKETS_POW_7);
} total_latency SEC(".maps");

struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(map_flags, BPF_F_MMAPABLE);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, HISTOGRAM_BUCKETS_POW_7);
} read_latency SEC(".maps");

struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(map_flags, BPF_F_MMAPABLE);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, HISTOGRAM_BUCKETS_POW_7);
} write_latency SEC(".maps");

struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(map_flags, BPF_F_MMAPABLE);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, HISTOGRAM_BUCKETS_POW_7);
} poll_latency SEC(".maps");

struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(map_flags, BPF_F_MMAPABLE);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, HISTOGRAM_BUCKETS_POW_7);
} lock_latency SEC(".maps");

struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(map_flags, BPF_F_MMAPABLE);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, HISTOGRAM_BUCKETS_POW_7);
} time_latency SEC(".maps");

struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(map_flags, BPF_F_MMAPABLE);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, HISTOGRAM_BUCKETS_POW_7);
} sleep_latency SEC(".maps");

struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(map_flags, BPF_F_MMAPABLE);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, HISTOGRAM_BUCKETS_POW_7);
} socket_latency SEC(".maps");

// provides a lookup table from syscall id to a counter index offset
struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(map_flags, BPF_F_MMAPABLE);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, MAX_SYSCALL_ID);
} syscall_lut SEC(".maps");

SEC("tracepoint/raw_syscalls/sys_enter")
int sys_enter(struct trace_event_raw_sys_enter *args)
{
	u64 id = bpf_get_current_pid_tgid();
	u32 tid = id;
	u64 ts;

	ts = bpf_ktime_get_ns();
	bpf_map_update_elem(&start, &tid, &ts, 0);
	return 0;
}

SEC("tracepoint/raw_syscalls/sys_exit")
int sys_exit(struct trace_event_raw_sys_exit *args)
{
	u64 id = bpf_get_current_pid_tgid();
	u64 *start_ts, lat = 0;
	u32 tid = id;

	u64 *cnt;
	u32 idx;

	if (args->id < 0) {
		return 0;
	}

	u32 syscall_id = args->id;

	// update the total counter
	idx = COUNTER_GROUP_WIDTH * bpf_get_smp_processor_id();
	cnt = bpf_map_lookup_elem(&counters, &idx);

	if (cnt) {
		__sync_fetch_and_add(cnt, 1);
	}

	// for some syscalls, we track counts by "family" of syscall. check the
	// lookup table and increment the appropriate counter
	idx = 0;
	if (syscall_id < MAX_SYSCALL_ID) {
		u32 *counter_offset = bpf_map_lookup_elem(&syscall_lut, &syscall_id);

		if (counter_offset && *counter_offset && *counter_offset < COUNTER_GROUP_WIDTH) {
			idx = COUNTER_GROUP_WIDTH * bpf_get_smp_processor_id() + ((u32)*counter_offset);
			cnt = bpf_map_lookup_elem(&counters, &idx);

			if (cnt) {
				__sync_fetch_and_add(cnt, 1);
			}
		} else {
			// syscall counter offset was outside of the expected range
			// this indicates that the LUT contains invalid values
		}
	} else {
		// syscall id was out of the expected range
	}

	// lookup the start time
	start_ts = bpf_map_lookup_elem(&start, &tid);

	// possible we missed the start
	if (!start_ts || *start_ts == 0) {
		return 0;
	}

	// calculate the latency
	lat = bpf_ktime_get_ns() - *start_ts;

	// clear the start timestamp
	*start_ts = 0;

	// calculate the histogram index for this latency value
	idx = value_to_index(lat, HISTOGRAM_POWER);

	// update the total latency histogram
	cnt = bpf_map_lookup_elem(&total_latency, &idx);

	if (cnt) {
		__sync_fetch_and_add(cnt, 1);
	}

	// increment latency histogram for the syscall family
	if (syscall_id < MAX_SYSCALL_ID) {
		u32 *counter_offset = bpf_map_lookup_elem(&syscall_lut, &syscall_id);

		if (!counter_offset || !*counter_offset || *counter_offset >= COUNTER_GROUP_WIDTH) {
			return 0;
		}

		// nested if-else binary search. finds the correct histogram in 3 branches
		if (*counter_offset < 5) {
			if (*counter_offset < 3) {
				if (*counter_offset == 1) {
					cnt = bpf_map_lookup_elem(&read_latency, &idx);
				} else {
					cnt = bpf_map_lookup_elem(&write_latency, &idx);
				}
			} else if (*counter_offset == 3) {
				cnt = bpf_map_lookup_elem(&poll_latency, &idx);
			} else {
				cnt = bpf_map_lookup_elem(&lock_latency, &idx);
			}
		} else if (*counter_offset < 7) {
			if (*counter_offset == 5) {
				cnt = bpf_map_lookup_elem(&time_latency, &idx);
			} else {
				cnt = bpf_map_lookup_elem(&sleep_latency, &idx);
			}
		} else {
			cnt = bpf_map_lookup_elem(&socket_latency, &idx);
		}

		if (cnt) {
			__sync_fetch_and_add(cnt, 1);
		}
	}

	return 0;
}

char LICENSE[] SEC("license") = "GPL";