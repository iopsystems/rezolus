// SPDX-License-Identifier: GPL-2.0
// Copyright (c) 2026 The Rezolus Authors

// This BPF program times ext4's request-path operations -- fsync, unlink,
// write and rename -- from the calling thread's point of view: how long each
// call held the thread, per filesystem and per cgroup. Design:
// docs/journal/2026-09-28-filesystem-telemetry-gaps.md (C5, C6).
//
// Each operation is a begin/end pair. fsync and unlink have enter/exit
// tracepoints; write and rename do not, so those are fentry/fexit on the ext4
// functions themselves (module BTF on kernels where ext4 is a module, 5.11+).
// The start timestamp lives in task local storage, one slot per operation,
// because a write to an O_SYNC file runs fsync inside it on the same thread
// and a single per-thread slot would lose the outer timing. Task local
// storage reached tracing programs in 5.12 (5.11 had it for LSM programs
// only), so 5.12 is this sampler's floor: one release above the module-BTF
// fentry it also needs.

#include <vmlinux.h>
#include "../../../agent/bpf/cgroup.h"
#include "../../../agent/bpf/helpers.h"
#include "../../../agent/bpf/filesystem.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_tracing.h>

#define COUNTER_GROUP_WIDTH 16
#define HISTOGRAM_BUCKETS HISTOGRAM_BUCKETS_POW_3
// What ext4_file_write_iter returns for an asynchronous direct write
// (io_uring, libaio): the request is queued, not failed, and its bytes are
// reported at completion, out of this sampler's sight.
#define EIOCBQUEUED 529
#define HISTOGRAM_POWER 3
#define MAX_CPUS 1024

// Per-cgroup attribution is the config option `cgroup_attribution`, off by
// default: the serial check and two atomics it adds to the end hook measured
// 265 ns of the hook's 535 ns on the null_blk fsync bench, half the hook.
// Userspace writes the switch into read-only data before load, so with it
// off the verifier removes the path from the program rather than testing a
// flag on every run.
const volatile __u8 cgroup_attribution = 0;

// The operations, in the order of the `op` label's histograms and of the
// per-op counter runs below.
#define OP_FSYNC 0
#define OP_UNLINK 1
#define OP_WRITE 2
#define OP_RENAME 3
#define OP_COUNT 4

// per-filesystem counters: one bank of COUNTER_GROUP_WIDTH per (CPU, slot).
// Three runs of OP_COUNT plus one; the order MUST match the `counters` vec in
// mod.rs.
#define C_OPS 0    // + op: calls completed
#define C_TIME 4   // + op: nanoseconds the calls held the thread, summed
#define C_ERRORS 8 // + op: calls that returned an error
#define C_WRITE_BYTES 12

// Per-thread start state: one slot per operation, so nested operations on one
// thread (fsync inside an O_SYNC write) each keep their own start.
struct op_start {
    u64 ts[OP_COUNT];
    u32 dev[OP_COUNT];
};

struct {
    __uint(type, BPF_MAP_TYPE_TASK_STORAGE);
    __uint(map_flags, BPF_F_NO_PREALLOC);
    __type(key, int);
    __type(value, struct op_start);
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
    __uint(max_entries, MAX_CPUS * MAX_FILESYSTEMS * COUNTER_GROUP_WIDTH);
} counters SEC(".maps");

// Latency histograms, host-wide, one per operation.
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS);
} fsync_latency SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS);
} unlink_latency SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS);
} write_latency SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS);
} rename_latency SEC(".maps");

/*
 * per-cgroup counters: calls and nanoseconds held, per operation
 */

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CGROUPS);
} cgroup_ops_fsync SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CGROUPS);
} cgroup_ops_unlink SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CGROUPS);
} cgroup_ops_write SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CGROUPS);
} cgroup_ops_rename SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CGROUPS);
} cgroup_time_fsync SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CGROUPS);
} cgroup_time_unlink SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CGROUPS);
} cgroup_time_write SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CGROUPS);
} cgroup_time_rename SEC(".maps");

static __always_inline void counter_add(u32 slot, u32 counter, u64 value) {
    array_add(&counters, fs_counter_idx(slot, counter, COUNTER_GROUP_WIDTH), value);
}

