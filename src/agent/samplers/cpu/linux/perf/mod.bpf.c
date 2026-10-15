// SPDX-License-Identifier: GPL-2.0
// Copyright (c) 2023 The Rezolus Authors

#include <vmlinux.h>
#include "../../../agent/bpf/cgroup.h"
#include "../../../agent/bpf/helpers.h"
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_tracing.h>

#define COUNTERS 2
#define COUNTER_GROUP_WIDTH 8
#define MAX_CPUS 1024

#define TASK_RUNNING 0

// counter positions
#define CYCLES 0
#define INSTRUCTIONS 1

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

// counters for various events

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CGROUPS * CGROUP_SHARDS);
} cgroup_cycles SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CGROUPS * CGROUP_SHARDS);
} cgroup_instructions SEC(".maps");

// previous readings for various events, one per CPU at processor id * 8 so
// each CPU's reading is in its own cache line
// (BPF-internal state, not read by userspace, so not mmapable)

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CPUS * 8);
} cycles_prev SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CPUS * 8);
} instructions_prev SEC(".maps");

/**
 * perf event arrays
 */

struct {
    __uint(type, BPF_MAP_TYPE_PERF_EVENT_ARRAY);
    __type(key, u32);
    __type(value, u32);
} cycles SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_PERF_EVENT_ARRAY);
    __type(key, u32);
    __type(value, u32);
} instructions SEC(".maps");

// Stored in `cycles_prev`/`instructions_prev` after a failed read: the next
// successful read on that CPU sets a baseline and credits nothing, rather than
// crediting the whole outage to whichever cgroup is switching out then.
#define NO_BASELINE (~0ULL)

// Read one counter of this CPU's perf event group. Returns false when the read
// failed: -ENOENT for a CPU with no event in the array (outside the sampler's
// PMU grant, or its group did not open) and -EBUSY for a pinned event that is
// not on the PMU now. Do not use `bpf_perf_event_read` here: it returns those
// errors in the same u64 as the count, and they would be credited as counts.
static __always_inline bool read_counter(void* map, u64* value) {
    struct bpf_perf_event_value v = {};

    if (bpf_perf_event_read_value(map, BPF_F_CURRENT_CPU, &v, sizeof(v)) != 0) {
        return false;
    }

    *value = v.counter;
    return true;
}

// Credit the counter's advance since this CPU's last switch to `cgroup_id`.
// The count is the kernel's 64-bit `perf_event` count, which already folds the
// hardware counter's narrower width, so the u64 difference is the delta.
static __always_inline void credit(void* prev_map, u32 prev_idx, u64 now, void* counters,
                                   u32 cgroup_id) {
    u64* prev = bpf_map_lookup_elem(prev_map, &prev_idx);

    if (prev && *prev != NO_BASELINE) {
        cgroup_add(counters, cgroup_id, now - *prev);
    }
}

// attach a tracepoint on sched_switch for per-cgroup accounting

// `btf` is a compile-time constant: true in the tp_btf program, whose task
// arguments are BTF pointers (see task_group_of() in cgroup.h).
static __always_inline int account__sched_switch(u64* ctx, bool btf) {
    /* TP_PROTO(bool preempt, struct task_struct *prev,
     *      struct task_struct *next)
     */
    struct task_struct* prev = (struct task_struct*)ctx[1];

    u32 prev_idx = bpf_get_smp_processor_id() * 8;

    u64 c = 0, i = 0;
    bool c_ok = read_counter(&cycles, &c);
    bool i_ok = read_counter(&instructions, &i);

    u32 cgroup_id = 0;
    u64 serial_nr = 0;
    struct task_group* tg = bpf_core_field_exists(prev->sched_task_group)
                                ? task_group_of(prev, btf, &cgroup_id, &serial_nr)
                                : NULL;

    if (tg) {
        if (cgroup_id < MAX_CGROUPS) {
            int ret = handle_new_cgroup_read(&tg->css, cgroup_id, serial_nr,
                                             &cgroup_serial_numbers, &cgroup_info);

            if (ret == 0) {
                // New cgroup detected, zero the counters
                cgroup_zero(&cgroup_cycles, cgroup_id);
                cgroup_zero(&cgroup_instructions, cgroup_id);
            }

            if (c_ok) {
                credit(&cycles_prev, prev_idx, c, &cgroup_cycles, cgroup_id);
            }

            if (i_ok) {
                credit(&instructions_prev, prev_idx, i, &cgroup_instructions, cgroup_id);
            }
        }
    }

    if (!c_ok) {
        c = NO_BASELINE;
    }
    if (!i_ok) {
        i = NO_BASELINE;
    }

    bpf_map_update_elem(&cycles_prev, &prev_idx, &c, BPF_ANY);
    bpf_map_update_elem(&instructions_prev, &prev_idx, &i, BPF_ANY);

    return 0;
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
