// SPDX-License-Identifier: GPL-2.0
// Copyright (c) 2024 The Rezolus Authors

#ifndef TASK_H
#define TASK_H

#include <vmlinux.h>
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>
#include "core_fixes.h"

#define TASK_COMM_LEN 16
#define TASK_CGROUP_NAME_LEN 64
#define MAX_PID 4194304
#define TASK_RINGBUF_CAPACITY 262144

// Task info sent when a new task is observed
struct task_info {
    u32 pid;
    u32 tgid;
    int cgroup_level;
    u8 comm[TASK_COMM_LEN];
    u8 cgroup_name[TASK_CGROUP_NAME_LEN];
    u8 cgroup_pname[TASK_CGROUP_NAME_LEN];
    u8 cgroup_gpname[TASK_CGROUP_NAME_LEN];
};

// Task exit notification
struct task_exit {
    u32 pid;
};

/**
 * populate_task_info - Fill in task_info struct from a task_struct
 * @task: The task_struct pointer
 * @info: The task_info struct to populate
 *
 * Populates the task_info with pid, tgid, comm, and cgroup hierarchy.
 */
static __always_inline void populate_task_info(struct task_struct* task, struct task_info* info) {
    info->pid = BPF_CORE_READ(task, pid);
    info->tgid = BPF_CORE_READ(task, tgid);

    bpf_probe_read_kernel_str(&info->comm, TASK_COMM_LEN, BPF_CORE_READ(task, comm));

    // Read cgroup info if available
    struct task_group* tg = BPF_CORE_READ(task, sched_task_group);
    if (tg) {
        info->cgroup_level = BPF_CORE_READ(tg, css.cgroup, level);

        // Read cgroup name
        bpf_probe_read_kernel_str(&info->cgroup_name, TASK_CGROUP_NAME_LEN,
                                  BPF_CORE_READ(tg, css.cgroup, kn, name));

        // Read parent cgroup name
        if (info->cgroup_level > 0) {
            struct kernfs_node* kn = BPF_CORE_READ(tg, css.cgroup, kn);
            struct kernfs_node* parent = get_kernfs_node_parent(kn);
            bpf_probe_read_kernel_str(&info->cgroup_pname, TASK_CGROUP_NAME_LEN,
                                      BPF_CORE_READ(parent, name));
        }

        // Read grandparent cgroup name
        if (info->cgroup_level > 1) {
            struct kernfs_node* kn = BPF_CORE_READ(tg, css.cgroup, kn);
            struct kernfs_node* parent = get_kernfs_node_parent(kn);
            struct kernfs_node* grandparent = get_kernfs_node_parent(parent);
            bpf_probe_read_kernel_str(&info->cgroup_gpname, TASK_CGROUP_NAME_LEN,
                                      BPF_CORE_READ(grandparent, name));
        }
    }
}

#endif // TASK_H
