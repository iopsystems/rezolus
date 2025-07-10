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
#include "../../../agent/bpf/cgroup.h"
#include "../../../agent/bpf/helpers.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_tracing.h>
#include <bpf/bpf_core_read.h>

#define COUNTER_GROUP_WIDTH 16
#define MAX_CPUS 1024
#define MAX_SYSCALL_ID 1024
#define MAX_PID 4194304

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

// counters for syscalls
// 0 - other
// 1..COUNTER_GROUP_WIDTH - grouped syscalls defined in userspace in the
//                          `syscall_lut` map
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CPUS* COUNTER_GROUP_WIDTH);
} counters SEC(".maps");

// provides a lookup table from syscall id to a counter index offset
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_SYSCALL_ID);
} syscall_lut SEC(".maps");

/*
 * per-cgroup counters
 */

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CGROUPS);
} cgroup_syscall_other SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CGROUPS);
} cgroup_syscall_read SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CGROUPS);
} cgroup_syscall_write SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CGROUPS);
} cgroup_syscall_poll SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CGROUPS);
} cgroup_syscall_lock SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CGROUPS);
} cgroup_syscall_time SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CGROUPS);
} cgroup_syscall_sleep SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CGROUPS);
} cgroup_syscall_socket SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CGROUPS);
} cgroup_syscall_yield SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CGROUPS);
} cgroup_syscall_filesystem SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CGROUPS);
} cgroup_syscall_memory SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CGROUPS);
} cgroup_syscall_process SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CGROUPS);
} cgroup_syscall_query SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CGROUPS);
} cgroup_syscall_ipc SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CGROUPS);
} cgroup_syscall_timer SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CGROUPS);
} cgroup_syscall_event SEC(".maps");

SEC("tracepoint/raw_syscalls/sys_enter")
int sys_enter(struct trace_event_raw_sys_enter* args) {
    u32 offset, idx, group = 0;
    u64* elem;

    if (args->id < 0) {
        return 0;
    }

    u32 syscall_id = args->id;
    offset = COUNTER_GROUP_WIDTH * bpf_get_smp_processor_id();

    // for some syscalls, we track counts by "family" of syscall. check the
    // lookup table and increment the appropriate counter
    idx = 0;
    if (syscall_id < MAX_SYSCALL_ID) {
        u32* counter_offset = bpf_map_lookup_elem(&syscall_lut, &syscall_id);

        if (counter_offset && *counter_offset && *counter_offset < COUNTER_GROUP_WIDTH) {
            group = (u32)*counter_offset;
        }
    }

    idx = offset + group;
    array_incr(&counters, idx);

    struct task_struct* current = bpf_get_current_task_btf();

    if (bpf_core_field_exists(current->sched_task_group)) {
        int cgroup_id = current->sched_task_group->css.id;
        u64 serial_nr = current->sched_task_group->css.serial_nr;

        if (cgroup_id < MAX_CGROUPS) {

            int ret = handle_new_cgroup(current, &cgroup_serial_numbers, &cgroup_info);

            if (ret == 0) {
                // New cgroup detected, zero all counters
                u64 zero = 0;
                bpf_map_update_elem(&cgroup_syscall_other, &cgroup_id, &zero, BPF_ANY);
                bpf_map_update_elem(&cgroup_syscall_read, &cgroup_id, &zero, BPF_ANY);
                bpf_map_update_elem(&cgroup_syscall_write, &cgroup_id, &zero, BPF_ANY);
                bpf_map_update_elem(&cgroup_syscall_poll, &cgroup_id, &zero, BPF_ANY);
                bpf_map_update_elem(&cgroup_syscall_lock, &cgroup_id, &zero, BPF_ANY);
                bpf_map_update_elem(&cgroup_syscall_time, &cgroup_id, &zero, BPF_ANY);
                bpf_map_update_elem(&cgroup_syscall_sleep, &cgroup_id, &zero, BPF_ANY);
                bpf_map_update_elem(&cgroup_syscall_socket, &cgroup_id, &zero, BPF_ANY);
                bpf_map_update_elem(&cgroup_syscall_yield, &cgroup_id, &zero, BPF_ANY);
                bpf_map_update_elem(&cgroup_syscall_filesystem, &cgroup_id, &zero, BPF_ANY);
                bpf_map_update_elem(&cgroup_syscall_memory, &cgroup_id, &zero, BPF_ANY);
                bpf_map_update_elem(&cgroup_syscall_process, &cgroup_id, &zero, BPF_ANY);
                bpf_map_update_elem(&cgroup_syscall_query, &cgroup_id, &zero, BPF_ANY);
                bpf_map_update_elem(&cgroup_syscall_ipc, &cgroup_id, &zero, BPF_ANY);
                bpf_map_update_elem(&cgroup_syscall_timer, &cgroup_id, &zero, BPF_ANY);
                bpf_map_update_elem(&cgroup_syscall_event, &cgroup_id, &zero, BPF_ANY);
            }

            switch (group) {
            case 1:
                array_incr(&cgroup_syscall_read, cgroup_id);
                break;
            case 2:
                array_incr(&cgroup_syscall_write, cgroup_id);
                break;
            case 3:
                array_incr(&cgroup_syscall_poll, cgroup_id);
                break;
            case 4:
                array_incr(&cgroup_syscall_lock, cgroup_id);
                break;
            case 5:
                array_incr(&cgroup_syscall_time, cgroup_id);
                break;
            case 6:
                array_incr(&cgroup_syscall_sleep, cgroup_id);
                break;
            case 7:
                array_incr(&cgroup_syscall_socket, cgroup_id);
                break;
            case 8:
                array_incr(&cgroup_syscall_yield, cgroup_id);
                break;
            case 9:
                array_incr(&cgroup_syscall_filesystem, cgroup_id);
                break;
            case 10:
                array_incr(&cgroup_syscall_memory, cgroup_id);
                break;
            case 11:
                array_incr(&cgroup_syscall_process, cgroup_id);
                break;
            case 12:
                array_incr(&cgroup_syscall_query, cgroup_id);
                break;
            case 13:
                array_incr(&cgroup_syscall_ipc, cgroup_id);
                break;
            case 14:
                array_incr(&cgroup_syscall_timer, cgroup_id);
                break;
            case 15:
                array_incr(&cgroup_syscall_event, cgroup_id);
                break;
            default:
                array_incr(&cgroup_syscall_other, cgroup_id);
                break;
            }
        }
    }

    return 0;
}

char LICENSE[] SEC("license") = "GPL";
