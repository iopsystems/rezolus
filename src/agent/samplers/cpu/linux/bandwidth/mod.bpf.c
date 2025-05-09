// SPDX-License-Identifier: GPL-2.0
// Copyright (c) 2025 The Rezolus Authors

// This BPF program probes CFS throttling events and changes to CFS bandwidth
// settings to capture metrics about throttling and cpu quota

#include <vmlinux.h>
#include "../../../agent/bpf/cgroup_info.h"
#include "../../../agent/bpf/helpers.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_tracing.h>

#define MAX_CGROUPS 4096
#define RINGBUF_CAPACITY 262144

// struct to pass bandwidth info to userspace
struct bandwidth_info {
    u32 id;             // cgroup id
    u64 quota;          // quota in nanoseconds
    u64 period;         // period in nanoseconds
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

// track throttle start times
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CGROUPS);
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

SEC("kprobe/tg_set_cfs_bandwidth")
int tg_set_cfs_bandwidth(struct pt_regs *ctx)
{
    struct task_group *tg = (struct task_group *)PT_REGS_PARM1(ctx);
    struct cfs_bandwidth *cfs_b = (struct cfs_bandwidth *)PT_REGS_PARM2(ctx);

    if (!tg || !cfs_b)
        return 0;

    // get the cgroup id and serial number

    struct cgroup_subsys_state *css = &tg->css;
    if (!css)
        return 0;

    u32 cgroup_id = BPF_CORE_READ(css, id);
    if (!cgroup_id || cgroup_id >= MAX_CGROUPS)
        return 0;

    u64 serial_nr = BPF_CORE_READ(css, serial_nr);

    // check if this is a new cgroup by checking the serial number and id

    u64 *elem = bpf_map_lookup_elem(&cgroup_serial_numbers, &cgroup_id);

    if (elem && *elem != serial_nr) {
        // zero the counters, they will not be exported until they are non-zero
        u64 zero = 0;
        bpf_map_update_elem(&throttled_time, &cgroup_id, &zero, BPF_ANY);
        bpf_map_update_elem(&throttled_count, &cgroup_id, &zero, BPF_ANY);

        // initialize the cgroup info
        struct cgroup_info cginfo = {
            .id = cgroup_id,
            .level = BPF_CORE_READ(css, cgroup, level),
        };

        // assemble cgroup name
        bpf_probe_read_kernel_str(&cginfo.name, CGROUP_NAME_LEN, BPF_CORE_READ(css, cgroup, kn, name));
        bpf_probe_read_kernel_str(&cginfo.pname, CGROUP_NAME_LEN, BPF_CORE_READ(css, cgroup, kn, parent, name));
        bpf_probe_read_kernel_str(&cginfo.gpname, CGROUP_NAME_LEN, BPF_CORE_READ(css, cgroup, kn, parent, parent, name));

        // push the cgroup info into the ringbuf
        bpf_ringbuf_output(&cgroup_info, &cginfo, sizeof(cginfo), 0);

        // update the serial number in the local map
        bpf_map_update_elem(&cgroup_serial_numbers, &cgroup_id, &serial_nr, BPF_ANY);
    }

    // get the bandwidth info and send to userspace
    u64 quota = BPF_CORE_READ(cfs_b, quota);
    u64 period = BPF_CORE_READ(cfs_b, period);
    struct bandwidth_info bw_info = {
        .id = cgroup_id,
        .quota = quota,
        .period = period
    };
    bpf_ringbuf_output(&bandwidth_info, &bw_info, sizeof(bw_info), 0);

    return 0;
}

