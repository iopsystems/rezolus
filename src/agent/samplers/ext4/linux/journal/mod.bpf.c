// SPDX-License-Identifier: GPL-2.0
// Copyright (c) 2026 The Rezolus Authors

// This BPF program instruments the ext4 journal (jbd2) and ext4's own sync
// and error tracepoints. Design and decisions:
// docs/journal/2026-09-28-ext4-sampler.md.
//
// Every hook is a tracepoint with a tp_btf twin and a raw_tp twin sharing one
// handler. mod.rs disables the unused twin PER HOOK on whether the kernel's
// BTF -- vmlinux or a module's -- carries the tracepoint's btf_trace_* type
// (kernel_btf_has_tracepoints), because ext4 and jbd2 are modules on some
// kernels and a tp_btf target that is not in BTF fails at load, for the whole
// skeleton.

#include <vmlinux.h>
#include "../../../agent/bpf/helpers.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_tracing.h>

#define COUNTER_GROUP_WIDTH 16
#define HISTOGRAM_BUCKETS HISTOGRAM_BUCKETS_POW_3
#define HISTOGRAM_POWER 3
#define MAX_CPUS 1024

#define NS_PER_MS 1000000ULL

// jbd2's per-commit and per-checkpoint statistics, as passed to
// jbd2_run_stats and jbd2_checkpoint_stats. Declared here as CO-RE "flavors"
// -- libbpf strips the ___rz suffix when matching against the running kernel's
// BTF -- because the vendored x86_64 vmlinux.h was generated from a kernel
// with ext4 built as a module and carries neither struct, while the aarch64
// header carries both and a plain redeclaration would collide there. Fields
// are read with BPF_CORE_READ, so the offsets come from the running kernel,
// never from this declaration.
struct transaction_run_stats_s___rz {
    unsigned long rs_wait;
    unsigned long rs_request_delay;
    unsigned long rs_running;
    unsigned long rs_locked;
    unsigned long rs_flushing;
    unsigned long rs_logging;
    __u32 rs_handle_count;
    __u32 rs_blocks;
    __u32 rs_blocks_logged;
} __attribute__((preserve_access_index));

struct transaction_chp_stats_s___rz {
    unsigned long cs_chp_time;
    __u32 cs_forced_to_close;
    __u32 cs_written;
    __u32 cs_dropped;
} __attribute__((preserve_access_index));

// counters: one bank of COUNTER_GROUP_WIDTH per CPU. The order MUST match the
// `counters` vec in mod.rs.
#define C_COMMITS 0
#define C_COMMIT_HANDLES 1
#define C_COMMIT_BLOCKS_DIRTIED 2
#define C_COMMIT_BLOCKS_LOGGED 3
#define C_CHECKPOINTS 4
#define C_CHECKPOINT_WRITTEN 5
#define C_CHECKPOINT_DROPPED 6
#define C_CHECKPOINT_FORCED_TO_CLOSE 7
#define C_SYNC_FSYNC 8
#define C_SYNC_FDATASYNC 9
#define C_SYNC_ERRORS 10
#define C_ERRORS 11
#define C_SHUTDOWNS 12
#define C_LOCK_BUFFER_STALLS 13

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CPUS* COUNTER_GROUP_WIDTH);
} counters SEC(".maps");

// [0] = nanoseconds per jiffy, written by userspace before attach from
// clock_getres(CLOCK_MONOTONIC_COARSE). jbd2 reports its phases in jiffies and
// there is no in-BPF way to learn HZ. A zero here (userspace could not
// measure the tick) disables the jiffy-based histograms; the counters still
// land.
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, 1);
} config SEC(".maps");

// Commit phases: six LIKE ENTITIES of one family (the `phase` label), read as
// one acquisition group on the userspace side (see stats.rs).
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS);
} commit_wait SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS);
} commit_request_delay SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS);
} commit_running SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS);
} commit_locked SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS);
} commit_flushing SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS);
} commit_logging SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS);
} checkpoint_latency SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS);
} lock_buffer_stall_latency SEC(".maps");

static __always_inline u64 tick_ns(void) {
    u32 idx = 0;
    u64* v = bpf_map_lookup_elem(&config, &idx);

    return v ? *v : 0;
}

static __always_inline void counter_add(u32 counter, u64 value) {
    u32 idx = COUNTER_GROUP_WIDTH * bpf_get_smp_processor_id() + counter;

    array_add(&counters, idx, value);
}

static __always_inline void counter_incr(u32 counter) {
    counter_add(counter, 1);
}

// jbd2_run_stats fires once per commit, from the journal's kjournald2 thread,
// after the commit completes. Every phase is in jiffies.
static int __always_inline handle_run_stats(void* stats) {
    struct transaction_run_stats_s___rz* s = stats;
    u64 tick = tick_ns();

    counter_incr(C_COMMITS);
    counter_add(C_COMMIT_HANDLES, BPF_CORE_READ(s, rs_handle_count));
    counter_add(C_COMMIT_BLOCKS_DIRTIED, BPF_CORE_READ(s, rs_blocks));
    counter_add(C_COMMIT_BLOCKS_LOGGED, BPF_CORE_READ(s, rs_blocks_logged));

    if (tick == 0) {
        return 0;
    }

    histogram_incr(&commit_wait, HISTOGRAM_POWER, BPF_CORE_READ(s, rs_wait) * tick);
    histogram_incr(&commit_request_delay, HISTOGRAM_POWER,
                   BPF_CORE_READ(s, rs_request_delay) * tick);
    histogram_incr(&commit_running, HISTOGRAM_POWER, BPF_CORE_READ(s, rs_running) * tick);
    histogram_incr(&commit_locked, HISTOGRAM_POWER, BPF_CORE_READ(s, rs_locked) * tick);
    histogram_incr(&commit_flushing, HISTOGRAM_POWER, BPF_CORE_READ(s, rs_flushing) * tick);
    histogram_incr(&commit_logging, HISTOGRAM_POWER, BPF_CORE_READ(s, rs_logging) * tick);

    return 0;
}

