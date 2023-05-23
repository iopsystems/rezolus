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

// counters for syscalls
// 0 - total
// 1 - read, recvfrom, readv, pread64, recvmsg, preadv, recvmmsg
// 2 - write, sendto, writev, pwrite64, sendmsg, pwritev, sendmmsg
// 3 - poll, select, epoll_create, epoll_wait, epoll_ctl, epoll_pwait,
//     epoll_create1, ppoll, pselect6
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
	// This LUT works only on x86_64 and translates a sycall number to a counter
	// group. Current groups:
	// 0 - total
	// 1 - read related (read/recvfrom/readv/...)
	// 2 - write related (write/sendmsg/writev/...)
	// 3 - poll related (poll/select/epoll/...)
	// 4 - reserved for future use
	// 5 - reserved for future use
	// 6 - reserved for future use
	// 7 - reserved for future use
	static const u32 syscall_lookup[512] = {
		1, 2, 0, 0, 0, 0, 0, 3, 0, 0, 0, 0, 0, 0, 0, 0, // 0-15
		0, 1, 2, 1, 2, 0, 0, 3, 0, 0, 0, 0, 0, 0, 0, 0, // 16-31
		0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 1, 2, 1, // 32-47
		0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, // 48-63
		0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, // 64-79
		0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, // 80-95
		0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, // 96-111
		0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, // 112-127
		0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, // 128-143
		0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, // 144-159
		0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, // 160-175
		0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, // 176-191
		0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, // 192-207
		0, 0, 0, 0, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, // 208-223
		0, 0, 0, 0, 0, 0, 0, 3, 3, 0, 0, 0, 0, 0, 0, 0, // 224-239
		0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, // 240-255
		0, 0, 0, 0, 0, 0, 0, 0, 3, 0, 0, 0, 0, 0, 3, 3, // 256-271
		0, 0, 3, 0, 0, 0, 1, 2, 0, 0, 1, 0, 0, 0, 0, 0, // 272-287
		0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, // 288-303
		0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, // 304-319
		0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, // 320-335
	};

	u64 id = bpf_get_current_pid_tgid();
	u64 *start_ts, lat = 0;
	u32 tid = id;

	u64 *cnt;
	u32 idx;

	if (args->id < 0)
		return 0;

	u32 syscall_id = args->id;

	// update the total counter
	idx = 8 * bpf_get_smp_processor_id();
	cnt = bpf_map_lookup_elem(&counters, &idx);

	if (cnt) {
		__sync_fetch_and_add(cnt, 1);
	}

	// for some syscalls, we track counts by "family" of syscall. check the
	// lookup table and increment the appropriate counter
	idx = 0;
	if (syscall_id < 336) {
		u32 counter_idx = syscall_lookup[syscall_id];

		if (counter_idx < 8) {
			idx = 8 * bpf_get_smp_processor_id() + counter_idx;
			cnt = bpf_map_lookup_elem(&counters, &idx);

			if (cnt) {
				__sync_fetch_and_add(cnt, 1);
			}
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