// SPDX-License-Identifier: GPL-2.0
// Copyright (c) 2025 The Rezolus Authors

// This BPF program probes CFS throttling events and changes to CFS bandwidth
// settings to capture metrics about throttling and cpu quota

#include <vmlinux.h>
#include "../../../agent/bpf/cgroup.h"
#include "../../../agent/bpf/helpers.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_tracing.h>

#define MAX_CPUS 1024

// struct to pass bandwidth info to userspace
struct bandwidth_info {
    u32 id;     // cgroup id
    u64 quota;  // quota in nanoseconds
    u64 period; // period in nanoseconds
};

// dummy instance for skeleton to generate definition
struct cgroup_info _cgroup_info = {};
struct bandwidth_info _bandwidth_info = {};

// ringbuf to pass cgroup info
struct {
    __uint(type, BPF_MAP_TYPE_RINGBUF);
    __uint(key_size, 0);
    __uint(value_size, 0);
    __uint(max_entries, RINGBUF_CAPACITY);
} cgroup_info SEC(".maps");

// ringbuf to pass bandwidth info
struct {
    __uint(type, BPF_MAP_TYPE_RINGBUF);
    __uint(key_size, 0);
    __uint(value_size, 0);
    __uint(max_entries, RINGBUF_CAPACITY);
} bandwidth_info SEC(".maps");

// holds known cgroup serial numbers to help determine new or changed groups
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CGROUPS);
} cgroup_serial_numbers SEC(".maps");

// track throttle start time of per-cpu cgroup runqueues
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CGROUPS* MAX_CPUS);
} throttle_start SEC(".maps");

// counters

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CGROUPS);
} throttled_time SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CGROUPS);
} throttled_count SEC(".maps");

// per-cgroup periods
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CGROUPS);
} bandwidth_periods SEC(".maps");

// per-cgroup throttled periods
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CGROUPS);
} bandwidth_throttled_periods SEC(".maps");

// per-cgroup throttled time
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CGROUPS);
} bandwidth_throttled_time SEC(".maps");

// fentry/kprobe twins share these handlers; only the attach mechanism differs.
// fentry is the cheaper dispatch (see
// docs/journal/2026-09-04-fentry-vs-kprobe-dispatch.md) but needs BTF, so the
// kprobe twins are the CO-RE-only fallback and one set is disabled at load time
// on kernel_has_btf() (see disabled_programs in mod.rs).

static __always_inline int handle_tg_set_cfs_bandwidth(struct task_group* tg, u64 period,
                                                       u64 quota) {
    if (!tg)
        return 0;

    // get the cgroup id and serial number

    struct cgroup_subsys_state* css = &tg->css;
    if (!css)
        return 0;

    u32 cgroup_id = BPF_CORE_READ(css, id);
    if (cgroup_id >= MAX_CGROUPS)
        return 0;

    int ret = handle_new_cgroup_from_css(css, &cgroup_serial_numbers, &cgroup_info);

    if (ret == 0) {
        // New cgroup detected, zero the counters
        u64 zero = 0;
        bpf_map_update_elem(&throttled_time, &cgroup_id, &zero, BPF_ANY);
        bpf_map_update_elem(&throttled_count, &cgroup_id, &zero, BPF_ANY);
        bpf_map_update_elem(&bandwidth_periods, &cgroup_id, &zero, BPF_ANY);
        bpf_map_update_elem(&bandwidth_throttled_periods, &cgroup_id, &zero, BPF_ANY);
        bpf_map_update_elem(&bandwidth_throttled_time, &cgroup_id, &zero, BPF_ANY);
    }

    struct bandwidth_info* bw_info =
        bpf_ringbuf_reserve(&bandwidth_info, sizeof(struct bandwidth_info), 0);
    if (bw_info) {
        bw_info->id = cgroup_id;
        // period/quota come straight from the args. The kernel signature is
        // tg_set_cfs_bandwidth(tg, period, quota, burst); the old code cast
        // arg1 (the period) to a pointer and dereferenced it, reading 0
        // (issue #1166). fentry receives these typed; the kprobe reads
        // PARM2/PARM3.
        bw_info->quota = quota;
        bw_info->period = period;
        bpf_ringbuf_submit(bw_info, 0);
    }

    return 0;
}

