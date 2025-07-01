// SPDX-License-Identifier: GPL-2.0
// Copyright (c) 2024 The Rezolus Authors

#include <vmlinux.h>
#include "../../../agent/bpf/cgroup_info.h"
#include "../../../agent/bpf/helpers.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_tracing.h>

#define CPU_USAGE_GROUP_WIDTH 8
#define MAX_CPUS 1024
#define MAX_PID 4194304
#define MAX_CGROUPS 4096
#define RINGBUF_CAPACITY 262144
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

static void account_cpu_usage(struct task_struct* task, u32 index) {
    u32 idx, offset;
    u64* elem;
    void* task_time;
    u64 curr_time;
    u64* last_time;
    u32 pid = BPF_CORE_READ(task, pid);
    if (pid == 0 || pid >= MAX_PID)
        return;
    switch (index) {
    case USER:
        offset = USER_OFFSET;
        task_time = &task_utime;
        curr_time = BPF_CORE_READ(task, utime);
        break;
    case SYSTEM:
        offset = SYSTEM_OFFSET;
        task_time = &task_stime;
        curr_time = BPF_CORE_READ(task, stime);
        break;
    default:
        return;
    }
    last_time = bpf_map_lookup_elem(task_time, &pid);
    if (last_time == NULL)
        return;
    // nothing needs to update
    if (*last_time == curr_time)
        return;
    // first time counting this task
    if (*last_time == 0) {
        *last_time = curr_time;
        return;
    }
    // calculate the counter index and increment the counter
    u64 delta = curr_time - *last_time;

    // check if we've had a counter reset, in which case this is pid re-use and
    // we use the current time as the delta (since delta would be from zero)
    if (delta > 1 << 63) {
        delta = curr_time;
    }

    *last_time = curr_time;
    idx = CPU_USAGE_GROUP_WIDTH * bpf_get_smp_processor_id() + offset;
    array_add(&cpu_usage, idx, delta);

    // additional accounting on a per-cgroup basis follows
    if (bpf_core_field_exists(task->sched_task_group)) {
        // int cgroup_id = task->sched_task_group->css.id;
        int cgroup_id = BPF_CORE_READ(task, sched_task_group, css.id);
        u64 serial_nr = BPF_CORE_READ(task, sched_task_group, css.serial_nr);

        if (cgroup_id && cgroup_id < MAX_CGROUPS) {

            // we check to see if this is a new cgroup by checking the serial number

            elem = bpf_map_lookup_elem(&cgroup_serial_numbers, &cgroup_id);

            if (elem && *elem != serial_nr) {
                // zero the counters, they will not be exported until they are non-zero
                u64 zero = 0;
                bpf_map_update_elem(&cgroup_user, &cgroup_id, &zero, BPF_ANY);
                bpf_map_update_elem(&cgroup_system, &cgroup_id, &zero, BPF_ANY);

                // initialize the cgroup info
                struct cgroup_info cginfo = { .id = cgroup_id,
                                              .level = BPF_CORE_READ(task, sched_task_group,
                                                                     css.cgroup, level) };

                // read the cgroup name
                bpf_probe_read_kernel_str(
                    &cginfo.name, CGROUP_NAME_LEN,
                    BPF_CORE_READ(task, sched_task_group, css.cgroup, kn, name));

                // read the cgroup parent name
                bpf_probe_read_kernel_str(
                    &cginfo.pname, CGROUP_NAME_LEN,
                    BPF_CORE_READ(task, sched_task_group, css.cgroup, kn, parent, name));

                // read the cgroup grandparent name
                bpf_probe_read_kernel_str(
                    &cginfo.gpname, CGROUP_NAME_LEN,
                    BPF_CORE_READ(task, sched_task_group, css.cgroup, kn, parent, parent, name));

                // push the cgroup info into the ringbuf
                bpf_ringbuf_output(&cgroup_info, &cginfo, sizeof(cginfo), 0);

                // update the serial number in the local map
                bpf_map_update_elem(&cgroup_serial_numbers, &cgroup_id, &serial_nr, BPF_ANY);
            }

            switch (index) {
            case USER:
                array_add(&cgroup_user, cgroup_id, delta);
                break;
            case SYSTEM:
                array_add(&cgroup_system, cgroup_id, delta);
                break;
            default:
                break;
            }
        }
    }
}

// The kprobe handler is not always invoked, so using the delta to count the CPU usage could cause
// undercounting. Kernel increases the task utime/stime before invoking cpuacct_account_field. So we
// count the CPU usage by tracking the per-task utime/stime. The user time includes both the
// CPUTIME_NICE and CPUTIME_USER. The system time includes CPUTIME_SYSTEM, CPUTIME_SOFTIRQ and
// CPUTIME_IRQ.
SEC("kprobe/cpuacct_account_field")
int BPF_KPROBE(cpuacct_account_field_kprobe, struct task_struct* task, u32 index, u64 delta) {
    account_cpu_usage(task, SYSTEM);
    account_cpu_usage(task, USER);
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
