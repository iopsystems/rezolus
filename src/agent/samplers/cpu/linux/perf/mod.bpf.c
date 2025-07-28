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
    __uint(max_entries, MAX_CGROUPS);
} cgroup_cycles SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CGROUPS);
} cgroup_instructions SEC(".maps");

// previous readings for various events

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CGROUPS);
} cycles_prev SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CGROUPS);
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

/**
 * commit 2f064a59a1 ("sched: Change task_struct::state") changes
 * the name of task_struct::state to task_struct::__state
 * see:
 *     https://github.com/torvalds/linux/commit/2f064a59a1
 */
struct task_struct___o {
    volatile long int state;
} __attribute__((preserve_access_index));

struct task_struct___x {
    unsigned int __state;
} __attribute__((preserve_access_index));

// attach a tracepoint on sched_switch for per-cgroup accounting

SEC("tp_btf/sched_switch")
int handle__sched_switch(u64* ctx) {
    /* TP_PROTO(bool preempt, struct task_struct *prev,
     *      struct task_struct *next)
     */
    struct task_struct* prev = (struct task_struct*)ctx[1];

    u64 *elem, delta_c, delta_i;

    u32 processor_id = bpf_get_smp_processor_id();

    u64 c = bpf_perf_event_read(&cycles, BPF_F_CURRENT_CPU);
    u64 i = bpf_perf_event_read(&instructions, BPF_F_CURRENT_CPU);

    if (bpf_core_field_exists(prev->sched_task_group)) {
        int cgroup_id = prev->sched_task_group->css.id;

        if (cgroup_id < MAX_CGROUPS) {

            // we check to see if this is a new cgroup by checking the serial number

            int ret = handle_new_cgroup(prev, &cgroup_serial_numbers, &cgroup_info);

            if (ret == 0) {
                // New cgroup detected, zero the counters
                u64 zero = 0;
                bpf_map_update_elem(&cgroup_cycles, &cgroup_id, &zero, BPF_ANY);
                bpf_map_update_elem(&cgroup_instructions, &cgroup_id, &zero, BPF_ANY);
            }

            // update cgroup cycles

            elem = bpf_map_lookup_elem(&cycles_prev, &processor_id);

            if (elem) {
                delta_c = c - *elem;

                array_add(&cgroup_cycles, cgroup_id, delta_c);
            }

            // update cgroup instructions

            elem = bpf_map_lookup_elem(&instructions_prev, &processor_id);

            if (elem) {
                delta_i = i - *elem;

                array_add(&cgroup_instructions, cgroup_id, delta_i);
            }
        }
    }

    // update the per-core counters

    bpf_map_update_elem(&cycles_prev, &processor_id, &c, BPF_ANY);
    bpf_map_update_elem(&instructions_prev, &processor_id, &i, BPF_ANY);

    return 0;
}

char LICENSE[] SEC("license") = "GPL";
