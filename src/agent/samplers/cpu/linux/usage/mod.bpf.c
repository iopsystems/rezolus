// SPDX-License-Identifier: GPL-2.0
// Copyright (c) 2024 The Rezolus Authors

#include <vmlinux.h>
#include "../../../agent/bpf/btf_read.h"
#include "../../../agent/bpf/cgroup.h"
#include "../../../agent/bpf/helpers.h"
#include "../../../agent/bpf/task.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_tracing.h>

#define CPU_USAGE_GROUP_WIDTH 8
#define MAX_CPUS 1024
#define SOFTIRQ_GROUP_WIDTH 16

// Per-cgroup attribution is the config option `cgroup_attribution`, on by
// default for this sampler. Written into read-only data before load, so with
// it off the verifier removes the per-cgroup path (the task-group read, the
// new-cgroup check and the per-cgroup adds) rather than testing a flag on
// every event.
const volatile __u8 cgroup_attribution = 0;

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
#define EXITED_OFFSET 2

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
    __uint(max_entries, TASK_EXIT_RINGBUF_CAPACITY);
} task_exit SEC(".maps");

// holds task start times to detect new or reused PIDs, at pid_slot(pid)
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
//
// one slot per CPU (softirqs do not nest on a CPU), padded out to a whole
// cacheline (* 8) to avoid false sharing between cores; the index is always
// cpu * 8
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

// tracking per-task user time (internal, for delta calculation), at
// pid_slot(pid)
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_PID);
} task_utime SEC(".maps");

// tracking per-task system time (internal, for delta calculation), at
// pid_slot(pid)
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_PID);
} task_stime SEC(".maps");

// per-task cpu usage in nanoseconds (user + system, exported), at
// pid_slot(pid); userspace assigns each task's series to that index
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
    __uint(max_entries, MAX_CGROUPS * CGROUP_SHARDS);
} cgroup_user SEC(".maps");

// per-cgroup system
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CGROUPS * CGROUP_SHARDS);
} cgroup_system SEC(".maps");

// per-cgroup CPU time belonging to tasks that have since exited
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CGROUPS * CGROUP_SHARDS);
} cgroup_exited SEC(".maps");

/*
 * task_start_times holds, per pid, the start_time of the task instance whose
 * counters are live, with METADATA_PENDING set until that instance's
 * task_info has reached userspace. `task->start_time` is nanoseconds of
 * CLOCK_MONOTONIC since boot, so bits 63 and 62 are never part of a real
 * value. 0 is "no instance", the map's initial value.
 *
 * sched_process_exit writes the exiting instance's start_time with
 * INSTANCE_EXITED set and leaves its utime/stime baselines in place. Either
 * of two tasks can then reach handle_new_task with that pid and start_time:
 * the exited task itself, still running the rest of do_exit, or a thread
 * that exec'd from outside the group leader, which de_thread gives the
 * leader's pid and start_time. Neither is a new task whose CPU can be counted
 * from zero (see handle_new_task).
 */
#define METADATA_PENDING (1ULL << 63)
#define INSTANCE_EXITED (1ULL << 62)
#define PF_EXITING 0x00000004

// handle_new_task's returns for the exited instance itself, and for a task
// past exit_notify whose pid changed (see handle_new_task)
#define EXITED_INSTANCE 2
#define SKIP_TASK 3

/*
 * CLOCK_MONOTONIC when these programs first ran, the clock
 * `task->start_time` is on. A task that started after it is counted from
 * zero; one that started before is counted from its first observation, so an
 * agent restart does not credit every running task's lifetime CPU to one
 * tick. Set by the first handle_new_task call; racing CPUs all write a time
 * from that same first moment, so which one wins does not matter.
 */
u64 attach_ns = 0;

