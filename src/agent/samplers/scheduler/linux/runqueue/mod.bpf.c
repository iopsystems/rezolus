// SPDX-License-Identifier: GPL-2.0
// Copyright (c) 2019 Facebook
// Copyright (c) 2023 The Rezolus Authors

// NOTICE: this file is based off `runqslower.bpf.c` from the BCC project
// <https://github.com/iovisor/bcc/> and has been modified for use within
// Rezolus.

// This BPF program probes enqueue and dequeue from the scheduler runqueue
// to calculate the runqueue latency, running time, and off-cpu time.

#include <vmlinux.h>
#include "../../../agent/bpf/cgroup.h"
#include "../../../agent/bpf/helpers.h"
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_helpers.h>

#define COUNTER_GROUP_WIDTH 8
#define HISTOGRAM_BUCKETS HISTOGRAM_BUCKETS_POW_3
#define HISTOGRAM_POWER 3
#define MAX_CPUS 1024
#define MAX_PID 4194304

#define TASK_RUNNING 0

// counter positions
#define IVCSW 0
#define RUNQ_WAIT 1
#define DISCARDED 2

// counters (see constants defined at top)
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CPUS* COUNTER_GROUP_WIDTH);
} counters SEC(".maps");

/*
 * tracking structs
 */

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(max_entries, MAX_PID);
    __type(key, u32);
    __type(value, u64);
} enqueued_at SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(max_entries, MAX_PID);
    __type(key, u32);
    __type(value, u64);
} offcpu_at SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(max_entries, MAX_PID);
    __type(key, u32);
    __type(value, u64);
} running_at SEC(".maps");

/*
 * cgroup tracking
 */

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

/*
 * system histograms
 */

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS);
} runqlat SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS);
} running SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS);
} offcpu SEC(".maps");

/*
 * cgroup counters
 */

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CGROUPS);
} cgroup_ivcsw SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CGROUPS);
} cgroup_runq_wait SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CGROUPS);
} cgroup_offcpu SEC(".maps");

/* record enqueue timestamp */
static __always_inline int trace_enqueue(u32 tgid, u32 pid) {
    u64 ts;

    if (!pid) {
        return 0;
    }

    ts = bpf_ktime_get_ns();
    bpf_map_update_elem(&enqueued_at, &pid, &ts, 0);
    return 0;
}

static __always_inline int account__sched_wakeup(u64* ctx) {
    /* TP_PROTO(struct task_struct *p) */
    struct task_struct* p = (void*)ctx[0];

    return trace_enqueue(BPF_CORE_READ(p, tgid), BPF_CORE_READ(p, pid));
}

static __always_inline int account__sched_wakeup_new(u64* ctx) {
    /* TP_PROTO(struct task_struct *p) */
    struct task_struct* p = (void*)ctx[0];

    return trace_enqueue(BPF_CORE_READ(p, tgid), BPF_CORE_READ(p, pid));
}