static __always_inline int handle_throttle_cfs_rq(struct cfs_rq* cfs_rq) {
    int cpu = BPF_CORE_READ(cfs_rq, rq, cpu);

    // get the cgroup id and serial number

    struct task_group* tg = BPF_CORE_READ(cfs_rq, tg);
    if (!tg)
        return 0;

    struct cgroup_subsys_state* css = &tg->css;
    if (!css)
        return 0;

    u64 cgroup_id = BPF_CORE_READ(css, id);
    if (cgroup_id >= MAX_CGROUPS)
        return 0;

    int ret = handle_new_cgroup_from_css(css, &cgroup_serial_numbers, &cgroup_info);

    if (ret == 0) {
        // New cgroup detected, zero the counters
        u64 zero = 0;
        bpf_map_update_elem(&throttled_time, &cgroup_id, &zero, BPF_ANY);
        bpf_map_update_elem(&throttled_count, &cgroup_id, &zero, BPF_ANY);

        struct bandwidth_info* bw_info =
            bpf_ringbuf_reserve(&bandwidth_info, sizeof(struct bandwidth_info), 0);
        if (bw_info) {
            bw_info->id = cgroup_id;
            bw_info->quota = BPF_CORE_READ(tg, cfs_bandwidth.quota);
            bw_info->period = BPF_CORE_READ(tg, cfs_bandwidth.period);
            bpf_ringbuf_submit(bw_info, 0);
        }
    }

    u64 now = bpf_ktime_get_ns();
    u32 cgroup_runqueue_idx = cpu * MAX_CGROUPS + (u32)cgroup_id;
    bpf_map_update_elem(&throttle_start, &cgroup_runqueue_idx, &now, BPF_ANY);

    array_incr(&throttled_count, cgroup_id);

    return 0;
}

static __always_inline int handle_unthrottle_cfs_rq(struct cfs_rq* cfs_rq) {
    int cpu = BPF_CORE_READ(cfs_rq, rq, cpu);

    // get the cgroup id

    struct task_group* tg = BPF_CORE_READ(cfs_rq, tg);
    if (!tg)
        return 0;

    struct cgroup_subsys_state* css = &tg->css;
    if (!css)
        return 0;

    u64 cgroup_id = BPF_CORE_READ(css, id);
    if (cgroup_id >= MAX_CGROUPS)
        return 0;

    // skip accounting if the serial number doesn't match
    u64 serial_nr = BPF_CORE_READ(css, serial_nr);
    u64* elem = bpf_map_lookup_elem(&cgroup_serial_numbers, &cgroup_id);
    if (!elem || *elem != serial_nr)
        return 0;

    int nr_periods = BPF_CORE_READ(cfs_rq, tg, cfs_bandwidth.nr_periods);
    int nr_throttled = BPF_CORE_READ(cfs_rq, tg, cfs_bandwidth.nr_throttled);
    u64 cgroup_throttled_time = BPF_CORE_READ(cfs_rq, tg, cfs_bandwidth.throttled_time);

    // benign race: these kernel counters are monotone, so the non-atomic
    // load+compare+store in array_set_if_larger can lose an occasional
    // concurrent update and the next observation self-heals
    array_set_if_larger(&bandwidth_periods, (u32)cgroup_id, (u64)nr_periods);
    array_set_if_larger(&bandwidth_throttled_periods, (u32)cgroup_id, (u64)nr_throttled);
    array_set_if_larger(&bandwidth_throttled_time, (u32)cgroup_id, cgroup_throttled_time);

    u32 cgroup_runqueue_idx = cpu * MAX_CGROUPS + (u32)cgroup_id;
    u64* start_ts = bpf_map_lookup_elem(&throttle_start, &cgroup_runqueue_idx);
    if (!start_ts || *start_ts == 0)
        return 0;

    u64 now = bpf_ktime_get_ns();
    u64 duration = now - *start_ts;
    array_add(&throttled_time, cgroup_id, duration);

    u64 zero = 0;
    bpf_map_update_elem(&throttle_start, &cgroup_runqueue_idx, &zero, BPF_ANY);

    return 0;
}

SEC("fentry/tg_set_cfs_bandwidth")
int BPF_PROG(tg_set_cfs_bandwidth_fentry, struct task_group* tg, u64 period, u64 quota, u64 burst) {
    return handle_tg_set_cfs_bandwidth(tg, period, quota);
}

SEC("kprobe/tg_set_cfs_bandwidth")
int BPF_KPROBE(tg_set_cfs_bandwidth_kprobe, struct task_group* tg, u64 period, u64 quota) {
    return handle_tg_set_cfs_bandwidth(tg, period, quota);
}

SEC("fentry/throttle_cfs_rq")
int BPF_PROG(throttle_cfs_rq_fentry, struct cfs_rq* cfs_rq) {
    return handle_throttle_cfs_rq(cfs_rq);
}

SEC("kprobe/throttle_cfs_rq")
int BPF_KPROBE(throttle_cfs_rq_kprobe, struct cfs_rq* cfs_rq) {
    return handle_throttle_cfs_rq(cfs_rq);
}

SEC("fentry/unthrottle_cfs_rq")
int BPF_PROG(unthrottle_cfs_rq_fentry, struct cfs_rq* cfs_rq) {
    return handle_unthrottle_cfs_rq(cfs_rq);
}

SEC("kprobe/unthrottle_cfs_rq")
int BPF_KPROBE(unthrottle_cfs_rq_kprobe, struct cfs_rq* cfs_rq) {
    return handle_unthrottle_cfs_rq(cfs_rq);
}

char LICENSE[] SEC("license") = "GPL";