/*
 * Per-task export is the config option `task_attribution`, off by default.
 * The accounting below runs either way -- the per-CPU and per-cgroup deltas
 * are computed from each task's utime/stime baseline, and exited tasks' CPU
 * is folded from task_cpu_usage into the exited counters -- so what the
 * switch removes is only what exists for the per-task series: the task_info
 * event (a ringbuf reserve and four string reads per new task), the
 * task_exit event, and the task_rename program, which is not loaded.
 * Userspace does not read task_cpu_usage or drain either ringbuf when it is
 * off. Written into read-only data before load, so the verifier removes the
 * sends rather than testing a flag per event.
 */
const volatile __u8 task_attribution = 0;

/**
 * send_task_info - Send one task's metadata to userspace
 * @task: The task_struct to describe
 *
 * Returns 1 when the event was submitted, 0 when the ringbuf was full.
 */
static __always_inline int send_task_info(struct task_struct* task) {
    // Not exporting: nothing to deliver, so count it as delivered and never
    // leave METADATA_PENDING set.
    if (!task_attribution)
        return 1;

    struct task_info* info = bpf_ringbuf_reserve(&task_info, sizeof(struct task_info), 0);
    if (!info)
        return 0;

    __builtin_memset(info, 0, sizeof(struct task_info));
    populate_task_info(task, info);
    info->ts = bpf_ktime_get_ns();
    bpf_ringbuf_submit(info, 0);
    return 1;
}

/**
 * handle_new_task - Start a new task instance's counters, and deliver its
 * metadata
 * @task: The task_struct to check
 *
 * Returns 0 if this is a new task (or a reused pid), 1 if it is a task
 * already seen, EXITED_INSTANCE for a task after its own sched_process_exit,
 * SKIP_TASK for a task whose events must not be accounted, -1 on error.
 *
 * Accounting and metadata delivery are separate. A new instance has its
 * counters zeroed exactly once, and its start_time committed at the same
 * time, whether or not its task_info fits in the ringbuf. If it does not,
 * METADATA_PENDING stays set and later calls retry the send only. They never
 * touch the counters again, so the deltas computed after this call are
 * credited to the per-CPU and per-cgroup totals as for any other task.
 *
 * It used to commit start_time only after a successful send, so a task whose
 * send failed looked new on every call and had its utime/stime baseline
 * re-zeroed each time. Every delta was then skipped, and its CPU was missing
 * from the per-CPU and per-cgroup totals as well as its own. That held while
 * the ringbuf stayed full, which is up to one snapshot interval, since
 * userspace drains it only when a snapshot is taken. Under heavy thread churn
 * (about 17,900 threads/s, a 262 KiB ringbuf holding about 1,130 task_info
 * events at the 220 bytes they were then) system time came out 69% short
 * against the kernel's cpu.stat; see docs/backlog.md, "Agent — per-task CPU
 * usage completeness".
 *
 * The new instance's utime/stime baseline is set here too, once: zero for a
 * task that started after `attach_ns`, so every tick it runs is counted,
 * including the ones before its first observation; its current utime/stime
 * for a task that predates the agent. It used to be left at zero and the
 * first delta skipped for every task, which lost all the CPU of a thread
 * that lives about one tick (most of a thread-churn workload's), and each
 * field's first non-zero value for any task whose utime or stime was still
 * zero when first seen.
 *
 * A marker of this pid and start_time with INSTANCE_EXITED set is not a new
 * task. If the task has PF_EXITING, it is the exited task in the rest of
 * do_exit: EXITED_INSTANCE is returned, its CPU since the last event is
 * still credited to the per-CPU and per-cgroup totals from the baseline the
 * exit left, and no instance is started. Otherwise a thread has exec'd from
 * outside the group leader and taken the leader's pid and start_time, and
 * this event came before sched_process_exec, which normally starts that
 * instance. Its CPU up to its last event under its own pid was counted
 * there, so its baseline is its current utime/stime, and the CPU accounted
 * in this event is not counted.
 *
 * A thread whose exec fails after de_thread gets no sched_process_exec and
 * exits under the leader's pid with the leader's exited marker in place.
 * Its events between PF_EXITING and its own sched_process_exit are taken for
 * the exited leader's, measured from the leader's baselines; that counts the
 * difference between its utime/stime and the leader's again when positive.
 * Its sched_process_exit then sets the baselines to its own utime/stime. Its
 * old pid's slot is never retired:
 * it keeps its labels, and its per-task total is not folded into the exited
 * counters, until the pid is reused.
 *
 * A task past exit_notify (exit_state set) whose marker is neither its live
 * nor its exited one is not the slot's instance. Usually it is the old group
 * leader, which de_thread gives the exec'ing thread's pid while it finishes
 * do_exit; it can also be a task whose sched_process_exit ran before the
 * agent attached, or one whose freed pid was reused. SKIP_TASK is returned
 * and the event is not accounted.
 *
 * What a failed send still costs: the task is not exported while its
 * metadata is pending, since group membership follows metadata presence
 * (docs/principles.md principle 18). Its CPU is in the totals meanwhile, and
 * in the exited counters if it exits before a send succeeds.
 */
