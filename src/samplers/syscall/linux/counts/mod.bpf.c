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

#define COUNTER_GROUP_WIDTH 16
#define MAX_CPUS 1024
#define MAX_SYSCALL_ID 1024
#define MAX_PID 4194304

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
	u64 *cnt;
	u32 offset, idx;

	if (args->id < 0) {
		return 0;
	}

	u32 syscall_id = args->id;
	offset = COUNTER_GROUP_WIDTH * bpf_get_smp_processor_id();

	// update the total counter
	cnt = bpf_map_lookup_elem(&counters, &offset);

	if (cnt) {
		__atomic_fetch_add(cnt, 1, __ATOMIC_RELAXED);
	}

	// for some syscalls, we track counts by "family" of syscall. check the
	// lookup table and increment the appropriate counter
	idx = 0;
	if (syscall_id < MAX_SYSCALL_ID) {
		u32 *counter_offset = bpf_map_lookup_elem(&syscall_lut, &syscall_id);

		if (counter_offset && *counter_offset && *counter_offset < COUNTER_GROUP_WIDTH) {
			idx = offset + ((u32)*counter_offset);
			cnt = bpf_map_lookup_elem(&counters, &idx);

			if (cnt) {
				__atomic_fetch_add(cnt, 1, __ATOMIC_RELAXED);
			}
		} else {
			// syscall counter offset was outside of the expected range
			// this indicates that the LUT contains invalid values
		}
	} else {
		// syscall id was out of the expected range
	}

	return 0;
}

char LICENSE[] SEC("license") = "GPL";
