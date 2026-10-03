// SPDX-License-Identifier: GPL-2.0
// Copyright (c) 2020 Anton Protopopov
// Copyright (c) 2023 The Rezolus Authors
//
// Based on syscount(8) from BCC by Sasha Goldshtein

// NOTICE: this file is based off `syscount.bpf.c` from the BCC project
// <https://github.com/iovisor/bcc/> and has been modified for use within
// Rezolus.

// One program per syscall hook for the `syscall` sampler: `sys_enter` counts
// the syscall (and attributes it to a cgroup) and stamps its start, and
// `sys_exit` turns the stamp into a latency. Each part is switched by
// read-only data written before load, so a part that is off is not in the
// loaded program. Before this the counts and the latencies were two samplers
// with a program each on `sys_enter`, and each program on a hook pays its own
// dispatch (docs/journal/2026-10-03-one-program-per-hook.md).

#include <vmlinux.h>
#include "../../../agent/bpf/btf_read.h"
#include "../../../agent/bpf/cgroup.h"
#include "../../../agent/bpf/helpers.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_tracing.h>
#include <bpf/bpf_core_read.h>

#define COUNTER_GROUP_WIDTH 24
#define HISTOGRAM_BUCKETS HISTOGRAM_BUCKETS_POW_3
#define HISTOGRAM_POWER 3
#define MAX_CPUS 1024
#define MAX_SYSCALL_ID 1024
#define MAX_PID 4194304

// Per-cgroup attribution is the config option `cgroup_attribution`, on by
// default for this sampler. Written into read-only data before load, so with
// it off the verifier removes the per-cgroup path (the task-group read, the
// new-cgroup check and the per-cgroup adds) rather than testing a flag on
// every event.
const volatile __u8 cgroup_attribution = 0;

// The sampler's parts, the config options `counts` and `latency` (both on by
// default). With `latency` off the `sys_exit` programs are not loaded either.
const volatile __u8 counts = 0;
const volatile __u8 latency = 0;

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
// 1..16 - grouped syscalls defined in userspace in the `syscall_lut` map
// 17..COUNTER_GROUP_WIDTH - padding: a bank is a whole number of cachelines
//                           (17 counters round up to 24), matching the
//                           userspace `Counters` layout in bpf/counters.rs
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
 * latency: the start stamp per thread and one histogram per syscall family
 */

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

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS);
} sync_latency SEC(".maps");

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

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CGROUPS);
} cgroup_syscall_sync SEC(".maps");

// `btf` is a compile-time constant from each program below: the tp_btf
// program reads the task group through a BTF task pointer, the raw_tp one
// through bpf_probe_read_kernel() (see current_task_group() in cgroup.h).
static __always_inline void count_syscall(long id, bool btf) {
    u32 offset, idx, group = 0;

    if (id < 0) {
        return;
    }

    u32 syscall_id = id;
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

    if (!cgroup_attribution) {
        return;
    }

    u32 cgroup_id = 0;
    u64 serial_nr = 0;
    struct task_group* tg = current_task_group(btf, &cgroup_id, &serial_nr);
    if (tg) {
        if (cgroup_id < MAX_CGROUPS) {
            int ret = handle_new_cgroup_read(&tg->css, cgroup_id, serial_nr, &cgroup_serial_numbers,
                                             &cgroup_info);

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
                bpf_map_update_elem(&cgroup_syscall_sync, &cgroup_id, &zero, BPF_ANY);
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
            case 16:
                array_incr(&cgroup_syscall_sync, cgroup_id);
                break;
            default:
                array_incr(&cgroup_syscall_other, cgroup_id);
                break;
            }
        }
    }

    return;
}

// The start stamp is taken after the counts, as near the syscall as the
// program gets, so that the latency does not include the counting (and the
// cgroup path) done on the way in.
static __always_inline int account_sys_enter(long id, bool btf) {
    if (counts) {
        count_syscall(id, btf);
    }

    if (latency) {
        u32 tid = bpf_get_current_pid_tgid();
        u64 ts = bpf_ktime_get_ns();
        bpf_map_update_elem(&start, &tid, &ts, 0);
    }

    return 0;
}

// The syscall number at exit, read from the registers as the kernel's
// syscall_get_nr() does and as the classic sys_exit tracepoint reported it.
// `btf` is a compile-time constant: true in the tp_btf program, whose `regs` is
// a BTF pointer (see BTF_READ in btf_read.h).
static __always_inline long exit_syscall_nr(struct pt_regs* regs, bool btf) {
#if defined(__TARGET_ARCH_x86)
    // x86's syscall_get_nr() returns orig_ax as an int
    return (long)(int)BTF_READ(btf, regs, orig_ax);
#elif defined(__TARGET_ARCH_arm64)
    return (long)BTF_READ(btf, regs, syscallno);
#else
#error "syscall_latency: unsupported architecture"
#endif
}

static __always_inline int account_sys_exit(struct pt_regs* regs, bool btf) {
    if (!latency) {
        return 0;
    }

    u64 id = bpf_get_current_pid_tgid();
    u64 *start_ts, lat = 0;
    u32 tid = id, group = 0;

    long nr = exit_syscall_nr(regs, btf);
    if (nr < 0) {
        return 0;
    }

    u32 syscall_id = nr;

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
    case 16:
        histogram_incr(&sync_latency, HISTOGRAM_POWER, lat);
        break;
    default:
        histogram_incr(&other_latency, HISTOGRAM_POWER, lat);
        break;
    }

    return 0;
}

// sys_enter's arguments are (struct pt_regs *regs, long id). A raw
// tracepoint program is handed them as they are; the classic tracepoint this
// replaces had the kernel copy the syscall number and all six arguments into a
// trace record before the program ran.
SEC("tp_btf/sys_enter")
int sys_enter_btf(u64* ctx) {
    return account_sys_enter((long)ctx[1], true);
}

SEC("raw_tp/sys_enter")
int sys_enter_raw(u64* ctx) {
    return account_sys_enter((long)ctx[1], false);
}

SEC("tp_btf/sys_exit")
int sys_exit_btf(u64* ctx) {
    return account_sys_exit((struct pt_regs*)ctx[0], true);
}

SEC("raw_tp/sys_exit")
int sys_exit_raw(u64* ctx) {
    return account_sys_exit((struct pt_regs*)ctx[0], false);
}

char LICENSE[] SEC("license") = "GPL";