//
// The caller reads `pid` and `start_time`, directly where its `task` is a BTF
// pointer, and passes them in; the reads left here are on the new-task path.
static __noinline int handle_new_task(struct task_struct* task, u32 pid, u64 start_time) {
    if (!task)
        return -1;

    if (pid == 0 || pid >= MAX_PID)
        return -1;

    u32 slot = pid_slot(pid);

    u64* last_start = bpf_map_lookup_elem(&task_start_times, &slot);
    if (!last_start)
        return -1;

    u64 marker = *last_start;

    // The instance whose counters are live: only its metadata may be owed.
    if ((marker & ~METADATA_PENDING) == start_time) {
        if ((marker & METADATA_PENDING) && send_task_info(task)) {
            bpf_map_update_elem(&task_start_times, &slot, &start_time, BPF_ANY);
        }
        return 1;
    }

    bool exec_takeover = false;
    if (marker == (start_time | INSTANCE_EXITED)) {
        if (BPF_CORE_READ(task, flags) & PF_EXITING)
            return EXITED_INSTANCE;
        exec_takeover = true;
    } else if (BPF_CORE_READ(task, exit_state)) {
        return SKIP_TASK;
    }

    // New task or PID reuse: start its counters, once.
    u64 now = bpf_ktime_get_ns();
    if (attach_ns == 0)
        attach_ns = now;

    u64 zero = 0;
    if (start_time >= attach_ns && !exec_takeover) {
        bpf_map_update_elem(&task_utime, &slot, &zero, BPF_ANY);
        bpf_map_update_elem(&task_stime, &slot, &zero, BPF_ANY);
    } else {
        u64 utime = BPF_CORE_READ(task, utime);
        u64 stime = BPF_CORE_READ(task, stime);
        bpf_map_update_elem(&task_utime, &slot, &utime, BPF_ANY);
        bpf_map_update_elem(&task_stime, &slot, &stime, BPF_ANY);
    }
    bpf_map_update_elem(&task_cpu_usage, &slot, &zero, BPF_ANY);

    marker = start_time;
    if (!send_task_info(task))
        marker |= METADATA_PENDING;
    bpf_map_update_elem(&task_start_times, &slot, &marker, BPF_ANY);

    return 0;
}