// The device under a kiocb's file, or 0 (slot 0) for a NULL pointer.
static __always_inline u32 kiocb_dev(struct kiocb* iocb) {
    if (!iocb) {
        return 0;
    }

    return BPF_CORE_READ(iocb, ki_filp, f_inode, i_sb, s_dev);
}

// Stamp the start of operation `op` on the current thread. `dev` is the
// filesystem the call is against, read here because the end hook of a
// tracepoint pair does not always have it (ext4_unlink_exit has the dentry
// only) and the fexit twin reads the same argument the fentry did.
static __always_inline void op_begin(u32 op, u32 dev) {
    struct task_struct* task = bpf_get_current_task_btf();
    struct op_start* s;

    if (op >= OP_COUNT) {
        return;
    }

    s = bpf_task_storage_get(&starts, task, 0, BPF_LOCAL_STORAGE_GET_F_CREATE);
    if (!s) {
        return;
    }

    s->ts[op] = bpf_ktime_get_ns();
    s->dev[op] = dev;
}

static __always_inline void cgroup_account(struct task_struct* task, u32 op, u64 lat) {
    if (!cgroup_attribution) {
        return;
    }

    // runtime NULL check (bpf_core_field_exists is a compile-time BTF check)
    void* task_group = BPF_CORE_READ(task, sched_task_group);
    if (!task_group) {
        return;
    }

    u32 cgroup_id = BPF_CORE_READ(task, sched_task_group, css.id);
    if (cgroup_id >= MAX_CGROUPS) {
        return;
    }

    if (handle_new_cgroup(task, &cgroup_serial_numbers, &cgroup_info) == 0) {
        // New cgroup detected, zero all counters
        u64 zero = 0;
        bpf_map_update_elem(&cgroup_ops_fsync, &cgroup_id, &zero, BPF_ANY);
        bpf_map_update_elem(&cgroup_ops_unlink, &cgroup_id, &zero, BPF_ANY);
        bpf_map_update_elem(&cgroup_ops_write, &cgroup_id, &zero, BPF_ANY);
        bpf_map_update_elem(&cgroup_ops_rename, &cgroup_id, &zero, BPF_ANY);
        bpf_map_update_elem(&cgroup_time_fsync, &cgroup_id, &zero, BPF_ANY);
        bpf_map_update_elem(&cgroup_time_unlink, &cgroup_id, &zero, BPF_ANY);
        bpf_map_update_elem(&cgroup_time_write, &cgroup_id, &zero, BPF_ANY);
        bpf_map_update_elem(&cgroup_time_rename, &cgroup_id, &zero, BPF_ANY);
    }

    switch (op) {
    case OP_FSYNC:
        array_incr(&cgroup_ops_fsync, cgroup_id);
        array_add(&cgroup_time_fsync, cgroup_id, lat);
        break;
    case OP_UNLINK:
        array_incr(&cgroup_ops_unlink, cgroup_id);
        array_add(&cgroup_time_unlink, cgroup_id, lat);
        break;
    case OP_WRITE:
        array_incr(&cgroup_ops_write, cgroup_id);
        array_add(&cgroup_time_write, cgroup_id, lat);
        break;
    case OP_RENAME:
        array_incr(&cgroup_ops_rename, cgroup_id);
        array_add(&cgroup_time_rename, cgroup_id, lat);
        break;
    }
}

