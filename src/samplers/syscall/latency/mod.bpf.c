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

#include "../../../common/bpf/vmlinux.h"
#include "../../../common/bpf/histogram.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_tracing.h>
#include <bpf/bpf_core_read.h>

struct {
	__uint(type, BPF_MAP_TYPE_HASH);
	__uint(max_entries, 65536);
	__type(key, u32);
	__type(value, u64);
} start SEC(".maps");

#define SYSCALL_TOTAL 0
#define SYSCALL_READ 1
#define SYSCALL_WRITE 2

// counters for syscalls
// 0 - total
// 1 - read, recvfrom, readv, pread64, recvmsg, preadv, recvmmsg
// 2 - write, sendto, writev, pwrite64, sendmsg, pwritev, sendmmsg

struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(map_flags, BPF_F_MMAPABLE);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, 8192); // good for up to 1024 cores w/ 8 counters
} counters SEC(".maps");

// tracks the latency distribution of all syscalls
struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(map_flags, BPF_F_MMAPABLE);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, 7424);
} total_latency SEC(".maps");

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

	// this happens when there is an interrupt
	if (args->id == -1)
		return 0;

	// update the total counter
	idx = 8 * bpf_get_smp_processor_id() + SYSCALL_TOTAL;
	cnt = bpf_map_lookup_elem(&counters, &idx);

	if (cnt) {
		__sync_fetch_and_add(cnt, 1);
	}

	// check if: read, recvfrom, readv, pread64, recvmsg, preadv, recvmmsg
	if (args -> id == 0) ||
		(args -> id == 45) ||
		(args -> id == 19) ||
		(args -> id == 17) ||
		(args -> id == 47) ||
		(args -> id == 295) ||
		(args -> id == 299)
	{
		idx = 8 * bpf_get_smp_processor_id() + SYSCALL_READ;
		cnt = bpf_map_lookup_elem(&counters, &idx);

		if (cnt) {
			__sync_fetch_and_add(cnt, 1);
		}
	}

	// check if: write, sendto, writev, pwrite64, sendmsg, pwritev, sendmmsg
	if (args -> id == 1) ||
		(args -> id == 44) ||
		(args -> id == 20) ||
		(args -> id == 18) ||
		(args -> id == 46) ||
		(args -> id == 296) ||
		(args -> id == 307)
	{
		idx = 8 * bpf_get_smp_processor_id() + SYSCALL_WRITE;
		cnt = bpf_map_lookup_elem(&counters, &idx);

		if (cnt) {
			__sync_fetch_and_add(cnt, 1);
		}
	}

	// lookup the start time
	start_ts = bpf_map_lookup_elem(&start, &tid);

	// possible we missed the start
	if (!start_ts)
		return 0;

	lat = bpf_ktime_get_ns() - *start_ts;
	idx = value_to_index(lat);
	cnt = bpf_map_lookup_elem(&total_latency, &idx);

	if (cnt) {
		__sync_fetch_and_add(cnt, 1);
	}

	return 0;
}

char LICENSE[] SEC("license") = "GPL";