// The kprobe handler is not always invoked, so using the delta to count the CPU usage could cause
// undercounting. Kernel increases the task utime/stime before invoking cpuacct_account_field. So we
// count the CPU usage by tracking the per-task utime/stime. The user time includes both the
// CPUTIME_NICE and CPUTIME_USER. The system time includes CPUTIME_SYSTEM, CPUTIME_SOFTIRQ and
// CPUTIME_IRQ.
// fentry/kprobe twins share this handler; only the attach mechanism differs.
// fentry is the cheaper dispatch (see
// docs/journal/2026-09-04-fentry-vs-kprobe-dispatch.md) but needs BTF, so the
// kprobe twin is the fallback and one is disabled at load time on
// kernel_can_fentry() (see disabled_programs/required_programs in mod.rs). Only
// `task` is used -- the kernel bumps task->utime/stime before calling
// cpuacct_account_field, so we read those rather than the passed index/delta.
// `btf` is a compile-time constant: true in the fentry program, whose `task`
// is a BTF pointer (see task_group_of() in cgroup.h).
static __always_inline int handle_cpuacct_account_field(struct task_struct* task, bool btf) {
    u32 cpu, idx;
    u64 curr_utime, curr_stime;
    u64 *last_utime, *last_stime;
    u32 pid;

    if (!task)
        return 0;

    pid = BTF_READ(btf, task, pid);
    if (pid == 0 || pid >= MAX_PID)
        return 0;

    u32 slot = pid_slot(pid);

    int seen = handle_new_task(task, pid, BTF_READ(btf, task, start_time));
    if (seen == SKIP_TASK)
        return 0;

    curr_utime = BTF_READ(btf, task, utime);
    curr_stime = BTF_READ(btf, task, stime);

    last_utime = bpf_map_lookup_elem(&task_utime, &slot);
    last_stime = bpf_map_lookup_elem(&task_stime, &slot);

    if (!last_utime || !last_stime)
        return 0;

    // Calculate deltas with overflow protection
    u64 delta_utime = 0;
    u64 delta_stime = 0;

    // The baseline is set once per task instance by handle_new_task: zero
    // for a task started after the agent attached, its utime/stime at first
    // sight for one started before (so an agent restart does not credit a
    // running task's whole lifetime to one tick). Every delta from it counts.
    if (curr_utime >= *last_utime) {
        delta_utime = curr_utime - *last_utime;
    }

    if (curr_stime >= *last_stime) {
        delta_stime = curr_stime - *last_stime;
    }

    *last_utime = curr_utime;
    *last_stime = curr_stime;

    // Skip updating metrics if there's no change
    if (delta_utime == 0 && delta_stime == 0)
        return 0;

    cpu = bpf_get_smp_processor_id();
    if (cpu >= MAX_CPUS)
        return 0;

    if (delta_utime > 0) {
        idx = CPU_USAGE_GROUP_WIDTH * cpu + USER_OFFSET;
        if (idx < MAX_CPUS * CPU_USAGE_GROUP_WIDTH) {
            array_add(&cpu_usage, idx, delta_utime);
        }
    }

    if (delta_stime > 0) {
        idx = CPU_USAGE_GROUP_WIDTH * cpu + SYSTEM_OFFSET;
        if (idx < MAX_CPUS * CPU_USAGE_GROUP_WIDTH) {
            array_add(&cpu_usage, idx, delta_stime);
        }
    }

    // An exited instance's per-task total was folded into the exited
    // counters at its exit; what it runs afterwards is in the totals only.
    u64 delta_total = delta_utime + delta_stime;
    if (delta_total > 0 && seen != EXITED_INSTANCE) {
        array_add(&task_cpu_usage, slot, delta_total);
    }

    if (!cgroup_attribution)
        return 0;

    u32 cgroup_id = 0;
    u64 serial_nr = 0;
    struct task_group* tg = task_group_of(task, btf, &cgroup_id, &serial_nr);
    if (!tg || cgroup_id >= MAX_CGROUPS)
        return 0;

    int ret = handle_new_cgroup_read(&tg->css, cgroup_id, serial_nr, &cgroup_serial_numbers,
                                     &cgroup_info);
    if (ret == 0) {
        // New cgroup detected, zero the counters
        cgroup_zero(&cgroup_user, cgroup_id);
        cgroup_zero(&cgroup_system, cgroup_id);
        cgroup_zero(&cgroup_exited, cgroup_id);
    }

    if (delta_utime > 0) {
        cgroup_add(&cgroup_user, cgroup_id, delta_utime);
    }

    if (delta_stime > 0) {
        cgroup_add(&cgroup_system, cgroup_id, delta_stime);
    }

    return 0;
}

SEC("fentry/cpuacct_account_field")
int BPF_PROG(cpuacct_account_field_fentry, struct task_struct* task, u32 index, u64 delta) {
    return handle_cpuacct_account_field(task, true);
}

