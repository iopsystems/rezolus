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
#include "../../../common/bpf/cgroup_info.h"
#include "../../../common/bpf/helpers.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_tracing.h>
#include <bpf/bpf_core_read.h>

#define COUNTER_GROUP_WIDTH 16
#define MAX_CPUS 1024
#define MAX_CGROUPS 4096
#define MAX_SYSCALL_ID 1024
#define MAX_PID 4194304
#define RINGBUF_CAPACITY 32768

// dummy instance for skeleton to generate definition
struct cgroup_info _cgroup_info = {};

// ringbuf to pass cgroup info
struct {
	__uint(type, BPF_MAP_TYPE_RINGBUF);
	__uint(key_size, 0);
	__uint(value_size, 0);
	__uint(max_entries, RINGBUF_CAPACITY);
} cgroup_info SEC(".maps");

// holds known cgroup serial numbers to help determine new or changed groups
struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(map_flags, BPF_F_MMAPABLE);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, MAX_CGROUPS);
} cgroup_serial_numbers SEC(".maps");

// counters for syscalls
// 0 - other
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

// per-cgroup syscalls - other
struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(map_flags, BPF_F_MMAPABLE);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, MAX_CGROUPS);
} cgroup_syscall_other SEC(".maps");

// per-cgroup syscalls - read
struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(map_flags, BPF_F_MMAPABLE);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, MAX_CGROUPS);
} cgroup_syscall_read SEC(".maps");

// per-cgroup syscalls - write
struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(map_flags, BPF_F_MMAPABLE);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, MAX_CGROUPS);
} cgroup_syscall_write SEC(".maps");

SEC("tracepoint/raw_syscalls/sys_enter")
int sys_enter(struct trace_event_raw_sys_enter *args)
{
	u32 offset, idx, group = 0;
	u64 *elem;

	if (args->id < 0) {
		return 0;
	}

	u32 syscall_id = args->id;
	offset = COUNTER_GROUP_WIDTH * bpf_get_smp_processor_id();

	// for some syscalls, we track counts by "family" of syscall. check the
	// lookup table and increment the appropriate counter
	idx = 0;
	if (syscall_id < MAX_SYSCALL_ID) {
		u32 *counter_offset = bpf_map_lookup_elem(&syscall_lut, &syscall_id);

		if (counter_offset && *counter_offset && *counter_offset < COUNTER_GROUP_WIDTH) {
			group = (u32)*counter_offset;
		}
	}

	idx = offset + group;
	array_incr(&counters, idx);

	struct task_struct *current = bpf_get_current_task_btf();

	if (bpf_core_field_exists(current->sched_task_group)) {
		int cgroup_id = current->sched_task_group->css.id;
		u64	serial_nr = current->sched_task_group->css.serial_nr;

		if (cgroup_id && cgroup_id < MAX_CGROUPS) {

			// we check to see if this is a new cgroup by checking the serial number

			elem = bpf_map_lookup_elem(&cgroup_serial_numbers, &cgroup_id);

			if (elem && *elem != serial_nr) {
				// zero the counters, they will not be exported until they are non-zero
				u64 zero = 0;
				bpf_map_update_elem(&cgroup_syscall_other, &cgroup_id, &zero, BPF_ANY);
				bpf_map_update_elem(&cgroup_syscall_read, &cgroup_id, &zero, BPF_ANY);
				bpf_map_update_elem(&cgroup_syscall_write, &cgroup_id, &zero, BPF_ANY);

				// initialize the cgroup info
				struct cgroup_info cginfo = {
					.id = cgroup_id,
				};

				// read the cgroup name
				bpf_probe_read_kernel_str(&cginfo.name, CGROUP_NAME_LEN, current->sched_task_group->css.cgroup->kn->name);

				// read the cgroup parent name
				bpf_probe_read_kernel_str(&cginfo.pname, CGROUP_NAME_LEN, current->sched_task_group->css.cgroup->kn->parent->name);

				// read the cgroup grandparent name
				bpf_probe_read_kernel_str(&cginfo.gpname, CGROUP_NAME_LEN, current->sched_task_group->css.cgroup->kn->parent->parent->name);

				// push the cgroup info into the ringbuf
				bpf_ringbuf_output(&cgroup_info, &cginfo, sizeof(cginfo), 0);

				// update the serial number in the local map
				bpf_map_update_elem(&cgroup_serial_numbers, &cgroup_id, &serial_nr, BPF_ANY);
			}

			switch (group) {
			    case 1:
			        array_incr(&cgroup_syscall_read, cgroup_id);
			        break;
			    case 2:
			        array_incr(&cgroup_syscall_write, cgroup_id);
			        break;
			    default:
			        array_incr(&cgroup_syscall_other, cgroup_id);
			        break;
			}
		}
	}

	return 0;
}

char LICENSE[] SEC("license") = "GPL";
