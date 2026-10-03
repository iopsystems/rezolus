// SPDX-License-Identifier: GPL-2.0
// Copyright (c) 2019 Facebook
// Copyright (c) 2023 The Rezolus Authors

// NOTICE: this file is based off `runqslower.bpf.c` from the BCC project
// <https://github.com/iovisor/bcc/> and has been modified for use within
// Rezolus.

// One program per scheduler hook for the `scheduler` sampler. The wakeups
// stamp a task's enqueue, and `sched_switch` measures runqueue latency,
// running time, off-cpu time and context switches (the part `runqueue`) and
// counts CPU migrations (the part `migrations`). Each part is switched by
// read-only data written before load, so a part that is off is not in the
// loaded program. Before this `scheduler_runqueue` and `cpu_migrations` were
// two samplers with a program each on `sched_switch`, and each program on a
// hook pays its own dispatch (docs/journal/2026-10-03-one-program-per-hook.md).

#include <vmlinux.h>
#include "../../../agent/bpf/btf_read.h"
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

// Per-cgroup attribution is the config option `cgroup_attribution`, on by
// default for this sampler. Written into read-only data before load, so with
// it off the verifier removes the per-cgroup path (the task-group read, the
// new-cgroup check and the per-cgroup adds) rather than testing a flag on
// every event.
const volatile __u8 cgroup_attribution = 0;

// The sampler's parts, the config options `runqueue` and `migrations` (both on
// by default). With `runqueue` off the wakeup programs are not loaded.
const volatile __u8 runqueue = 0;
const volatile __u8 migrations = 0;

// counter positions
#define IVCSW 0
#define RUNQ_WAIT 1
#define DISCARDED 2
#define VCSW 3

// migration counter positions
#define FROM 0
#define TO 1

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
} cgroup_vcsw SEC(".maps");

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

/*
 * migrations
 */

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CPUS* COUNTER_GROUP_WIDTH);
} migrations_counts SEC(".maps");

// per-cgroup migration counts
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CGROUPS);
} cgroup_cpu_migrations SEC(".maps");

// For storing the CPU a process was last seen on
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(max_entries, MAX_PID);
    __type(key, u32);   // pid
    __type(value, u32); // cpu
} last_cpu SEC(".maps");

// `btf` is a compile-time constant: true in the tp_btf program, whose task
// arguments are BTF pointers (see task_group_of() in cgroup.h).

/* record enqueue timestamp */
static __always_inline int trace_enqueue(u32 pid) {
    u64 ts;

    if (!runqueue || !pid) {
        return 0;
    }

    ts = bpf_ktime_get_ns();
    bpf_map_update_elem(&enqueued_at, &pid, &ts, 0);
    return 0;
}

// `btf` is a compile-time constant: true in the tp_btf programs, whose task
// arguments are BTF pointers (see BTF_READ in btf_read.h).
static __always_inline int account__sched_wakeup(u64* ctx, bool btf) {
    /* TP_PROTO(struct task_struct *p) */
    struct task_struct* p = (void*)ctx[0];

    return trace_enqueue(BTF_READ(btf, p, pid));
}

static __always_inline int account__sched_wakeup_new(u64* ctx, bool btf) {
    /* TP_PROTO(struct task_struct *p) */
    struct task_struct* p = (void*)ctx[0];

    return trace_enqueue(BTF_READ(btf, p, pid));
}

// The cgroup id of `task`, or MAX_CGROUPS when it has none to count against.
// A cgroup seen for the first time is sent to userspace and its counters are
// zeroed, for each part that is on: a part that is off has no maps.
static __always_inline u32 resolve_cgroup(struct task_struct* task, bool btf) {
    u32 id = 0;
    u64 serial_nr = 0;
    struct task_group* tg = task_group_of(task, btf, &id, &serial_nr);
    if (!tg || id >= MAX_CGROUPS) {
        return MAX_CGROUPS;
    }

    if (handle_new_cgroup_read(&tg->css, id, serial_nr, &cgroup_serial_numbers, &cgroup_info) ==
        0) {
        u64 zero = 0;
        if (runqueue) {
            bpf_map_update_elem(&cgroup_ivcsw, &id, &zero, BPF_ANY);
            bpf_map_update_elem(&cgroup_vcsw, &id, &zero, BPF_ANY);
            bpf_map_update_elem(&cgroup_runq_wait, &id, &zero, BPF_ANY);
            bpf_map_update_elem(&cgroup_offcpu, &id, &zero, BPF_ANY);
        }
        if (migrations) {
            bpf_map_update_elem(&cgroup_cpu_migrations, &id, &zero, BPF_ANY);
        }
    }

    return id;
}

