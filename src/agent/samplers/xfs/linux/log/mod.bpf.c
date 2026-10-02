// SPDX-License-Identifier: GPL-2.0
// Copyright (c) 2026 The Rezolus Authors

// This BPF program times the two places a thread blocks on the XFS log --
// waiting for log space (a transaction reservation that found the log full)
// and forcing the log (the synchronous flush an fsync demands) -- per
// filesystem and per cgroup, and counts CIL-full waits. Design:
// docs/journal/2026-09-29-xfs-samplers.md (step 2).
//
// XFS keeps the COUNTS of these events itself (/sys/fs/xfs/<dev>/stats/stats,
// read by the xfs_stats sampler); what the stats file cannot say is how long
// anyone waited or which cgroup did, and that is all this program adds. Each
// wait is a begin/end pair on one thread: the grant sleep/wake tracepoints
// bracket one schedule() in xlog_grant_head_wait, and a log force is
// fentry/fexit on the XFS functions that perform it (module BTF, 5.11+). The
// start timestamp lives in task local storage, one slot per pair, so a force
// that sleeps for log space inside it keeps both timings. Task local storage
// reached tracing programs in 5.12, so that is this sampler's floor.

#include <vmlinux.h>
#include "../../../agent/bpf/cgroup.h"
#include "../../../agent/bpf/helpers.h"
#include "../../../agent/bpf/filesystem.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_tracing.h>

#define COUNTER_GROUP_WIDTH 8
#define HISTOGRAM_BUCKETS HISTOGRAM_BUCKETS_POW_3
#define HISTOGRAM_POWER 3
#define MAX_CPUS 1024

// Per-cgroup attribution is the config option `cgroup_attribution`, off by
// default: the serial check and two atomics it adds to the end hook measured
// 265 ns of the hook's 535 ns on the null_blk fsync bench, half the hook.
// Userspace writes the switch into read-only data before load, so with it
// off the verifier removes the path from the program rather than testing a
// flag on every run.
const volatile __u8 cgroup_attribution = 0;

// XFS's mount and log structs, as CO-RE flavors: the vendored vmlinux.h
// headers were generated from kernels with XFS as a module and carry neither,
// and libbpf strips the ___rz suffix when relocating against the running
// kernel's BTF (vmlinux or xfs.ko's). Only the fields read here are declared;
// the offsets come from the running kernel.
struct xfs_mount___rz {
    struct super_block* m_super;
} __attribute__((preserve_access_index));

struct xlog___rz {
    struct xfs_mount___rz* l_mp;
} __attribute__((preserve_access_index));

// The begin/end pairs, each with its own start slot on the thread.
#define PAIR_SPACE 0      // xfs_log_grant_sleep -> xfs_log_grant_wake
#define PAIR_FORCE 1      // xfs_log_force fentry -> fexit
#define PAIR_FORCE_SEQ 2  // xfs_log_force_seq (or _lsn) fentry -> fexit
#define PAIR_COUNT 3

// What a pair publishes as: the `wait` label's index. Both force pairs are
// one wait, "force".
#define WAIT_SPACE 0
#define WAIT_FORCE 1
#define WAIT_COUNT 2

// per-filesystem counters: one bank of COUNTER_GROUP_WIDTH per (CPU, slot).
// The order MUST match the `counters` vec in mod.rs.
#define C_WAITS_SPACE 0
#define C_WAITS_FORCE 1
#define C_WAITS_CIL 2
#define C_TIME_SPACE 3
#define C_TIME_FORCE 4

struct wait_start {
    u64 ts[PAIR_COUNT];
    u32 dev[PAIR_COUNT];
};

struct {
    __uint(type, BPF_MAP_TYPE_TASK_STORAGE);
    __uint(map_flags, BPF_F_NO_PREALLOC);
    __type(key, int);
    __type(value, struct wait_start);
} starts SEC(".maps");

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

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CPUS* MAX_FILESYSTEMS* COUNTER_GROUP_WIDTH);
} counters SEC(".maps");

// Latency histograms, host-wide, one per wait.
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS);
} space_latency SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS);
} force_latency SEC(".maps");