SEC("kprobe/throttle_cfs_rq")
int throttle_cfs_rq(struct pt_regs *ctx)
{
    struct cfs_rq *cfs_rq = (struct cfs_rq *)PT_REGS_PARM1(ctx);

    // get the cgroup id and serial number

    struct task_group *tg = BPF_CORE_READ(cfs_rq, tg);
    if (!tg)
        return 0;

    struct cgroup_subsys_state *css = &tg->css;
    if (!css)
        return 0;

    u64 cgroup_id = BPF_CORE_READ(css, id);
    if (!cgroup_id || cgroup_id >= MAX_CGROUPS)
        return 0;

    u64 serial_nr = BPF_CORE_READ(css, serial_nr);

    // check if this is a new cgroup by checking the serial number and id

    u64 *elem = bpf_map_lookup_elem(&cgroup_serial_numbers, &cgroup_id);

    if (elem && *elem != serial_nr) {
        // zero the counters, they will not be exported until they are non-zero
        u64 zero = 0;
        bpf_map_update_elem(&throttled_time, &cgroup_id, &zero, BPF_ANY);
        bpf_map_update_elem(&throttled_count, &cgroup_id, &zero, BPF_ANY);

        // initialize the cgroup info
        struct cgroup_info cginfo = {
            .id = cgroup_id,
            .level = BPF_CORE_READ(css, cgroup, level),
        };

        // assemble cgroup name
        bpf_probe_read_kernel_str(&cginfo.name, CGROUP_NAME_LEN, BPF_CORE_READ(css, cgroup, kn, name));
        bpf_probe_read_kernel_str(&cginfo.pname, CGROUP_NAME_LEN, BPF_CORE_READ(css, cgroup, kn, parent, name));
        bpf_probe_read_kernel_str(&cginfo.gpname, CGROUP_NAME_LEN, BPF_CORE_READ(css, cgroup, kn, parent, parent, name));

        // push the cgroup info into the ringbuf
        bpf_ringbuf_output(&cgroup_info, &cginfo, sizeof(cginfo), 0);

        // get the bandwidth info and send to userspace
        u64 quota = BPF_CORE_READ(tg, cfs_bandwidth.quota);
        u64 period = BPF_CORE_READ(tg, cfs_bandwidth.period);
        struct bandwidth_info bw_info = {
            .id = cgroup_id,
            .quota = quota,
            .period = period
        };
        bpf_ringbuf_output(&bandwidth_info, &bw_info, sizeof(bw_info), 0);

        // update the serial number in the local map
        bpf_map_update_elem(&cgroup_serial_numbers, &cgroup_id, &serial_nr, BPF_ANY);
    }

    // increment the throttle count
    array_incr(&throttled_count, cgroup_id);

    // lookup start time, don't update it if currently non-zero
    u32 cgroup_idx = (u32)cgroup_id;
    u64 *start_ts = bpf_map_lookup_elem(&throttle_start, &cgroup_idx);
    if (!start_ts || *start_ts != 0)
        return 0;

    // record throttle start time
    u64 now = bpf_ktime_get_ns();
    bpf_map_update_elem(&throttle_start, &cgroup_idx, &now, BPF_ANY);

    return 0;
}

SEC("kprobe/unthrottle_cfs_rq")
int unthrottle_cfs_rq(struct pt_regs *ctx)
{
    struct cfs_rq *cfs_rq = (struct cfs_rq *)PT_REGS_PARM1(ctx);

    // get the cgroup id

    struct task_group *tg = BPF_CORE_READ(cfs_rq, tg);
    if (!tg)
        return 0;

    struct cgroup_subsys_state *css = &tg->css;
    if (!css)
        return 0;

    u64 cgroup_id = BPF_CORE_READ(css, id);
    if (!cgroup_id || cgroup_id >= MAX_CGROUPS)
        return 0;

    // skip accounting if the serial number doesn't match
    u64 serial_nr = BPF_CORE_READ(css, serial_nr);
    u64 *elem = bpf_map_lookup_elem(&cgroup_serial_numbers, &cgroup_id);
    if (!elem || *elem != serial_nr)
        return 0;

    // lookup start time
    u32 cgroup_idx = (u32)cgroup_id;
    u64 *start_ts = bpf_map_lookup_elem(&throttle_start, &cgroup_idx);
    if (!start_ts || *start_ts == 0)
        return 0;

    // increment the throttled time counter
    u64 now = bpf_ktime_get_ns();
    u64 duration = now - *start_ts;
    array_add(&throttled_time, cgroup_id, duration);

    // clear the throttle start time
    u64 zero = 0;
    bpf_map_update_elem(&throttle_start, &cgroup_idx, &zero, BPF_ANY);

    return 0;
}

char LICENSE[] SEC("license") = "GPL";
