// SPDX-License-Identifier: GPL-2.0
// Copyright (c) 2024 The Rezolus Authors

#include <vmlinux.h>
#include "../../../agent/bpf/cgroup_info.h"
#include "../../../agent/bpf/helpers.h"
#include "../../../agent/bpf/task_info.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_tracing.h>

#define CPU_USAGE_GROUP_WIDTH 8
#define MAX_CPUS 1024
#define MAX_CGROUPS 4096
#define MAX_PID 4194304
#define CGROUP_RINGBUF_CAPACITY 262144
#define TASK_RINGBUF_CAPACITY 524288
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
#define SYSTEM_OFFSET 1
#define SOFTIRQ_OFFSET 2
#define IRQ_OFFSET 3

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
struct task_info _task_info = {};

// ringbuf to pass task info
struct {
	__uint(type, BPF_MAP_TYPE_RINGBUF);
	__uint(key_size, 0);
	__uint(value_size, 0);
	__uint(max_entries, TASK_RINGBUF_CAPACITY);
} task_info SEC(".maps");

// holds known task start times to determine new or changed tasks
struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(map_flags, BPF_F_MMAPABLE);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, MAX_PID);
} task_start SEC(".maps");

struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(map_flags, BPF_F_MMAPABLE);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, MAX_PID);
} task_syscall_depth SEC(".maps");

struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(map_flags, BPF_F_MMAPABLE);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, MAX_PID);
} task_last_switch SEC(".maps");

// ringbuf to pass cgroup info
struct {
	__uint(type, BPF_MAP_TYPE_RINGBUF);
	__uint(key_size, 0);
	__uint(value_size, 0);
	__uint(max_entries, CGROUP_RINGBUF_CAPACITY);
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
// 1 - SYSTEM
// 2 - SOFTIRQ
// 3 - IRQ
struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(map_flags, BPF_F_MMAPABLE);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, MAX_CPUS * CPU_USAGE_GROUP_WIDTH);
} cpu_usage SEC(".maps");

// per-task (process) cpu usage (total)
struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(map_flags, BPF_F_MMAPABLE);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, MAX_PID);
} task_usage SEC(".maps");

// per-cgroup user
struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(map_flags, BPF_F_MMAPABLE);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, MAX_CGROUPS);
} cgroup_user SEC(".maps");

// per-cgroup system
struct {
	__uint(type, BPF_MAP_TYPE_ARRAY);
	__uint(map_flags, BPF_F_MMAPABLE);
	__type(key, u32);
	__type(value, u64);
	__uint(max_entries, MAX_CGROUPS);
} cgroup_system SEC(".maps");

static __noinline int update_pid_info(struct task_struct *task) {
    if (!task)
        return -1;

    u64 *elem;

    u32 pid = BPF_CORE_READ(task, pid);
    u32 tgid = BPF_CORE_READ(task, tgid);

    if (pid >= MAX_PID) {
    	return -1;
    }

    u64 start = BPF_CORE_READ(task, start_time);
    elem = bpf_map_lookup_elem(&task_start, &pid);

    if (elem && *elem != start) {
        // we have a new task

        // zero the counters, they will not be exported until they are non-zero
        u64 zero = 0;
        bpf_map_update_elem(&task_usage, &pid, &zero, BPF_ANY);

        // Safely read cgroup_id with null check first
        void *sched_task_group = BPF_CORE_READ(task, sched_task_group);
        if (sched_task_group) {
            int cgroup_id = BPF_CORE_READ(task, sched_task_group, css.id);

            if (cgroup_id && cgroup_id < MAX_CGROUPS) {
                struct task_info tinfo = {
                    .pid = pid,
                    .tgid = tgid,
                    .cglevel = BPF_CORE_READ(task, sched_task_group, css.cgroup, level),
                };

                bpf_get_current_comm(&tinfo.name, TASK_COMM_LEN);

                // read the cgroup name
                bpf_probe_read_kernel_str(&tinfo.cg_name, CGROUP_NAME_LEN,
                    BPF_CORE_READ(task, sched_task_group, css.cgroup, kn, name));

                // read the cgroup parent name
                bpf_probe_read_kernel_str(&tinfo.cg_pname, CGROUP_NAME_LEN,
                    BPF_CORE_READ(task, sched_task_group, css.cgroup, kn, parent, name));

                // read the cgroup grandparent name
                bpf_probe_read_kernel_str(&tinfo.cg_gpname, CGROUP_NAME_LEN,
                    BPF_CORE_READ(task, sched_task_group, css.cgroup, kn, parent, parent, name));

                // push the task info into the ringbuf
                bpf_ringbuf_output(&task_info, &tinfo, sizeof(tinfo), 0);
            }
        } else {
            struct task_info tinfo = {
                .pid = pid,
                .tgid = tgid,
                .cglevel = 0,
            };

            bpf_get_current_comm(&tinfo.name, TASK_COMM_LEN);

            // push the task info into the ringbuf
            bpf_ringbuf_output(&task_info, &tinfo, sizeof(tinfo), 0);
        }

        // update the start time in the local map
        bpf_map_update_elem(&task_start, &pid, &start, BPF_ANY);
    }

    return 0;
}

