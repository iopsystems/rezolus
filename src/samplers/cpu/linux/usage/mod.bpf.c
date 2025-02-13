// SPDX-License-Identifier: GPL-2.0
// Copyright (c) 2024 The Rezolus Authors

#include <vmlinux.h>
#include "../../../common/bpf/cgroup_info.h"
#include "../../../common/bpf/helpers.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_tracing.h>

#define COUNTER_GROUP_WIDTH 8
#define MAX_CPUS 1024
#define MAX_CGROUPS 4096
#define RINGBUF_CAPACITY 262144

#define IDLE_STAT_INDEX 5
#define IOWAIT_STAT_INDEX 6

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

// cpu usage stat index (https://elixir.bootlin.com/linux/v6.9-rc4/source/include/linux/kernel_stat.h#L20)
// 0 - user
// 1 - nice
// 2 - system
// 3 - softirq
// 4 - irq
//   - idle - *NOTE* this will not increment. User-space must calculate it. This index is skipped
//   - iowait - *NOTE* this will not increment. This index is skipped
// 5 - steal
// 6 - guest
// 7 - guest_nice
struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(map_flags, BPF_F_MMAPABLE);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, MAX_CPUS * COUNTER_GROUP_WIDTH);
} counters SEC(".maps");

// per-cgroup user
struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(map_flags, BPF_F_MMAPABLE);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, MAX_CGROUPS);
} cgroup_user SEC(".maps");

// per-cgroup nice
struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(map_flags, BPF_F_MMAPABLE);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, MAX_CGROUPS);
} cgroup_nice SEC(".maps");

// per-cgroup system
struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(map_flags, BPF_F_MMAPABLE);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, MAX_CGROUPS);
} cgroup_system SEC(".maps");

// per-cgroup softirq
struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(map_flags, BPF_F_MMAPABLE);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, MAX_CGROUPS);
} cgroup_softirq SEC(".maps");

// per-cgroup irq
struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(map_flags, BPF_F_MMAPABLE);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, MAX_CGROUPS);
} cgroup_irq SEC(".maps");

// per-cgroup steal
struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(map_flags, BPF_F_MMAPABLE);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, MAX_CGROUPS);
} cgroup_steal SEC(".maps");

// per-cgroup guest
struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(map_flags, BPF_F_MMAPABLE);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, MAX_CGROUPS);
} cgroup_guest SEC(".maps");

// per-cgroup guest nice
struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(map_flags, BPF_F_MMAPABLE);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, MAX_CGROUPS);
} cgroup_guest_nice SEC(".maps");

SEC("kprobe/cpuacct_account_field")
int BPF_KPROBE(cpuacct_account_field_kprobe, void *task, u32 index, u64 delta)
{
	u32 idx;
	u64 *elem;
	
  // ignore both the idle and the iowait counting since both count the idle time
  // https://elixir.bootlin.com/linux/v6.9-rc4/source/kernel/sched/cputime.c#L227
	if (index == IDLE_STAT_INDEX || index == IOWAIT_STAT_INDEX) {
		return 0;
	}

	// we pack the counters by skipping over the index values for idle and iowait
	// this prevents having those counters mapped to non-incrementing values in
	// this BPF program
	if (index < IDLE_STAT_INDEX) {
		idx = COUNTER_GROUP_WIDTH * bpf_get_smp_processor_id() + index;
		array_add(&counters, idx, delta);
	} else {
		idx = COUNTER_GROUP_WIDTH * bpf_get_smp_processor_id() + index - 2;
		array_add(&counters, idx, delta);
	}

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
				bpf_map_update_elem(&cgroup_user, &cgroup_id, &zero, BPF_ANY);
				bpf_map_update_elem(&cgroup_nice, &cgroup_id, &zero, BPF_ANY);
				bpf_map_update_elem(&cgroup_system, &cgroup_id, &zero, BPF_ANY);
				bpf_map_update_elem(&cgroup_softirq, &cgroup_id, &zero, BPF_ANY);
				bpf_map_update_elem(&cgroup_irq, &cgroup_id, &zero, BPF_ANY);
				bpf_map_update_elem(&cgroup_steal, &cgroup_id, &zero, BPF_ANY);
				bpf_map_update_elem(&cgroup_guest, &cgroup_id, &zero, BPF_ANY);
				bpf_map_update_elem(&cgroup_guest_nice, &cgroup_id, &zero, BPF_ANY);

				// initialize the cgroup info
				struct cgroup_info cginfo = {
					.id = cgroup_id,
					.level = prev->sched_task_group->css.cgroup->level,
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

			switch (index) {
				case 0:
					array_add(&cgroup_user, cgroup_id, delta);
					break;
				case 1:
					array_add(&cgroup_nice, cgroup_id, delta);
					break;
				case 2:
					array_add(&cgroup_system, cgroup_id, delta);
					break;
				case 3:
					array_add(&cgroup_softirq, cgroup_id, delta);
					break;
				case 4:
					array_add(&cgroup_irq, cgroup_id, delta);
					break;
				case 7:
					array_add(&cgroup_steal, cgroup_id, delta);
					break;
				case 8:
					array_add(&cgroup_guest, cgroup_id, delta);
					break;
				case 9:
					array_add(&cgroup_guest_nice, cgroup_id, delta);
					break;
				default:
					break;
			}
		}
	}

	return 0;
}

char LICENSE[] SEC("license") = "GPL";
