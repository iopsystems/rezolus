// SPDX-License-Identifier: GPL-2.0
// Copyright (c) 2020 Anton Protopopov
// Copyright (c) 2023 The Rezolus Authors
//
// Based on syscount(8) from BCC by Sasha Goldshtein

// NOTICE: this file is based off `syscount.bpf.c` from the BCC project
// <https://github.com/iovisor/bcc/> and has been modified for use within
// Rezolus.

// This BPF program tracks syscall enter and exit to provide metrics about
// syscall counts and latencies.

#include <vmlinux.h>
#include "../../../agent/bpf/helpers.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_tracing.h>
#include <bpf/bpf_core_read.h>

#define COUNTER_GROUP_WIDTH 16
#define HISTOGRAM_BUCKETS HISTOGRAM_BUCKETS_POW_3
#define HISTOGRAM_POWER 3
#define MAX_CPUS 1024
#define MAX_PID 4194304
#define MAX_SYSCALL_ID 1024

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(max_entries, MAX_PID);
    __type(key, u32);
    __type(value, u64);
} start SEC(".maps");

// tracks the latency distribution of all other syscalls
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS);
} other_latency SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS);
} read_latency SEC(".maps");

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
} poll_latency SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS);
} lock_latency SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS);
} time_latency SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS);
} sleep_latency SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS);
} socket_latency SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS);
} yield_latency SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS);
} filesystem_latency SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS);
} memory_latency SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS);
} process_latency SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS);
} query_latency SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS);
} ipc_latency SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS);
} timer_latency SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS);
} event_latency SEC(".maps");

// provides a lookup table from syscall id to a counter index offset
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_SYSCALL_ID);
} syscall_lut SEC(".maps");

SEC("tracepoint/raw_syscalls/sys_enter")
int sys_enter(struct trace_event_raw_sys_enter* args) {
    u64 id = bpf_get_current_pid_tgid();
    u32 tid = id;
    u64 ts;

    ts = bpf_ktime_get_ns();
    bpf_map_update_elem(&start, &tid, &ts, 0);
    return 0;
}

SEC("tracepoint/raw_syscalls/sys_exit")
int sys_exit(struct trace_event_raw_sys_exit* args) {
    u64 id = bpf_get_current_pid_tgid();
    u64 *start_ts, lat = 0;
    u32 tid = id, group = 0;

    if (args->id < 0) {
        return 0;
    }

    u32 syscall_id = args->id;

    start_ts = bpf_map_lookup_elem(&start, &tid);

    // possible we missed the start
    if (!start_ts || *start_ts == 0) {
        return 0;
    }

    lat = bpf_ktime_get_ns() - *start_ts;

    *start_ts = 0;

    // increment latency histogram for the syscall family
    if (syscall_id < MAX_SYSCALL_ID) {
        u32* counter_offset = bpf_map_lookup_elem(&syscall_lut, &syscall_id);

        if (counter_offset && *counter_offset && *counter_offset < COUNTER_GROUP_WIDTH) {
            group = (u32)*counter_offset;
        }
    }

    switch (group) {
    case 1:
        histogram_incr(&read_latency, HISTOGRAM_POWER, lat);
        break;
    case 2:
        histogram_incr(&write_latency, HISTOGRAM_POWER, lat);
        break;
    case 3:
        histogram_incr(&poll_latency, HISTOGRAM_POWER, lat);
        break;
    case 4:
        histogram_incr(&lock_latency, HISTOGRAM_POWER, lat);
        break;
    case 5:
        histogram_incr(&time_latency, HISTOGRAM_POWER, lat);
        break;
    case 6:
        histogram_incr(&sleep_latency, HISTOGRAM_POWER, lat);
        break;
    case 7:
        histogram_incr(&socket_latency, HISTOGRAM_POWER, lat);
        break;
    case 8:
        histogram_incr(&yield_latency, HISTOGRAM_POWER, lat);
        break;
    case 9:
        histogram_incr(&filesystem_latency, HISTOGRAM_POWER, lat);
        break;
    case 10:
        histogram_incr(&memory_latency, HISTOGRAM_POWER, lat);
        break;
    case 11:
        histogram_incr(&process_latency, HISTOGRAM_POWER, lat);
        break;
    case 12:
        histogram_incr(&query_latency, HISTOGRAM_POWER, lat);
        break;
    case 13:
        histogram_incr(&ipc_latency, HISTOGRAM_POWER, lat);
        break;
    case 14:
        histogram_incr(&timer_latency, HISTOGRAM_POWER, lat);
        break;
    case 15:
        histogram_incr(&event_latency, HISTOGRAM_POWER, lat);
        break;
    default:
        histogram_incr(&other_latency, HISTOGRAM_POWER, lat);
        break;
    }

    return 0;
}

char LICENSE[] SEC("license") = "GPL";