SEC("kprobe/cpuacct_account_field")
int BPF_KPROBE(cpuacct_account_field_kprobe, struct task_struct* task, u32 index, u64 delta) {
    return handle_cpuacct_account_field(task, false);
}

/*
 * Retire a task instance's per-task counter: fold its total into the exited
 * counters, zero it, and tell userspace the slot is free.
 *
 * The fold keeps the books reconcilable -- sum(live tasks) + exited ~=
 * cgroup total -- at O(1) cost and with no additional cardinality. Without
 * it the time would be gone from the task view: the cgroup and host totals
 * still include it, but no per-task series accounts for it.
 *
 * The counter is zeroed whether or not the task_exit event fits in the
 * ringbuf. A dropped event leaves the slot's old labels in userspace with an
 * unchanging 0 until the pid is reused and handle_new_task sends the new
 * task's labels.
 */
static __always_inline void retire_task(struct task_struct* task, bool btf, u32 pid, u32 slot) {
    u64* usage = bpf_map_lookup_elem(&task_cpu_usage, &slot);
    if (usage && *usage) {
        u32 cpu = bpf_get_smp_processor_id();

        if (cpu < MAX_CPUS) {
            array_add(&cpu_usage, CPU_USAGE_GROUP_WIDTH * cpu + EXITED_OFFSET, *usage);
        }

        u32 cgroup_id = 0;
        u64 serial_nr = 0;
        struct task_group* tg =
            cgroup_attribution ? task_group_of(task, btf, &cgroup_id, &serial_nr) : NULL;
        if (tg) {
            if (cgroup_id > 0 && cgroup_id < MAX_CGROUPS) {
                cgroup_add(&cgroup_exited, cgroup_id, *usage);
            }
        }
    }

    // Zero the exported counter before the exit event, so no value is
    // exported without metadata.
    u64 zero = 0;
    bpf_map_update_elem(&task_cpu_usage, &slot, &zero, BPF_ANY);

    if (!task_attribution)
        return;

    struct task_exit* exit_event = bpf_ringbuf_reserve(&task_exit, sizeof(struct task_exit), 0);
    if (exit_event) {
        __builtin_memset(exit_event, 0, sizeof(struct task_exit));
        exit_event->pid = pid;
        exit_event->ts = bpf_ktime_get_ns();
        bpf_ringbuf_submit(exit_event, 0);
    }
}

static __always_inline int account__sched_process_exit(u64* ctx, bool btf) {
    /* TP_PROTO(struct task_struct *p), and from 6.16 (p, bool group_dead) */
    struct task_struct* task = (struct task_struct*)ctx[0];

    u32 pid = BTF_READ(btf, task, pid);
    if (pid == 0 || pid >= MAX_PID)
        return 0;

    u32 slot = pid_slot(pid);
    u64 start_time = BTF_READ(btf, task, start_time);

    // Mark the instance exited rather than clearing the slot; see
    // INSTANCE_EXITED. The baselines stay as the last event left them, so
    // CPU the task uses in the rest of do_exit is counted once. For a task
    // never seen live they belong to an earlier instance, so set them as
    // handle_new_task would have for this one. That earlier instance can be
    // the old group leader, with this start_time: a thread whose exec fails
    // after de_thread gets no sched_process_exec and exits under the
    // leader's pid with the leader's exited marker in place. Its CPU so far
    // was counted under its own pid.
    u64* marker = bpf_map_lookup_elem(&task_start_times, &slot);
    if (!marker) {
        retire_task(task, btf, pid, slot);
        return 0;
    }

    u64 m = *marker;
    if ((m & ~METADATA_PENDING) != start_time) {
        u64 utime = 0, stime = 0;
        if (attach_ns == 0 || start_time < attach_ns || m == (start_time | INSTANCE_EXITED)) {
            utime = BTF_READ(btf, task, utime);
            stime = BTF_READ(btf, task, stime);
        }
        bpf_map_update_elem(&task_utime, &slot, &utime, BPF_ANY);
        bpf_map_update_elem(&task_stime, &slot, &stime, BPF_ANY);
    }

    u64 exited = start_time | INSTANCE_EXITED;
    bpf_map_update_elem(&task_start_times, &slot, &exited, BPF_ANY);

    // After the marker: an event nested in this program on this CPU then
    // finds the instance exited, so it neither adds to the per-task counter
    // after the fold nor sends a task_info stamped after the task_exit.
    retire_task(task, btf, pid, slot);

    return 0;
}