// jbd2_checkpoint_stats fires once per checkpoint. chp_time is in jiffies.
static int __always_inline handle_checkpoint_stats(void* stats) {
    struct transaction_chp_stats_s___rz* s = stats;
    u64 tick = tick_ns();

    counter_incr(C_CHECKPOINTS);
    counter_add(C_CHECKPOINT_WRITTEN, BPF_CORE_READ(s, cs_written));
    counter_add(C_CHECKPOINT_DROPPED, BPF_CORE_READ(s, cs_dropped));
    counter_add(C_CHECKPOINT_FORCED_TO_CLOSE, BPF_CORE_READ(s, cs_forced_to_close));

    if (tick == 0) {
        return 0;
    }

    histogram_incr(&checkpoint_latency, HISTOGRAM_POWER, BPF_CORE_READ(s, cs_chp_time) * tick);

    return 0;
}

// jbd2_lock_buffer_stall reports the stall in whole milliseconds, a scalar
// argument: no struct read, so this hook works even where the jbd2 structs are
// not in BTF.
static int __always_inline handle_lock_buffer_stall(unsigned long stall_ms) {
    counter_incr(C_LOCK_BUFFER_STALLS);
    histogram_incr(&lock_buffer_stall_latency, HISTOGRAM_POWER, (u64)stall_ms * NS_PER_MS);

    return 0;
}

static int __always_inline handle_sync_file_enter(int datasync) {
    counter_incr(datasync ? C_SYNC_FDATASYNC : C_SYNC_FSYNC);

    return 0;
}

static int __always_inline handle_sync_file_exit(int ret) {
    if (ret < 0) {
        counter_incr(C_SYNC_ERRORS);
    }

    return 0;
}

static int __always_inline handle_error(void) {
    counter_incr(C_ERRORS);

    return 0;
}

static int __always_inline handle_shutdown(void) {
    counter_incr(C_SHUTDOWNS);

    return 0;
}

// tp_btf and raw_tp twins share the handlers above; the unused variant of
// each hook is disabled at load time by mod.rs. Argument lists are the
// tracepoints' TP_PROTO; the jbd2 stats pointers are taken as void* because
// the struct types are CO-RE flavors declared above, not the kernel's names.

SEC("tp_btf/jbd2_run_stats")
int BPF_PROG(jbd2_run_stats_btf, dev_t dev, unsigned int tid, void* stats) {
    return handle_run_stats(stats);
}

SEC("raw_tp/jbd2_run_stats")
int BPF_PROG(jbd2_run_stats_raw, dev_t dev, unsigned int tid, void* stats) {
    return handle_run_stats(stats);
}

SEC("tp_btf/jbd2_checkpoint_stats")
int BPF_PROG(jbd2_checkpoint_stats_btf, dev_t dev, unsigned int tid, void* stats) {
    return handle_checkpoint_stats(stats);
}

SEC("raw_tp/jbd2_checkpoint_stats")
int BPF_PROG(jbd2_checkpoint_stats_raw, dev_t dev, unsigned int tid, void* stats) {
    return handle_checkpoint_stats(stats);
}

SEC("tp_btf/jbd2_lock_buffer_stall")
int BPF_PROG(jbd2_lock_buffer_stall_btf, dev_t dev, unsigned long stall_ms) {
    return handle_lock_buffer_stall(stall_ms);
}

SEC("raw_tp/jbd2_lock_buffer_stall")
int BPF_PROG(jbd2_lock_buffer_stall_raw, dev_t dev, unsigned long stall_ms) {
    return handle_lock_buffer_stall(stall_ms);
}

SEC("tp_btf/ext4_sync_file_enter")
int BPF_PROG(ext4_sync_file_enter_btf, struct file* file, int datasync) {
    return handle_sync_file_enter(datasync);
}

SEC("raw_tp/ext4_sync_file_enter")
int BPF_PROG(ext4_sync_file_enter_raw, struct file* file, int datasync) {
    return handle_sync_file_enter(datasync);
}

SEC("tp_btf/ext4_sync_file_exit")
int BPF_PROG(ext4_sync_file_exit_btf, struct inode* inode, int ret) {
    return handle_sync_file_exit(ret);
}

SEC("raw_tp/ext4_sync_file_exit")
int BPF_PROG(ext4_sync_file_exit_raw, struct inode* inode, int ret) {
    return handle_sync_file_exit(ret);
}

SEC("tp_btf/ext4_error")
int BPF_PROG(ext4_error_btf, struct super_block* sb, const char* function, unsigned int line) {
    return handle_error();
}

SEC("raw_tp/ext4_error")
int BPF_PROG(ext4_error_raw, struct super_block* sb, const char* function, unsigned int line) {
    return handle_error();
}

SEC("tp_btf/ext4_shutdown")
int BPF_PROG(ext4_shutdown_btf, struct super_block* sb, unsigned long flags) {
    return handle_shutdown();
}

SEC("raw_tp/ext4_shutdown")
int BPF_PROG(ext4_shutdown_raw, struct super_block* sb, unsigned long flags) {
    return handle_shutdown();
}

char LICENSE[] SEC("license") = "GPL";
