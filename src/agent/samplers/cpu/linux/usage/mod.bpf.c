// SPDX-License-Identifier: GPL-2.0
// Copyright (c) 2024 The Rezolus Authors

#include <vmlinux.h>
#include "../../../agent/bpf/cgroup.h"
#include "../../../agent/bpf/helpers.h"
#include "../../../agent/bpf/task.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_tracing.h>

#define CPU_USAGE_GROUP_WIDTH 8
#define MAX_CPUS 1024
#define SOFTIRQ_GROUP_WIDTH 16

// cpu usage stat index
// (https://elixir.bootlin.com/linux/v6.9-rc4/source/include/linux/kernel_stat.h#L20)
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

// dummy instances for skeleton to generate definitions
struct cgroup_info _cgroup_info = {};
struct task_info _task_info = {};
struct task_exit _task_exit = {};

/*
 * cgroup tracking
 */

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

/*
 * task tracking
 */

// ringbuf to pass task info for new tasks
struct {
    __uint(type, BPF_MAP_TYPE_RINGBUF);
    __uint(key_size, 0);
    __uint(value_size, 0);
    __uint(max_entries, TASK_RINGBUF_CAPACITY);
} task_info SEC(".maps");

// ringbuf to notify userspace of task exits
struct {
    __uint(type, BPF_MAP_TYPE_RINGBUF);
    __uint(key_size, 0);
    __uint(value_size, 0);
    __uint(max_entries, TASK_RINGBUF_CAPACITY);
} task_exit SEC(".maps");

// holds task start times to detect new or reused PIDs
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_PID);
} task_start_times SEC(".maps");

/*
 * softirq tracking
 */

// track the start time of softirq
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(max_entries, MAX_CPUS * 8);
    __type(key, u32);
    __type(value, u64);
} softirq_start SEC(".maps");

// per-cpu softirq counts by category
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CPUS* SOFTIRQ_GROUP_WIDTH);
} softirq SEC(".maps");

// per-cpu softirq time in nanoseconds by category
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CPUS* SOFTIRQ_GROUP_WIDTH);
} softirq_time SEC(".maps");

/*
 * cpu usage counters
 */

// per-cpu cpu usage tracking in nanoseconds by category
// 0 - USER
// 1 - SYSTEM
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CPUS* CPU_USAGE_GROUP_WIDTH);
} cpu_usage SEC(".maps");

// tracking per-task user time (internal, for delta calculation)
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_PID);
} task_utime SEC(".maps");

// tracking per-task system time (internal, for delta calculation)
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_PID);
} task_stime SEC(".maps");

// per-task cpu usage in nanoseconds (user + system, exported)
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_PID);
} task_cpu_usage SEC(".maps");

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

/**
 * handle_new_task - Check if task is new/reused and send info to userspace
 * @task: The task_struct to check
 *
 * Returns 0 if new task was detected and info sent, 1 if existing task, -1 on error.
 */
static __noinline int handle_new_task(struct task_struct* task) {
    if (!task)
        return -1;

    u32 pid = BPF_CORE_READ(task, pid);
    if (pid == 0 || pid >= MAX_PID)
        return -1;

    u64 start_time = BPF_CORE_READ(task, start_time);

    u64* last_start = bpf_map_lookup_elem(&task_start_times, &pid);
    if (!last_start)
        return -1;

    // Check if this is the same task we've seen before
    if (*last_start == start_time)
        return 1;

    // New task or PID reuse - zero the counters first
    u64 zero = 0;
    bpf_map_update_elem(&task_utime, &pid, &zero, BPF_ANY);
    bpf_map_update_elem(&task_stime, &pid, &zero, BPF_ANY);
    bpf_map_update_elem(&task_cpu_usage, &pid, &zero, BPF_ANY);

    // Update the start time
    bpf_map_update_elem(&task_start_times, &pid, &start_time, BPF_ANY);

    // Populate and send task info
    struct task_info info = {};
    populate_task_info(task, &info);
    bpf_ringbuf_output(&task_info, &info, sizeof(info), 0);

    return 0;
}