// The runqueue part of a switch. Returns next's cgroup id (MAX_CGROUPS when
// not attributed), which the migrations part reuses.
// `btf` is a compile-time constant: true in the tp_btf program, whose task
// arguments are BTF pointers (see task_group_of() in cgroup.h).
static __always_inline u32 runqueue_switch(struct task_struct* prev, struct task_struct* next,
                                           u32 next_pid, u32 processor_id, bool btf) {
    u32 idx;
    // prev and next can belong to different cgroups; track each separately so
    // runqueue wait and off-cpu time are never charged to prev's cgroup.
    // MAX_CGROUPS is the "no attribution" sentinel.
    u32 prev_cgroup_id = MAX_CGROUPS;
    u32 next_cgroup_id = MAX_CGROUPS;
    u64 *tsp, delta_ns, offcpu_ns;

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
    u32 prev_pid = BTF_READ(btf, prev, pid);

    // read the prev task cgroup details and push to ringbuf if new cgroup
    if (cgroup_attribution) {
        prev_cgroup_id = resolve_cgroup(prev, btf);
    }

    // if prev was TASK_RUNNING, calculate how long prev was running, increment hist
    // if prev was TASK_RUNNING, increment ivcsw counter
    // if prev was TASK_RUNNING, trace enqueue of prev

    // prev task is moving from running
    // - update prev->pid enqueued_at with now
    // - calculate how long prev task was running and update hist
    if (get_task_state_btf(prev, btf) == TASK_RUNNING) {
        // The idle task is always TASK_RUNNING, so a CPU simply waking up to run
        // something counted as an involuntary context switch -- nothing was
        // competing for the CPU, and no task was preempted. The inflation is
        // load-dependent and largest exactly where a high context-switch rate is
        // most likely to be misread as contention: on a 32-core host it was 65%
        // of such switches when otherwise idle, and ~30% under load. The idle
        // task is excluded here for the same reason it is excluded from the
        // timing paths below.
        if (prev_pid) {
            idx = COUNTER_GROUP_WIDTH * processor_id + IVCSW;
            array_incr(&counters, idx);

            if (prev_cgroup_id < MAX_CGROUPS) {
                array_incr(&cgroup_ivcsw, prev_cgroup_id);
            }

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
    } else {
        // prev left the CPU while not runnable, i.e. it blocked: a voluntary
        // context switch. This mirrors the kernel's own split, which counts
        // nvcsw when a task deschedules with a non-zero state and nivcsw when
        // it is preempted while runnable. Emitting both classes is what lets a
        // consumer tell a blocking wakeup handoff from a true preemption --
        // with only one class emitted, "no voluntary switches" and "voluntary
        // switches not measured" are indistinguishable.
        idx = COUNTER_GROUP_WIDTH * processor_id + VCSW;
        array_incr(&counters, idx);

        if (prev_cgroup_id < MAX_CGROUPS) {
            array_incr(&cgroup_vcsw, prev_cgroup_id);
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
    if (cgroup_attribution) {
        next_cgroup_id = resolve_cgroup(next, btf);
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

    return next_cgroup_id;
}


// The migrations part of a switch. `next_cgroup_id` is next's cgroup when the
// runqueue part already resolved it (`next_cgroup_known`); otherwise it is
// resolved here, and only on a migration, as `cpu_migrations` did.
static __always_inline void migrations_switch(struct task_struct* next, u32 next_pid, u32 cpu,
                                              u32 next_cgroup_id, bool next_cgroup_known,
                                              bool btf) {
    // Skip kernel threads and idle task (pid 0)
    if (next_pid == 0) {
        return;
    }

    // find the last cpu the task ran on
    u32* last_cpu_ptr = bpf_map_lookup_elem(&last_cpu, &next_pid);

    // check the ptr and that the last cpu is known (it is stored one-indexed)
    if (last_cpu_ptr && *last_cpu_ptr) {
        // convert to zero-indexed
        u32 old_cpu = *last_cpu_ptr - 1;

        // check if this is a migration
        if (old_cpu != cpu) {
            u32 from_idx = old_cpu * COUNTER_GROUP_WIDTH + FROM;
            u32 to_idx = cpu * COUNTER_GROUP_WIDTH + TO;

            array_incr(&migrations_counts, from_idx);
            array_incr(&migrations_counts, to_idx);

            // handle per-cgroup accounting
            if (cgroup_attribution) {
                u32 cgroup_id = next_cgroup_known ? next_cgroup_id : resolve_cgroup(next, btf);
                if (cgroup_id < MAX_CGROUPS) {
                    array_incr(&cgroup_cpu_migrations, cgroup_id);
                }
            }
        }
    }

    // store the current cpu for the next task (converted to one-indexed)
    u32 stored = cpu + 1;
    bpf_map_update_elem(&last_cpu, &next_pid, &stored, BPF_ANY);
}

// `btf` is a compile-time constant: true in the tp_btf program.
static __always_inline int account__sched_switch(u64* ctx, bool btf) {
    /* TP_PROTO(bool preempt, struct task_struct *prev,
     *      struct task_struct *next)
     */
    struct task_struct* prev = (struct task_struct*)ctx[1];
    struct task_struct* next = (struct task_struct*)ctx[2];

    u32 processor_id = bpf_get_smp_processor_id();
    u32 next_pid = BTF_READ(btf, next, pid);
    u32 next_cgroup_id = MAX_CGROUPS;

    if (runqueue) {
        next_cgroup_id = runqueue_switch(prev, next, next_pid, processor_id, btf);
    }

    if (migrations) {
        migrations_switch(next, next_pid, processor_id, next_cgroup_id, runqueue, btf);
    }

    return 0;
}

SEC("tp_btf/sched_wakeup")
int handle__sched_wakeup_btf(u64* ctx) {
    return account__sched_wakeup(ctx, true);
}

SEC("raw_tp/sched_wakeup")
int handle__sched_wakeup_raw(u64* ctx) {
    return account__sched_wakeup(ctx, false);
}

SEC("tp_btf/sched_wakeup_new")
int handle__sched_wakeup_new_btf(u64* ctx) {
    return account__sched_wakeup_new(ctx, true);
}

SEC("raw_tp/sched_wakeup_new")
int handle__sched_wakeup_new_raw(u64* ctx) {
    return account__sched_wakeup_new(ctx, false);
}

SEC("tp_btf/sched_switch")
int handle__sched_switch_btf(u64* ctx) {
    return account__sched_switch(ctx, true);
}

SEC("raw_tp/sched_switch")
int handle__sched_switch_raw(u64* ctx) {
    return account__sched_switch(ctx, false);
}

char LICENSE[] SEC("license") = "GPL";
