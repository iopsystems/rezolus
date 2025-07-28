// SPDX-License-Identifier: GPL-2.0
// Copyright (c) 2024 The Rezolus Authors

#include <vmlinux.h>
#include "../../../agent/bpf/cgroup.h"
#include "../../../agent/bpf/helpers.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_tracing.h>

#define CPU_USAGE_GROUP_WIDTH 8
#define MAX_CPUS 1024
#define MAX_PID 4194304
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
    __uint(max_entries, MAX_CPUS * 8);
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
    __uint(max_entries, MAX_CPUS* SOFTIRQ_GROUP_WIDTH);
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
    __uint(max_entries, MAX_CPUS* SOFTIRQ_GROUP_WIDTH);
} softirq_time SEC(".maps");

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

// tracking per-task user time
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_PID);
} task_utime SEC(".maps");

// tracking per-task system time
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_PID);
} task_stime SEC(".maps");

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
    u64 *elem, *start_ts, dur = 0;
    u32 idx, cpuusage_idx, group = 0;

    u32 irq_id = 0;
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