// The kprobe handler is not always invoked, so using the delta to count the CPU usage could cause
// undercounting. Kernel increases the task utime/stime before invoking cpuacct_account_field. So we
// count the CPU usage by tracking the per-task utime/stime. The user time includes both the
// CPUTIME_NICE and CPUTIME_USER. The system time includes CPUTIME_SYSTEM, CPUTIME_SOFTIRQ and
// CPUTIME_IRQ.
SEC("kprobe/cpuacct_account_field")
int BPF_KPROBE(cpuacct_account_field_kprobe, struct task_struct* task, u32 index, u64 delta) {
    u32 cpu, idx;
    u64 curr_utime, curr_stime;
    u64 *last_utime, *last_stime;
    u32 pid;

    if (!task)
        return 0;

    pid = BPF_CORE_READ(task, pid);
    if (pid == 0 || pid >= MAX_PID)
        return 0;

    // Check if this is a new task
    handle_new_task(task);

    curr_utime = BPF_CORE_READ(task, utime);
    curr_stime = BPF_CORE_READ(task, stime);

    last_utime = bpf_map_lookup_elem(&task_utime, &pid);
    last_stime = bpf_map_lookup_elem(&task_stime, &pid);

    if (!last_utime || !last_stime)
        return 0;

    // Calculate deltas with overflow protection
    u64 delta_utime = 0;
    u64 delta_stime = 0;

    // Only calculate delta if we have valid previous values
    if (*last_utime != 0 && curr_utime >= *last_utime) {
        delta_utime = curr_utime - *last_utime;
    }

    if (*last_stime != 0 && curr_stime >= *last_stime) {
        delta_stime = curr_stime - *last_stime;
    }

    // Update last seen values
    *last_utime = curr_utime;
    *last_stime = curr_stime;

    // Skip updating metrics if there's no change
    if (delta_utime == 0 && delta_stime == 0)
        return 0;

    // Get CPU index
    cpu = bpf_get_smp_processor_id();
    if (cpu >= MAX_CPUS)
        return 0;

    // Update per-CPU user time
    if (delta_utime > 0) {
        idx = CPU_USAGE_GROUP_WIDTH * cpu + USER_OFFSET;
        if (idx < MAX_CPUS * CPU_USAGE_GROUP_WIDTH) {
            array_add(&cpu_usage, idx, delta_utime);
        }
    }

    // Update per-CPU system time
    if (delta_stime > 0) {
        idx = CPU_USAGE_GROUP_WIDTH * cpu + SYSTEM_OFFSET;
        if (idx < MAX_CPUS * CPU_USAGE_GROUP_WIDTH) {
            array_add(&cpu_usage, idx, delta_stime);
        }
    }

    // Update per-task CPU usage (total of user + system)
    u64 delta_total = delta_utime + delta_stime;
    if (delta_total > 0) {
        array_add(&task_cpu_usage, pid, delta_total);
    }

    // Additional accounting on a per-cgroup basis
    struct task_group *tg = BPF_CORE_READ(task, sched_task_group);
    if (!tg)
        return 0;

    int cgroup_id = BPF_CORE_READ(tg, css.id);
    if (cgroup_id < 0 || cgroup_id >= MAX_CGROUPS)
        return 0;

    int ret = handle_new_cgroup(task, &cgroup_serial_numbers, &cgroup_info);
    if (ret == 0) {
        // New cgroup detected, zero the counters
        u64 zero = 0;
        bpf_map_update_elem(&cgroup_user, &cgroup_id, &zero, BPF_ANY);
        bpf_map_update_elem(&cgroup_system, &cgroup_id, &zero, BPF_ANY);
    }

    // Update per-cgroup counters
    if (delta_utime > 0) {
        array_add(&cgroup_user, cgroup_id, delta_utime);
    }

    if (delta_stime > 0) {
        array_add(&cgroup_system, cgroup_id, delta_stime);
    }

    return 0;
}

SEC("tp_btf/sched_process_exit")
int handle__sched_process_exit(u64* ctx) {
    /* TP_PROTO(struct task_struct *p) */
    struct task_struct* task = (struct task_struct*)ctx[0];

    u32 pid = BPF_CORE_READ(task, pid);
    if (pid == 0 || pid >= MAX_PID)
        return 0;

    // Zero the exported counter FIRST to prevent exporting values without metadata
    u64 zero = 0;
    bpf_map_update_elem(&task_cpu_usage, &pid, &zero, BPF_ANY);

    // Clean up internal tracking state
    bpf_map_update_elem(&task_utime, &pid, &zero, BPF_ANY);
    bpf_map_update_elem(&task_stime, &pid, &zero, BPF_ANY);
    bpf_map_update_elem(&task_start_times, &pid, &zero, BPF_ANY);

    // Notify userspace to clear metadata
    struct task_exit exit_event = { .pid = pid };
    bpf_ringbuf_output(&task_exit, &exit_event, sizeof(exit_event), 0);

    return 0;
}

SEC("tracepoint/irq/softirq_entry")
int softirq_enter(struct trace_event_raw_softirq* args) {
    u32 cpu = bpf_get_smp_processor_id();
    u64 ts = bpf_ktime_get_ns();

    u32 idx = cpu * SOFTIRQ_GROUP_WIDTH + args->vec;
    u32 start_idx = cpu * 8;

    bpf_map_update_elem(&softirq_start, &start_idx, &ts, 0);
    array_incr(&softirq, idx);

    return 0;
}

SEC("tracepoint/irq/softirq_exit")
int softirq_exit(struct trace_event_raw_softirq* args) {
    u32 cpu = bpf_get_smp_processor_id();
    u64 *start_ts, dur = 0;
    u32 idx, cpuusage_idx;
    u32 start_idx = cpu * 8;

    // lookup the start time
    start_ts = bpf_map_lookup_elem(&softirq_start, &start_idx);

    // possible we missed the start
    if (!start_ts || *start_ts == 0) {
        return 0;
    }

    struct task_struct* current = (struct task_struct*)bpf_get_current_task();
    int pid = BPF_CORE_READ(current, pid);

    // calculate the duration
    dur = bpf_ktime_get_ns() - *start_ts;

    // update the softirq time
    idx = SOFTIRQ_GROUP_WIDTH * cpu + args->vec;
    array_add(&softirq_time, idx, dur);
    if (pid == 0) {
        cpuusage_idx = CPU_USAGE_GROUP_WIDTH * cpu + SYSTEM_OFFSET;
        array_add(&cpu_usage, cpuusage_idx, dur);
    }

    // clear the start timestamp
    *start_ts = 0;

    return 0;
}

char LICENSE[] SEC("license") = "GPL";