static __noinline int update_cgroup_info(struct task_struct *task) {
    if (!task)
        return -1;

    if (bpf_core_field_exists(task->sched_task_group)) {
        void *sched_task_group = BPF_CORE_READ(task, sched_task_group);
        if (!sched_task_group)
            return 0;

        int cgroup_id = BPF_CORE_READ(task, sched_task_group, css.id);
        u64 serial_nr = BPF_CORE_READ(task, sched_task_group, css.serial_nr);

        u64 *elem;

        if (cgroup_id && cgroup_id < MAX_CGROUPS) {
            // we check to see if this is a new cgroup by checking the serial number
            elem = bpf_map_lookup_elem(&cgroup_serial_numbers, &cgroup_id);

            if (elem && *elem != serial_nr) {
                // zero the counters, they will not be exported until they are non-zero
                u64 zero = 0;
                bpf_map_update_elem(&cgroup_user, &cgroup_id, &zero, BPF_ANY);
                bpf_map_update_elem(&cgroup_system, &cgroup_id, &zero, BPF_ANY);

                // initialize the cgroup info
                struct cgroup_info cginfo = {
                    .id = cgroup_id,
                    .level = BPF_CORE_READ(task, sched_task_group, css.cgroup, level),
                };

                // read the cgroup name
                bpf_probe_read_kernel_str(&cginfo.name, CGROUP_NAME_LEN,
                    BPF_CORE_READ(task, sched_task_group, css.cgroup, kn, name));

                // read the cgroup parent name
                bpf_probe_read_kernel_str(&cginfo.pname, CGROUP_NAME_LEN,
                    BPF_CORE_READ(task, sched_task_group, css.cgroup, kn, parent, name));

                // read the cgroup grandparent name
                bpf_probe_read_kernel_str(&cginfo.gpname, CGROUP_NAME_LEN,
                    BPF_CORE_READ(task, sched_task_group, css.cgroup, kn, parent, parent, name));

                // push the cgroup info into the ringbuf
                bpf_ringbuf_output(&cgroup_info, &cginfo, sizeof(cginfo), 0);

                // update the serial number in the local map
                bpf_map_update_elem(&cgroup_serial_numbers, &cgroup_id, &serial_nr, BPF_ANY);
            }
        }

        return cgroup_id;
    }

    return 0;
}

SEC("tp_btf/sched_switch")
int handle__sched_switch(u64 *ctx)
{
    /* TP_PROTO(bool preempt, struct task_struct *prev, struct task_struct *next) */
    struct task_struct *prev = (struct task_struct *)ctx[1];
    struct task_struct *next = (struct task_struct *)ctx[2];

    u32 cpu = bpf_get_smp_processor_id();
    u32 prev_pid = BPF_CORE_READ(prev, pid);
    u32 next_pid = BPF_CORE_READ(next, pid);

    u64 now = bpf_ktime_get_ns();

    u64 *syscall_depth, *last_switch;

    // Account for time spent by the previous process
    if (prev_pid > 0 && prev_pid < MAX_PID) {
        last_switch = bpf_map_lookup_elem(&task_last_switch, &prev_pid);

        u32 cgroup_id = 0;
        if (prev) {
            cgroup_id = update_cgroup_info(prev);
            update_pid_info(prev);
        }

    	if (!last_switch || !*last_switch) {
    		// missed a sched_switch, this is moving off-cpu so don't update last
    		return 0;
    	}

    	syscall_depth = bpf_map_lookup_elem(&task_syscall_depth, &prev_pid);

    	if (!syscall_depth) {
    		return 0;
    	}

    	u64 delta = now - *last_switch;

    	if (*syscall_depth == 0) {
    		if (cpu < MAX_CPUS) {
    			u32 idx = CPU_USAGE_GROUP_WIDTH * cpu + USER_OFFSET;
    			array_add(&cpu_usage, idx, delta);
    		}

			if (cgroup_id) {
				array_add(&cgroup_user, cgroup_id, delta);
			}

    	} else {
    		if (cpu < MAX_CPUS) {
    			u32 idx = CPU_USAGE_GROUP_WIDTH * cpu + SYSTEM_OFFSET;
    			array_add(&cpu_usage, idx, delta);
    		}

    		if (cgroup_id) {
				array_add(&cgroup_system, cgroup_id, delta);
			}
    	}

		array_add(&task_usage, prev_pid, delta);
	}

	if (next) {
        update_cgroup_info(next);
        update_pid_info(next);
    }

    // Initialize state for the next process if needed
    if (next_pid != 0) {
    	bpf_map_update_elem(&task_last_switch, &next_pid, &now, BPF_ANY);
    }

    return 0;
}