// Close operation `op` on the current thread: latency into the op's
// histogram, count and time into the filesystem's slot and the cgroup's.
// `ret` is the call's return value (negative errno on failure; for a write,
// the bytes written). An async direct write returns -EIOCBQUEUED: not an
// error, and its bytes are not known here, so it counts as a call whose
// latency is the submission time and adds nothing to write bytes.
static __always_inline void op_end(u32 op, long ret) {
    struct task_struct* task = bpf_get_current_task_btf();
    struct op_start* s;
    u64 start, lat;
    u32 slot;

    if (op >= OP_COUNT) {
        return;
    }

    s = bpf_task_storage_get(&starts, task, 0, 0);
    if (!s) {
        return;
    }

    start = s->ts[op];
    // possible we missed the start
    if (!start) {
        return;
    }
    s->ts[op] = 0;

    lat = bpf_ktime_get_ns() - start;
    slot = fs_slot(s->dev[op]);

    switch (op) {
    case OP_FSYNC:
        histogram_incr(&fsync_latency, HISTOGRAM_POWER, lat);
        break;
    case OP_UNLINK:
        histogram_incr(&unlink_latency, HISTOGRAM_POWER, lat);
        break;
    case OP_WRITE:
        histogram_incr(&write_latency, HISTOGRAM_POWER, lat);
        break;
    case OP_RENAME:
        histogram_incr(&rename_latency, HISTOGRAM_POWER, lat);
        break;
    }

    counter_add(slot, C_OPS + op, 1);
    counter_add(slot, C_TIME + op, lat);
    if (ret < 0 && ret != -EIOCBQUEUED) {
        counter_add(slot, C_ERRORS + op, 1);
    }
    if (op == OP_WRITE && ret > 0) {
        counter_add(slot, C_WRITE_BYTES, (u64)ret);
    }

    cgroup_account(task, op, lat);
}

// fsync and unlink: enter/exit tracepoints. Both need module BTF where ext4
// is a module, which every kernel this sampler runs on has (see the header
// comment), so there are no raw_tp twins.

SEC("tp_btf/ext4_sync_file_enter")
int BPF_PROG(ext4_sync_file_enter, struct file* file, int datasync) {
    op_begin(OP_FSYNC, file_dev(file));
    return 0;
}

SEC("tp_btf/ext4_sync_file_exit")
int BPF_PROG(ext4_sync_file_exit, struct inode* inode, int ret) {
    op_end(OP_FSYNC, ret);
    return 0;
}

SEC("tp_btf/ext4_unlink_enter")
int BPF_PROG(ext4_unlink_enter, struct inode* parent, struct dentry* dentry) {
    op_begin(OP_UNLINK, inode_dev(parent));
    return 0;
}

SEC("tp_btf/ext4_unlink_exit")
int BPF_PROG(ext4_unlink_exit, struct dentry* dentry, int ret) {
    op_end(OP_UNLINK, ret);
    return 0;
}

// write: ext4_file_write_iter(struct kiocb *, struct iov_iter *) -> ssize_t,
// the same signature on every kernel since the split into buffered, direct
// and DAX variants (5.5) kept this as the dispatcher.

SEC("fentry/ext4_file_write_iter")
int BPF_PROG(ext4_file_write_iter_fentry, struct kiocb* iocb, struct iov_iter* from) {
    op_begin(OP_WRITE, kiocb_dev(iocb));
    return 0;
}

SEC("fexit/ext4_file_write_iter")
int BPF_PROG(ext4_file_write_iter_fexit, struct kiocb* iocb, struct iov_iter* from, long ret) {
    op_end(OP_WRITE, ret);
    return 0;
}

// rename: ext4_rename2(ns, old_dir, old_dentry, new_dir, new_dentry, flags)
// -> int. The first argument is struct user_namespace * from 5.12 and struct
// mnt_idmap * from 6.3; it is not read, so one program serves both. Before
// 5.12 there was no namespace argument, but no kernel that old can load this
// skeleton (task storage, above), so there is no five-argument twin. A
// trampoline program reads by position, so mod.rs still confirms the arity
// from BTF and disables the pair on any other count rather than misreading.

SEC("fentry/ext4_rename2")
int BPF_PROG(ext4_rename2_fentry, void* ns, struct inode* old_dir, struct dentry* old_dentry,
             struct inode* new_dir, struct dentry* new_dentry, unsigned int flags) {
    op_begin(OP_RENAME, inode_dev(old_dir));
    return 0;
}

SEC("fexit/ext4_rename2")
int BPF_PROG(ext4_rename2_fexit, void* ns, struct inode* old_dir, struct dentry* old_dentry,
             struct inode* new_dir, struct dentry* new_dentry, unsigned int flags, int ret) {
    op_end(OP_RENAME, ret);
    return 0;
}

char LICENSE[] SEC("license") = "GPL";
