// SPDX-License-Identifier: GPL-2.0
// Copyright (c) 2026 The Rezolus Authors

// This BPF program instruments the kernel's page-cache writeback: the
// throttle that makes a writer sleep when dirty pages near their limit
// (balance_dirty_pages), the flusher work items and why they ran
// (writeback_start), and the pages the flushers wrote (writeback_pages_written).
// Filesystem-agnostic. Design: docs/journal/2026-09-28-filesystem-telemetry-gaps.md, C1.
//
// Every hook is a tracepoint with a tp_btf twin and a raw_tp twin sharing one
// handler; mod.rs disables the unused twin per hook on kernel_btf_has_tracepoints.
//
// balance_dirty_pages has two known argument lists. Through the kernels the
// vendored headers came from it is
//   (wb, thresh, bg_thresh, dirty, bdi_thresh, bdi_dirty, dirty_ratelimit,
//    task_ratelimit, dirtied, period, pause, start_time)          -- 12 args
// and a later rework passes the dirty_throttle_control instead of its fields:
//   (wb, dtc, dirty_ratelimit, task_ratelimit, dirtied, period, pause,
//    start_time)                                                 -- 8 args.
// A tp_btf/raw_tp program reads arguments by position, so a program for one
// arity reads the wrong argument on the other with no error. There is one
// program per arity below and mod.rs selects on the argument count BTF
// reports (kernel_btf_tracepoint_arg_count); an unknown count disables the
// hook rather than guessing.

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

// counters: one bank of COUNTER_GROUP_WIDTH per CPU. The order MUST match the
// `counters` vec in mod.rs.
#define C_THROTTLE_CHECKS 0
#define C_THROTTLE_EVENTS 1
#define C_THROTTLED_TIME 2
#define C_PAGES_WRITTEN 3
// runs by reason: C_RUNS_BASE + one slot per wb_reason, in the order of
// mod.rs's `counters` vec (background, vmscan, sync, periodic, laptop_timer,
// fs_free_space, forker_thread, foreign_flush).
#define C_RUNS_BASE 4
#define RUN_REASONS 8

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CPUS* COUNTER_GROUP_WIDTH);
} counters SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS);
} throttle_latency SEC(".maps");

static __always_inline void counter_add(u32 counter, u64 value) {
    u32 idx = COUNTER_GROUP_WIDTH * bpf_get_smp_processor_id() + counter;

    array_add(&counters, idx, value);
}

static __always_inline void counter_incr(u32 counter) {
    counter_add(counter, 1);
}

// `pause` is the sleep the throttle imposes on the caller, in milliseconds;
// zero or negative means the check did not throttle (negative encodes the
// "think time" credit the kernel carries between calls).
static int __always_inline handle_balance_dirty_pages(long pause) {
    counter_incr(C_THROTTLE_CHECKS);

    if (pause > 0) {
        u64 ns = (u64)pause * NS_PER_MS;

        counter_incr(C_THROTTLE_EVENTS);
        counter_add(C_THROTTLED_TIME, ns);
        histogram_incr(&throttle_latency, HISTOGRAM_POWER, ns);
    }

    return 0;
}

// Map the kernel's wb_reason to a counter slot through CO-RE enum
// relocation, so a kernel that renumbers the enum still lands each reason in
// the slot mod.rs labels for it. A reason this program does not know is not
// counted.
static __always_inline int reason_slot(int reason) {
    if (reason == bpf_core_enum_value(enum wb_reason, WB_REASON_BACKGROUND))
        return 0;
    if (reason == bpf_core_enum_value(enum wb_reason, WB_REASON_VMSCAN))
        return 1;
    if (reason == bpf_core_enum_value(enum wb_reason, WB_REASON_SYNC))
        return 2;
    if (reason == bpf_core_enum_value(enum wb_reason, WB_REASON_PERIODIC))
        return 3;
    if (reason == bpf_core_enum_value(enum wb_reason, WB_REASON_LAPTOP_TIMER))
        return 4;
    if (reason == bpf_core_enum_value(enum wb_reason, WB_REASON_FS_FREE_SPACE))
        return 5;
    if (reason == bpf_core_enum_value(enum wb_reason, WB_REASON_FORKER_THREAD))
        return 6;
    if (reason == bpf_core_enum_value(enum wb_reason, WB_REASON_FOREIGN_FLUSH))
        return 7;
    return -1;
}

