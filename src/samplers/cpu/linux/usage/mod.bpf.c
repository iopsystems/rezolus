// SPDX-License-Identifier: GPL-2.0
// Copyright (c) 2024 The Rezolus Authors

#include <vmlinux.h>
#include "../../../common/bpf/cgroup_info.h"
#include "../../../common/bpf/helpers.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_tracing.h>

#define CPU_USAGE_GROUP_WIDTH 8
#define MAX_CPUS 1024
#define MAX_CGROUPS 4096
#define RINGBUF_CAPACITY 262144
#define SOFTIRQ_GROUP_WIDTH 16

// cpu usage stat index (https://elixir.bootlin.com/linux/v6.9-rc4/source/include/linux/kernel_stat.h#L20)
#define USER 0
#define NICE 1
#define SYSTEM 2
#define SOFTIRQ 3
#define IRQ 4
#define IDLE 5
#define IOWAIT 6
#define STEAL 7
#define GUEST 8
#define GUEST_NICE 9

// the offsets to use in the `counters` group
#define USER_OFFSET 0
#define NICE_OFFSET 1
#define SYSTEM_OFFSET 2
#define SOFTIRQ_OFFSET 3
#define IRQ_OFFSET 4
#define STEAL_OFFSET 5
#define GUEST_OFFSET 6
#define GUEST_NICE_OFFSET 7

// the offsets to use in the `softirqs` group
#define HI 0
#define TIMER 1
#define NET_TX 2
#define NET_RX 3
#define BLOCK 4
#define IRQ_POLL 5
#define TASKLET 6
#define SCHED 7
#define HRTIMER 8
#define RCU 9

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

// track the start time of softirq
struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(max_entries, MAX_CPUS);
	__type(key, u32);
	__type(value, u64);
} softirq_start SEC(".maps");

// per-cpu softirq counts by category
// 0 - HI
// 1 - TIMER
// 2 - NET_TX
// 3 - NET_RX
// 4 - BLOCK
// 5 - IRQ_POLL
// 6 - TASKLET
// 7 - SCHED
// 8 - HRTIMER
// 9 - RCU
struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(map_flags, BPF_F_MMAPABLE);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, MAX_CPUS * SOFTIRQ_GROUP_WIDTH);
} softirq SEC(".maps");

// per-cpu softirq time in nanoseconds by category
// 0 - HI
// 1 - TIMER
// 2 - NET_TX
// 3 - NET_RX
// 4 - BLOCK
// 5 - IRQ_POLL
// 6 - TASKLET
// 7 - SCHED
// 8 - HRTIMER
// 9 - RCU
struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(map_flags, BPF_F_MMAPABLE);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, MAX_CPUS * SOFTIRQ_GROUP_WIDTH);
} softirq_time SEC(".maps");

// per-cpu cpu usage tracking in nanoseconds by category
// 0 - USER
// 1 - NICE
// 2 - SYSTEM
// 3 - SOFTIRQ
// 4 - IRQ
// 5 - STEAL
// 6 - GUEST
// 7 - GUEST_NICE
struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(map_flags, BPF_F_MMAPABLE);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, MAX_CPUS * CPU_USAGE_GROUP_WIDTH);
} cpu_usage SEC(".maps");

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
	u32 idx, offset;
	u64 *elem;

	// softirq is tracked with separate tracepoints for better accuracy
	//
	// ignore both the idle and the iowait counting since both count the idle time
    // https://elixir.bootlin.com/linux/v6.9-rc4/source/kernel/sched/cputime.c#L227
	switch (index) {
		case USER:
			offset = USER_OFFSET;
			break;
		case NICE:
			offset = NICE_OFFSET;
			break;
		case SYSTEM:
			offset = SYSTEM_OFFSET;
			break;
		case IRQ:
			offset = IRQ_OFFSET;
			break;
		case STEAL:
			offset = STEAL_OFFSET;
			break;
		case GUEST:
			offset = GUEST_OFFSET;
			break;
		case GUEST_NICE:
			offset = NICE_OFFSET;
			break;
		default:
			return 0;
	}

	// calculate the counter index and increment the counter
	idx = CPU_USAGE_GROUP_WIDTH * bpf_get_smp_processor_id() + offset;
	array_add(&cpu_usage, idx, delta);

	// additional accounting on a per-cgroup basis follows

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
					.level = current->sched_task_group->css.cgroup->level,
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
				case USER:
					array_add(&cgroup_user, cgroup_id, delta);
					break;
				case NICE:
					array_add(&cgroup_nice, cgroup_id, delta);
					break;
				case SYSTEM:
					array_add(&cgroup_system, cgroup_id, delta);
					break;
				case STEAL:
					array_add(&cgroup_steal, cgroup_id, delta);
					break;
				case GUEST:
					array_add(&cgroup_guest, cgroup_id, delta);
					break;
				case GUEST_NICE:
					array_add(&cgroup_guest_nice, cgroup_id, delta);
					break;
				default:
					break;
			}
		}
	}

	return 0;
}

SEC("tracepoint/irq/softirq_entry")
int softirq_enter(struct trace_event_raw_softirq *args)
{
	u32 cpu = bpf_get_smp_processor_id();
	u64 ts = bpf_ktime_get_ns();

	u32 idx = cpu * SOFTIRQ_GROUP_WIDTH + args->vec;

	bpf_map_update_elem(&softirq_start, &cpu, &ts, 0);
	array_incr(&softirq, idx);

	return 0;
}

SEC("tracepoint/irq/softirq_exit")
int softirq_exit(struct trace_event_raw_softirq *args)
{
	u32 cpu = bpf_get_smp_processor_id();
	u64 *elem, *start_ts, dur = 0;
	u32 offset, idx, group = 0;

	u32 irq_id = 0;

	// lookup the start time
	start_ts = bpf_map_lookup_elem(&softirq_start, &cpu);

	// possible we missed the start
	if (!start_ts || *start_ts == 0) {
		return 0;
	}

	// calculate the duration
	dur = bpf_ktime_get_ns() - *start_ts;

	// update the cpu usage
	offset = CPU_USAGE_GROUP_WIDTH * cpu + SOFTIRQ_OFFSET;
	array_add(&cpu_usage, idx, dur);

	// update the softirq time
	offset = SOFTIRQ_GROUP_WIDTH * cpu + args->vec;
	array_add(&softirq_time, idx, dur);

	// clear the start timestamp
	*start_ts = 0;

	return 0;
}

char LICENSE[] SEC("license") = "GPL";