/*
 * per-cgroup counters: waits and nanoseconds blocked, per wait
 */

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CGROUPS);
} cgroup_waits_space SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CGROUPS);
} cgroup_waits_force SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CGROUPS);
} cgroup_time_space SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CGROUPS);
} cgroup_time_force SEC(".maps");

static __always_inline void counter_add(u32 slot, u32 counter, u64 value) {
    array_add(&counters, fs_counter_idx(slot, counter, COUNTER_GROUP_WIDTH), value);
}

// The device of a mount, or 0 (slot 0) for a NULL pointer. tp_btf and fentry
// pointer arguments are trusted_ptr_or_null and need the check before
// BPF_CORE_READ's offset arithmetic.
static __always_inline u32 mount_dev(struct xfs_mount___rz* mp) {
    if (!mp) {
        return 0;
    }

    return BPF_CORE_READ(mp, m_super, s_dev);
}

static __always_inline u32 xlog_dev(struct xlog___rz* log) {
    if (!log) {
        return 0;
    }

    return BPF_CORE_READ(log, l_mp, m_super, s_dev);
}

// Stamp the start of `pair` on the current thread, against filesystem `dev`.
static __always_inline void wait_begin(u32 pair, u32 dev) {
    struct task_struct* task = bpf_get_current_task_btf();
    struct wait_start* s;

    if (pair >= PAIR_COUNT) {
        return;
    }

    s = bpf_task_storage_get(&starts, task, 0, BPF_LOCAL_STORAGE_GET_F_CREATE);
    if (!s) {
        return;
    }

    s->ts[pair] = bpf_ktime_get_ns();
    s->dev[pair] = dev;
}

static __always_inline void cgroup_account(struct task_struct* task, u32 wait, u64 lat) {
    if (!cgroup_attribution) {
        return;
    }

    // `task` is bpf_get_current_task_btf(), a BTF pointer
    u32 cgroup_id = 0;
    u64 serial_nr = 0;
    struct task_group* tg = task_group_of(task, true, &cgroup_id, &serial_nr);
    if (!tg || cgroup_id >= MAX_CGROUPS) {
        return;
    }

    if (handle_new_cgroup_read(&tg->css, cgroup_id, serial_nr, &cgroup_serial_numbers,
                               &cgroup_info) == 0) {
        // New cgroup detected, zero all counters
        u64 zero = 0;
        bpf_map_update_elem(&cgroup_waits_space, &cgroup_id, &zero, BPF_ANY);
        bpf_map_update_elem(&cgroup_waits_force, &cgroup_id, &zero, BPF_ANY);
        bpf_map_update_elem(&cgroup_time_space, &cgroup_id, &zero, BPF_ANY);
        bpf_map_update_elem(&cgroup_time_force, &cgroup_id, &zero, BPF_ANY);
    }

    switch (wait) {
    case WAIT_SPACE:
        array_incr(&cgroup_waits_space, cgroup_id);
        array_add(&cgroup_time_space, cgroup_id, lat);
        break;
    case WAIT_FORCE:
        array_incr(&cgroup_waits_force, cgroup_id);
        array_add(&cgroup_time_force, cgroup_id, lat);
        break;
    }
}

// Close `pair` on the current thread and publish it as `wait`: latency into
// the wait's histogram, count and time into the filesystem's slot and the
// cgroup's.
static __always_inline void wait_end(u32 pair, u32 wait) {
    struct task_struct* task = bpf_get_current_task_btf();
    struct wait_start* s;
    u64 start, lat;
    u32 slot;

    if (pair >= PAIR_COUNT || wait >= WAIT_COUNT) {
        return;
    }

    s = bpf_task_storage_get(&starts, task, 0, 0);
    if (!s) {
        return;
    }

    start = s->ts[pair];
    // possible we missed the start
    if (!start) {
        return;
    }
    s->ts[pair] = 0;

    lat = bpf_ktime_get_ns() - start;
    slot = fs_slot(s->dev[pair]);

    switch (wait) {
    case WAIT_SPACE:
        histogram_incr(&space_latency, HISTOGRAM_POWER, lat);
        counter_add(slot, C_WAITS_SPACE, 1);
        counter_add(slot, C_TIME_SPACE, lat);
        break;
    case WAIT_FORCE:
        histogram_incr(&force_latency, HISTOGRAM_POWER, lat);
        counter_add(slot, C_WAITS_FORCE, 1);
        counter_add(slot, C_TIME_FORCE, lat);
        break;
    }

    cgroup_account(task, wait, lat);
}