/*
 * A thread that execs from outside the group leader takes the leader's pid
 * and start_time in de_thread; its own pid, `old_pid` here, is released with
 * the old leader and gets no sched_process_exit.
 *
 * Start the thread's instance under the leader's pid, carrying the baselines
 * from its old slot, so its CPU is counted once and none is skipped. This is
 * done only when the leader's slot still holds the leader's exited marker;
 * an event that reached handle_new_task first has already started it. Then
 * retire the old slot as an exit would.
 *
 * old_pid can be reused between de_thread and this tracepoint only after
 * pid_max further allocations. The new task's slot is then retired instead:
 * its labels are released, its CPU so far is folded into the exec'ing task's
 * cgroup, its next event restarts it, counting that CPU again in the totals
 * when it started after attach_ns, and the exec'd thread's baselines are
 * copied from the new task's slot.
 */
static __always_inline int account__sched_process_exec(u64* ctx, bool btf) {
    /* TP_PROTO(struct task_struct *p, pid_t old_pid, struct linux_binprm *bprm) */
    struct task_struct* task = (struct task_struct*)ctx[0];
    u32 old_pid = (u32)ctx[1];

    u32 pid = BTF_READ(btf, task, pid);
    if (old_pid == pid || old_pid == 0 || old_pid >= MAX_PID)
        return 0;

    u32 slot = pid_slot(old_pid);
    u32 new_slot = pid_slot(pid);
    u64 start_time = BTF_READ(btf, task, start_time);

    u64* marker = bpf_map_lookup_elem(&task_start_times, &new_slot);
    if (marker && *marker == (start_time | INSTANCE_EXITED)) {
        u64 utime = BTF_READ(btf, task, utime);
        u64 stime = BTF_READ(btf, task, stime);

        // The old slot's baselines are this thread's last event under its
        // own pid, when that slot holds a live instance. One started with
        // this start_time is the old leader's, not this thread's (see
        // SKIP_TASK in handle_new_task).
        u64* old_marker = bpf_map_lookup_elem(&task_start_times, &slot);
        u64* old_utime = bpf_map_lookup_elem(&task_utime, &slot);
        u64* old_stime = bpf_map_lookup_elem(&task_stime, &slot);
        if (old_marker && old_utime && old_stime && *old_marker != 0 &&
            !(*old_marker & INSTANCE_EXITED) &&
            (*old_marker & ~METADATA_PENDING) != start_time) {
            utime = *old_utime;
            stime = *old_stime;
        }

        u64 zero = 0;
        bpf_map_update_elem(&task_utime, &new_slot, &utime, BPF_ANY);
        bpf_map_update_elem(&task_stime, &new_slot, &stime, BPF_ANY);
        bpf_map_update_elem(&task_cpu_usage, &new_slot, &zero, BPF_ANY);

        u64 live = start_time;
        if (!send_task_info(task))
            live |= METADATA_PENDING;
        bpf_map_update_elem(&task_start_times, &new_slot, &live, BPF_ANY);
    }

    retire_task(task, btf, old_pid, slot);

    u64 zero = 0;
    bpf_map_update_elem(&task_start_times, &slot, &zero, BPF_ANY);

    return 0;
}