// writeback_start fires once per writeback work item, on the flusher thread,
// before the pages are written.
static int __always_inline handle_writeback_start(struct wb_writeback_work* work) {
    int slot;

    // tp_btf pointer arguments are trusted_ptr_or_null to the verifier and
    // must be null-checked before BPF_CORE_READ's offset arithmetic.
    if (!work) {
        return 0;
    }

    slot = reason_slot(BPF_CORE_READ(work, reason));

    if (slot >= 0 && slot < RUN_REASONS) {
        counter_incr(C_RUNS_BASE + slot);
    }

    return 0;
}

// writeback_pages_written fires once per flusher pass with the pages it wrote.
static int __always_inline handle_writeback_pages_written(long pages) {
    if (pages > 0) {
        counter_add(C_PAGES_WRITTEN, (u64)pages);
    }

    return 0;
}

// tp_btf and raw_tp twins share the handlers above. The balance_dirty_pages
// programs come in two arities; see the header comment.

SEC("tp_btf/balance_dirty_pages")
int BPF_PROG(balance_dirty_pages_12_btf, struct bdi_writeback* wb, unsigned long thresh,
             unsigned long bg_thresh, unsigned long dirty, unsigned long bdi_thresh,
             unsigned long bdi_dirty, unsigned long dirty_ratelimit, unsigned long task_ratelimit,
             unsigned long dirtied, unsigned long period, long pause, unsigned long start_time) {
    return handle_balance_dirty_pages(pause);
}

SEC("raw_tp/balance_dirty_pages")
int BPF_PROG(balance_dirty_pages_12_raw, struct bdi_writeback* wb, unsigned long thresh,
             unsigned long bg_thresh, unsigned long dirty, unsigned long bdi_thresh,
             unsigned long bdi_dirty, unsigned long dirty_ratelimit, unsigned long task_ratelimit,
             unsigned long dirtied, unsigned long period, long pause, unsigned long start_time) {
    return handle_balance_dirty_pages(pause);
}

SEC("tp_btf/balance_dirty_pages")
int BPF_PROG(balance_dirty_pages_8_btf, struct bdi_writeback* wb, void* dtc,
             unsigned long dirty_ratelimit, unsigned long task_ratelimit, unsigned long dirtied,
             unsigned long period, long pause, unsigned long start_time) {
    return handle_balance_dirty_pages(pause);
}

SEC("raw_tp/balance_dirty_pages")
int BPF_PROG(balance_dirty_pages_8_raw, struct bdi_writeback* wb, void* dtc,
             unsigned long dirty_ratelimit, unsigned long task_ratelimit, unsigned long dirtied,
             unsigned long period, long pause, unsigned long start_time) {
    return handle_balance_dirty_pages(pause);
}

SEC("tp_btf/writeback_start")
int BPF_PROG(writeback_start_btf, struct bdi_writeback* wb, struct wb_writeback_work* work) {
    return handle_writeback_start(work);
}

SEC("raw_tp/writeback_start")
int BPF_PROG(writeback_start_raw, struct bdi_writeback* wb, struct wb_writeback_work* work) {
    return handle_writeback_start(work);
}

SEC("tp_btf/writeback_pages_written")
int BPF_PROG(writeback_pages_written_btf, long pages) {
    return handle_writeback_pages_written(pages);
}

SEC("raw_tp/writeback_pages_written")
int BPF_PROG(writeback_pages_written_raw, long pages) {
    return handle_writeback_pages_written(pages);
}

char LICENSE[] SEC("license") = "GPL";