static __always_inline int account__sched_switch(u64* ctx) {
    /* TP_PROTO(bool preempt, struct task_struct *prev,
     *      struct task_struct *next)
     */
    struct task_struct* prev = (struct task_struct*)ctx[1];
    struct task_struct* next = (struct task_struct*)ctx[2];

    u32 idx;
    // prev and next can belong to different cgroups; track each separately so
    // runqueue wait and off-cpu time are never charged to prev's cgroup.
    // MAX_CGROUPS is the "no attribution" sentinel.
    u32 prev_cgroup_id = MAX_CGROUPS;
    u32 next_cgroup_id = MAX_CGROUPS;
    u64 *tsp, delta_ns, offcpu_ns;

    u32 processor_id = bpf_get_smp_processor_id();
    u64 ts = bpf_ktime_get_ns();

    // The idle task (pid 0) is not a runqueue participant: it never waits to be
    // scheduled, and unlike a real task its slot in the per-pid arrays below is
    // shared by every CPU. Tracking it fabricates runqueue wait for the root
    // cgroup, and because `ts` is sampled here but consumed further down, it
    // lets a remote CPU publish a newer timestamp into slot 0 mid-handler --
    // making `ts - *tsp` underflow into the top histogram bucket. Measured on a
    // 32-core host: 65% of the writes below were the idle task, and the top
    // bucket accrued 59-189 samples/s while the machine was otherwise idle.
    // `trace_enqueue()` already skips pid 0 on the wakeup path; skipping it here
    // keeps the switch path consistent with it.
    u32 prev_pid = BPF_CORE_READ(prev, pid);
    u32 next_pid = BPF_CORE_READ(next, pid);

    // read the prev task cgroup details and push to ringbuf if new cgroup
    void* prev_task_group = BPF_CORE_READ(prev, sched_task_group);
    if (prev_task_group) {
        u32 id = BPF_CORE_READ(prev, sched_task_group, css.id);

        if (id < MAX_CGROUPS) {
            prev_cgroup_id = id;

            int ret = handle_new_cgroup(prev, &cgroup_serial_numbers, &cgroup_info);

            if (ret == 0) {
                // New cgroup detected, zero the counters
                u64 zero = 0;
                bpf_map_update_elem(&cgroup_ivcsw, &prev_cgroup_id, &zero, BPF_ANY);
                bpf_map_update_elem(&cgroup_runq_wait, &prev_cgroup_id, &zero, BPF_ANY);
                bpf_map_update_elem(&cgroup_offcpu, &prev_cgroup_id, &zero, BPF_ANY);
            }
        }
    }

    // if prev was TASK_RUNNING, calculate how long prev was running, increment hist
    // if prev was TASK_RUNNING, increment ivcsw counter
    // if prev was TASK_RUNNING, trace enqueue of prev

    // prev task is moving from running
    // - update prev->pid enqueued_at with now
    // - calculate how long prev task was running and update hist
    if (get_task_state(prev) == TASK_RUNNING) {
        idx = COUNTER_GROUP_WIDTH * processor_id + IVCSW;
        array_incr(&counters, idx);

        if (prev_cgroup_id < MAX_CGROUPS) {
            array_incr(&cgroup_ivcsw, prev_cgroup_id);
        }

        if (prev_pid) {
            bpf_map_update_elem(&enqueued_at, &prev_pid, &ts, 0);

            tsp = bpf_map_lookup_elem(&running_at, &prev_pid);
            if (tsp && *tsp) {
                // A timestamp pair can arrive out of order across CPUs; an
                // unguarded subtraction would wrap to ~2^64 and land in the top
                // bucket. Discard instead, and count it so the condition stays
                // observable rather than silently vanishing.
                if (ts >= *tsp) {
                    histogram_incr(&running, HISTOGRAM_POWER, ts - *tsp);
                } else {
                    array_incr(&counters, COUNTER_GROUP_WIDTH * processor_id + DISCARDED);
                }

                *tsp = 0;
            }
        }
    }

    // for all tasks: track when it went off-cpu
    if (prev_pid) {
        bpf_map_update_elem(&offcpu_at, &prev_pid, &ts, 0);
    }

    // next task has moved into running
    // - update next->pid running_at with now
    // - calculate how long next task was enqueued, update hist

    // read the next task cgroup details and push to ringbuf if new cgroup
    void* next_task_group = BPF_CORE_READ(next, sched_task_group);
    if (next_task_group) {
        u32 id = BPF_CORE_READ(next, sched_task_group, css.id);

        if (id < MAX_CGROUPS) {
            next_cgroup_id = id;

            int ret = handle_new_cgroup(next, &cgroup_serial_numbers, &cgroup_info);

            if (ret == 0) {
                // New cgroup detected, zero the counters
                u64 zero = 0;
                bpf_map_update_elem(&cgroup_ivcsw, &next_cgroup_id, &zero, BPF_ANY);
                bpf_map_update_elem(&cgroup_runq_wait, &next_cgroup_id, &zero, BPF_ANY);
                bpf_map_update_elem(&cgroup_offcpu, &next_cgroup_id, &zero, BPF_ANY);
            }
        }
    }

    if (next_pid) {
        bpf_map_update_elem(&running_at, &next_pid, &ts, 0);

        tsp = bpf_map_lookup_elem(&enqueued_at, &next_pid);
        if (tsp && *tsp) {
            if (ts >= *tsp) {
                delta_ns = ts - *tsp;

                histogram_incr(&runqlat, HISTOGRAM_POWER, delta_ns);

                idx = COUNTER_GROUP_WIDTH * processor_id + RUNQ_WAIT;
                array_add(&counters, idx, delta_ns);

                if (next_cgroup_id < MAX_CGROUPS) {
                    array_add(&cgroup_runq_wait, next_cgroup_id, delta_ns);
                }

                *tsp = 0;

                // calculate how long it was off-cpu, not including runqueue wait,
                // and increment stats
                tsp = bpf_map_lookup_elem(&offcpu_at, &next_pid);
                if (tsp && *tsp) {
                    if (ts >= *tsp) {
                        offcpu_ns = ts - *tsp;

                        if (offcpu_ns > delta_ns) {
                            offcpu_ns = offcpu_ns - delta_ns;

                            histogram_incr(&offcpu, HISTOGRAM_POWER, offcpu_ns);

                            if (next_cgroup_id < MAX_CGROUPS) {
                                array_add(&cgroup_offcpu, next_cgroup_id, offcpu_ns);
                            }
                        }
                    } else {
                        array_incr(&counters, COUNTER_GROUP_WIDTH * processor_id + DISCARDED);
                    }

                    *tsp = 0;
                }
            } else {
                array_incr(&counters, COUNTER_GROUP_WIDTH * processor_id + DISCARDED);

                *tsp = 0;
            }
        }
    }

    return 0;
}

SEC("tp_btf/sched_wakeup")
int handle__sched_wakeup_btf(u64* ctx) {
    return account__sched_wakeup(ctx);
}

SEC("raw_tp/sched_wakeup")
int handle__sched_wakeup_raw(u64* ctx) {
    return account__sched_wakeup(ctx);
}

SEC("tp_btf/sched_wakeup_new")
int handle__sched_wakeup_new_btf(u64* ctx) {
    return account__sched_wakeup_new(ctx);
}

SEC("raw_tp/sched_wakeup_new")
int handle__sched_wakeup_new_raw(u64* ctx) {
    return account__sched_wakeup_new(ctx);
}

SEC("tp_btf/sched_switch")
int handle__sched_switch_btf(u64* ctx) {
    return account__sched_switch(ctx);
}

SEC("raw_tp/sched_switch")
int handle__sched_switch_raw(u64* ctx) {
    return account__sched_switch(ctx);
}

char LICENSE[] SEC("license") = "GPL";