// Log space: xlog_grant_head_wait() fires xfs_log_grant_sleep, calls
// schedule(), and fires xfs_log_grant_wake on the same thread, once per trip
// through its loop, beside the xs_sleep_logspace count the stats file carries.
// So xfs_log_waits{wait="space"} equals xfs_log_space_sleeps per mount, and
// the time here is what that count could not say.

SEC("tp_btf/xfs_log_grant_sleep")
int BPF_PROG(xfs_log_grant_sleep, struct xlog___rz* log, void* tic) {
    wait_begin(PAIR_SPACE, xlog_dev(log));
    return 0;
}

SEC("tp_btf/xfs_log_grant_wake")
int BPF_PROG(xfs_log_grant_wake, struct xlog___rz* log, void* tic) {
    wait_end(PAIR_SPACE, WAIT_SPACE);
    return 0;
}

// Log force: xfs_log_force(mp, flags) is the whole-log force (sync, syncfs,
// the AIL pusher's flush); xfs_log_force_seq(mp, seq, flags, log_flushed) is
// the fsync path's force up to the inode's commit sequence (5.13+; before it,
// xfs_log_force_lsn(mp, lsn, flags, log_flushed) with the same first
// argument). Each counts xs_log_force once at entry, so xfs_log_waits{
// wait="force"} equals xfs_log_forces per mount when both pairs are loaded.
// Only the first argument is read, so the programs are safe by position on
// every arity these functions have had; mod.rs still confirms the names from
// BTF and disables a pair whose function the kernel lacks.

SEC("fentry/xfs_log_force")
int BPF_PROG(xfs_log_force_fentry, struct xfs_mount___rz* mp, unsigned int flags) {
    wait_begin(PAIR_FORCE, mount_dev(mp));
    return 0;
}

SEC("fexit/xfs_log_force")
int BPF_PROG(xfs_log_force_fexit, struct xfs_mount___rz* mp, unsigned int flags, int ret) {
    wait_end(PAIR_FORCE, WAIT_FORCE);
    return 0;
}

SEC("fentry/xfs_log_force_seq")
int BPF_PROG(xfs_log_force_seq_fentry, struct xfs_mount___rz* mp) {
    wait_begin(PAIR_FORCE_SEQ, mount_dev(mp));
    return 0;
}

SEC("fexit/xfs_log_force_seq")
int BPF_PROG(xfs_log_force_seq_fexit, struct xfs_mount___rz* mp) {
    wait_end(PAIR_FORCE_SEQ, WAIT_FORCE);
    return 0;
}

SEC("fentry/xfs_log_force_lsn")
int BPF_PROG(xfs_log_force_lsn_fentry, struct xfs_mount___rz* mp) {
    wait_begin(PAIR_FORCE_SEQ, mount_dev(mp));
    return 0;
}

SEC("fexit/xfs_log_force_lsn")
int BPF_PROG(xfs_log_force_lsn_fexit, struct xfs_mount___rz* mp) {
    wait_end(PAIR_FORCE_SEQ, WAIT_FORCE);
    return 0;
}

// CIL full: xlog_cil_push_background() fires xfs_log_cil_wait before it
// sleeps on the CIL's push wait queue, and nothing marks the wake, so this is
// a count only: how often committing transactions found the CIL over its
// hard limit and had to wait for a push.

SEC("tp_btf/xfs_log_cil_wait")
int BPF_PROG(xfs_log_cil_wait, struct xlog___rz* log, void* tic) {
    counter_add(fs_slot(xlog_dev(log)), C_WAITS_CIL, 1);
    return 0;
}

char LICENSE[] SEC("license") = "GPL";
