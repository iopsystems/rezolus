// SPDX-License-Identifier: GPL-2.0
// Copyright (c) 2025 The Rezolus Authors

// This BPF program tracks CPU migrations using software events.

#include <vmlinux.h>
#include "../../../agent/bpf/cgroup_info.h"
#include "../../../agent/bpf/helpers.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_tracing.h>

#define COUNTER_GROUP_WIDTH 8
#define MAX_CPUS 1024
#define MAX_CGROUPS 4096
#define MAX_PID 4194304
#define RINGBUF_CAPACITY 262144

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

// per-CPU migration counts (migrations from this CPU)
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CPUS);
} cpu_migrations_from SEC(".maps");

// per-CPU migration counts (migrations to this CPU)
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CPUS);
} cpu_migrations_to SEC(".maps");

// per-cgroup migration counts
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CGROUPS);
} cgroup_cpu_migrations SEC(".maps");

// For storing the CPU a process was last seen on
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(max_entries, MAX_PID);
    __type(key, u32);  // pid
    __type(value, u32); // cpu
} last_cpu SEC(".maps");

SEC("tp_btf/sched_switch")
int handle__sched_switch(u64 *ctx)
{
    /* TP_PROTO(bool preempt, struct task_struct *prev, struct task_struct *next) */
    struct task_struct *prev = (struct task_struct *)ctx[1];
    struct task_struct *next = (struct task_struct *)ctx[2];

    u32 cpu = bpf_get_smp_processor_id();
    u32 prev_pid = BPF_CORE_READ(prev, pid);
    u32 next_pid = BPF_CORE_READ(next, pid);

    // Skip kernel threads and idle task (pid 0)
    if (next_pid == 0) {
        return 0;
    }

    // find the last cpu the task ran on
    u32 *last_cpu_ptr = bpf_map_lookup_elem(&last_cpu, &next_pid);

    // check the ptr and that the last cpu is known (it is stored one-indexed)
    if (last_cpu_ptr && *last_cpu_ptr) {
        // convert to zero-indexed
        u32 old_cpu = *last_cpu_ptr - 1;

        // check if this is a migration
        if (old_cpu != cpu) {
            // increment counters
            array_incr(&cpu_migrations_from, old_cpu);
            array_incr(&cpu_migrations_to, cpu);

            // handle per-cgroup accounting
            if (bpf_core_field_exists(next->sched_task_group)) {
                int cgroup_id = BPF_CORE_READ(next, sched_task_group, css.id);
                u64 serial_nr = BPF_CORE_READ(next, sched_task_group, css.serial_nr);

                if (cgroup_id && cgroup_id < MAX_CGROUPS) {
                    // Check if this is a new cgroup by checking the serial number
                    u64 *elem = bpf_map_lookup_elem(&cgroup_serial_numbers, &cgroup_id);

                    if (elem && *elem != serial_nr) {
                        // Zero the counter, it will not be exported until non-zero
                        u64 zero = 0;
                        bpf_map_update_elem(&cgroup_cpu_migrations, &cgroup_id, &zero, BPF_ANY);

                        // Initialize the cgroup info
                        struct cgroup_info cginfo = {
                            .id = cgroup_id,
                            .level = BPF_CORE_READ(next, sched_task_group, css.cgroup, level),
                        };

                        // Read the cgroup name hierarchy
                        bpf_probe_read_kernel_str(&cginfo.name, CGROUP_NAME_LEN,
                            BPF_CORE_READ(next, sched_task_group, css.cgroup, kn, name));
                        bpf_probe_read_kernel_str(&cginfo.pname, CGROUP_NAME_LEN,
                            BPF_CORE_READ(next, sched_task_group, css.cgroup, kn, parent, name));
                        bpf_probe_read_kernel_str(&cginfo.gpname, CGROUP_NAME_LEN,
                            BPF_CORE_READ(next, sched_task_group, css.cgroup, kn, parent, parent, name));

                        // Push the cgroup info into the ringbuf
                        bpf_ringbuf_output(&cgroup_info, &cginfo, sizeof(cginfo), 0);

                        // Update the serial number in the local map
                        bpf_map_update_elem(&cgroup_serial_numbers, &cgroup_id, &serial_nr, BPF_ANY);
                    }

                    // Increment the per-cgroup counter
                    array_incr(&cgroup_cpu_migrations, cgroup_id);
                }
            }
        }
    }

    // store the current cpu for the next task (converted to one-indexed)
    cpu = cpu + 1;
    bpf_map_update_elem(&last_cpu, &next_pid, &cpu, BPF_ANY);

    return 0;
}

char LICENSE[] SEC("license") = "GPL";