/*
 * A live task's comm changed: pthread_setname_np, prctl(PR_SET_NAME), a write
 * to /proc/<pid>/task/<tid>/comm, or exec. Send its task_info again so its
 * series is relabelled; handle_new_task reads comm only when the task is
 * first seen. The tracepoint fires in __set_task_comm before the new name is
 * copied into task->comm, so the name is taken from the `comm` argument.
 *
 * Only for the instance whose first task_info has been submitted: marker
 * equal to start_time, METADATA_PENDING clear. One with METADATA_PENDING set
 * gets its first send from a later handle_new_task, which reads task->comm.
 * A rename whose send finds the ringbuf full sets METADATA_PENDING, and
 * handle_new_task's retry reads task->comm.
 *
 * Two windows can leave the old name in place until the next rename: a
 * handle_new_task (first send or retry) that runs between this tracepoint
 * and the copy reads the old task->comm; and from 6.14, which no longer
 * takes task_lock in __set_task_comm, two concurrent renames of one thread
 * can be traced in one order and copied in the other.
 *
 * Loaded only with task_attribution on (see disabled_programs in mod.rs).
 */
static __always_inline int account__task_rename(u64* ctx, bool btf) {
    /* TP_PROTO(struct task_struct *task, const char *comm) */
    struct task_struct* task = (struct task_struct*)ctx[0];
    const char* comm = (const char*)ctx[1];

    u32 pid = BTF_READ(btf, task, pid);
    if (pid == 0 || pid >= MAX_PID)
        return 0;

    u32 slot = pid_slot(pid);

    // Stamped before the marker is read, so an exit on another CPU that
    // marks the instance after this check is, in most interleavings, also
    // stamped after this event (see ASSIGNED_AT in mod.rs).
    u64 ts = bpf_ktime_get_ns();

    u64* marker = bpf_map_lookup_elem(&task_start_times, &slot);
    if (!marker)
        return 0;

    u64 start_time = BTF_READ(btf, task, start_time);
    if (*marker != start_time)
        return 0;

    struct task_info* info = bpf_ringbuf_reserve(&task_info, sizeof(struct task_info), 0);
    if (!info) {
        u64 pending = start_time | METADATA_PENDING;
        bpf_map_update_elem(&task_start_times, &slot, &pending, BPF_ANY);
        return 0;
    }

    __builtin_memset(info, 0, sizeof(struct task_info));
    populate_task_info(task, info);
    __builtin_memset(&info->comm, 0, TASK_COMM_LEN);
    bpf_probe_read_kernel_str(&info->comm, TASK_COMM_LEN, comm);
    info->ts = ts;
    bpf_ringbuf_submit(info, 0);

    return 0;
}

SEC("tp_btf/task_rename")
int handle__task_rename_btf(u64* ctx) {
    return account__task_rename(ctx, true);
}

SEC("raw_tp/task_rename")
int handle__task_rename_raw(u64* ctx) {
    return account__task_rename(ctx, false);
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

    start_ts = bpf_map_lookup_elem(&softirq_start, &start_idx);

    // possible we missed the start
    if (!start_ts || *start_ts == 0) {
        return 0;
    }

    // the current task's pid, the low half of pid_tgid, without a kernel read
    u32 pid = (u32)bpf_get_current_pid_tgid();

    dur = bpf_ktime_get_ns() - *start_ts;

    idx = SOFTIRQ_GROUP_WIDTH * cpu + args->vec;
    array_add(&softirq_time, idx, dur);
    if (pid == 0) {
        cpuusage_idx = CPU_USAGE_GROUP_WIDTH * cpu + SYSTEM_OFFSET;
        array_add(&cpu_usage, cpuusage_idx, dur);
    }

    *start_ts = 0;

    return 0;
}

SEC("tp_btf/sched_process_exit")
int handle__sched_process_exit_btf(u64* ctx) {
    return account__sched_process_exit(ctx, true);
}

SEC("raw_tp/sched_process_exit")
int handle__sched_process_exit_raw(u64* ctx) {
    return account__sched_process_exit(ctx, false);
}

SEC("tp_btf/sched_process_exec")
int handle__sched_process_exec_btf(u64* ctx) {
    return account__sched_process_exec(ctx, true);
}

SEC("raw_tp/sched_process_exec")
int handle__sched_process_exec_raw(u64* ctx) {
    return account__sched_process_exec(ctx, false);
}

char LICENSE[] SEC("license") = "GPL";