SEC("tracepoint/raw_syscalls/sys_enter")
int sys_enter(struct trace_event_raw_sys_enter *args)
{
	u32 pid = bpf_get_current_pid_tgid();

	if (pid >= MAX_PID) {
		return 0;
	}

	u32 cpu = bpf_get_smp_processor_id();
	u64 now = bpf_ktime_get_ns();

	u64 *syscall_depth, *last_switch;

	syscall_depth = bpf_map_lookup_elem(&task_syscall_depth, &pid);
	last_switch = bpf_map_lookup_elem(&task_last_switch, &pid);

	if (!syscall_depth) {
		return 0;
	}

	array_incr(&task_syscall_depth, pid);

	if (*syscall_depth > 0) {
		// task was already in a syscall, return early
		return 0;
	}

	// task switched from user to system mode

	// get last switch time and update the map with the current time
	last_switch = bpf_map_lookup_elem(&task_last_switch, &pid);
	bpf_map_update_elem(&task_last_switch, &pid, &now, BPF_ANY);

	if (!last_switch || !*last_switch) {
		// missed a switch, return early
		return 0;
	}

	// account time that was in user mode

	u64 delta = now - *last_switch;

	if (cpu < MAX_CPUS) {
		u32 idx = CPU_USAGE_GROUP_WIDTH * cpu + USER_OFFSET;
		array_add(&cpu_usage, idx, delta);
	}

	struct task_struct *task = bpf_get_current_task_btf();

	if (!task) {
        return 0;
    }

	u32 cgroup_id = update_cgroup_info(task);

	if (cgroup_id > 0 && cgroup_id < MAX_CGROUPS) {
		array_add(&cgroup_user, cgroup_id, delta);
	}

	update_pid_info(task);

	array_add(&task_usage, pid, delta);

	*last_switch = now;

    return 0;
}

SEC("tracepoint/raw_syscalls/sys_exit")
int sys_exit(struct trace_event_raw_sys_exit *args)
{
	u32 pid = bpf_get_current_pid_tgid();

	if (pid >= MAX_PID) {
		return 0;
	}

	u32 cpu = bpf_get_smp_processor_id();
	u64 now = bpf_ktime_get_ns();

	u64 *syscall_depth, *last_switch;

	syscall_depth = bpf_map_lookup_elem(&task_syscall_depth, &pid);

	if (!syscall_depth || !*syscall_depth) {
		// missed one or more sys_enter, return without doing anything
		return 0;
	}

	array_decr(&task_syscall_depth, pid);

	if (*syscall_depth) {
		// still in a syscall, return without doing anything
		return 0;
	}

	// task switched from system to user mode

	// get last switch time and update the map with the current time
	last_switch = bpf_map_lookup_elem(&task_last_switch, &pid);
	bpf_map_update_elem(&task_last_switch, &pid, &now, BPF_ANY);

	if (!last_switch || !*last_switch) {
		// missed a switch, return early
		return 0;
	}

	// account time that was in system mode

	u64 delta = now - *last_switch;

	if (cpu < MAX_CPUS) {
		u32 idx = CPU_USAGE_GROUP_WIDTH * cpu + SYSTEM_OFFSET;
		array_add(&cpu_usage, idx, delta);
	}

	struct task_struct *task = bpf_get_current_task_btf();

	if (!task) {
        return 0;
    }

	u32 cgroup_id = update_cgroup_info(task);

	if (cgroup_id) {
		array_add(&cgroup_system, cgroup_id, delta);
	}

	update_pid_info(task);

	array_add(&task_usage, pid, delta);

	bpf_map_update_elem(&task_last_switch, &pid, &now, BPF_ANY);

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
	u32 idx, group = 0;

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
	idx = CPU_USAGE_GROUP_WIDTH * cpu + SOFTIRQ_OFFSET;
	array_add(&cpu_usage, idx, dur);

	// update the softirq time
	idx = SOFTIRQ_GROUP_WIDTH * cpu + args->vec;
	array_add(&softirq_time, idx, dur);

	// clear the start timestamp
	*start_ts = 0;

	return 0;
}

char LICENSE[] SEC("license") = "GPL";
