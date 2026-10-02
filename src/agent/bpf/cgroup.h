#ifndef CGROUP_H
#define CGROUP_H

#include <vmlinux.h>
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>
#include "core_fixes.h"

#define CGROUP_NAME_LEN 64
#define MAX_CGROUPS 4096
#define RINGBUF_CAPACITY 262144

struct cgroup_info {
    int id;
    int level;
    u8 name[CGROUP_NAME_LEN];
    u8 pname[CGROUP_NAME_LEN];
    u8 gpname[CGROUP_NAME_LEN];
};

/**
 * handle_new_cgroup_read - The new-cgroup check for a caller that has already
 * read the css id and serial number
 * @css: The task group's cgroup_subsys_state, read only for a new cgroup
 * @cgroup_id: css->id
 * @serial_nr: css->serial_nr
 * @cgroup_serial_numbers: Map storing serial numbers for cgroup tracking
 * @cgroup_info_ringbuf: Ringbuf for passing cgroup info to userspace
 *
 * The caller reads the id and serial number once, with task_group_of() or
 * current_task_group(), directly from a BTF pointer where it can, and passes
 * them here, so the common case (a known cgroup) is one array lookup and a
 * compare.
 *
 * Returns:
 *  - 0 on success (a new cgroup, sent to userspace)
 *  - 1 if not a new cgroup (serial number matches)
 *  - -1 on error (invalid cgroup_id, lookup failure, or ringbuf full; the
 *    serial number is left unchanged so the next event retries)
 */
static __always_inline int handle_new_cgroup_read(struct cgroup_subsys_state* css, u32 cgroup_id,
                                                  u64 serial_nr, void* cgroup_serial_numbers,
                                                  void* cgroup_info_ringbuf) {
    if (cgroup_id >= MAX_CGROUPS) {
        return -1;
    }

    u64* elem = bpf_map_lookup_elem(cgroup_serial_numbers, &cgroup_id);

    if (!elem) {
        return -1;
    }

    if (*elem == serial_nr) {
        return 1;
    }

    struct cgroup_info* cginfo =
        bpf_ringbuf_reserve(cgroup_info_ringbuf, sizeof(struct cgroup_info), 0);
    if (!cginfo) {
        // The ringbuf is full. Leave the serial number unchanged so the next
        // event for this cgroup retries the send; updating it here would
        // permanently suppress the cgroup's name and hierarchy in userspace.
        return -1;
    }

    __builtin_memset(cginfo, 0, sizeof(struct cgroup_info));
    cginfo->id = cgroup_id;
    cginfo->level = BPF_CORE_READ(css, cgroup, level);

    if (cginfo->level == 0) {
        cginfo->name[0] = '/';
        cginfo->name[1] = '\0';
    } else {
        bpf_probe_read_kernel_str(&cginfo->name, CGROUP_NAME_LEN,
                                  BPF_CORE_READ(css, cgroup, kn, name));
    }

    if (cginfo->level > 0) {
        struct kernfs_node* kn = BPF_CORE_READ(css, cgroup, kn);
        struct kernfs_node* parent = get_kernfs_node_parent(kn);
        bpf_probe_read_kernel_str(&cginfo->pname, CGROUP_NAME_LEN, BPF_CORE_READ(parent, name));
    }

    if (cginfo->level > 1) {
        struct kernfs_node* kn = BPF_CORE_READ(css, cgroup, kn);
        struct kernfs_node* parent = get_kernfs_node_parent(kn);
        struct kernfs_node* grandparent = get_kernfs_node_parent(parent);
        bpf_probe_read_kernel_str(&cginfo->gpname, CGROUP_NAME_LEN,
                                  BPF_CORE_READ(grandparent, name));
    }

    bpf_ringbuf_submit(cginfo, 0);

    bpf_map_update_elem(cgroup_serial_numbers, &cgroup_id, &serial_nr, BPF_ANY);

    return 0;
}

/**
 * task_group_of - A task's CPU-controller task group, with its css id and
 * serial number
 * @task: the task. With @btf it must be a BTF-typed pointer: a tp_btf or
 *        fentry argument, or bpf_get_current_task_btf()
 * @btf: a compile-time constant; true in a BTF-typed program
 * @cgroup_id: out, css->id
 * @serial_nr: out, css->serial_nr
 *
 * Returns the task group, or NULL when the task has none.
 *
 * With @btf the three fields are plain loads from a BTF pointer, which the
 * verifier turns into inline guarded loads rather than helper calls.
 * Otherwise the task group is read once and the two fields from it: three
 * bpf_probe_read_kernel() calls, where reading each field from the task costs
 * two.
 */
static __always_inline struct task_group* task_group_of(struct task_struct* task, bool btf,
                                                        u32* cgroup_id, u64* serial_nr) {
    struct task_group* tg;

    if (btf) {
        tg = task->sched_task_group;
        if (!tg) {
            return NULL;
        }
        *cgroup_id = tg->css.id;
        *serial_nr = tg->css.serial_nr;
    } else {
        tg = BPF_CORE_READ(task, sched_task_group);
        if (!tg) {
            return NULL;
        }
        *cgroup_id = BPF_CORE_READ(tg, css.id);
        *serial_nr = BPF_CORE_READ(tg, css.serial_nr);
    }

    return tg;
}

/**
 * current_task_group - task_group_of() for the current task
 * @btf: true in a BTF-typed program (tp_btf, fentry); a compile-time constant
 *
 * The current task is a BTF pointer only through bpf_get_current_task_btf()
 * (5.11), which is checked at load. On an older kernel a BTF-typed program
 * falls back to the bpf_probe_read_kernel() path.
 */
static __always_inline struct task_group* current_task_group(bool btf, u32* cgroup_id,
                                                             u64* serial_nr) {
    if (btf && bpf_core_enum_value_exists(enum bpf_func_id, BPF_FUNC_get_current_task_btf)) {
        return task_group_of(bpf_get_current_task_btf(), true, cgroup_id, serial_nr);
    }

    return task_group_of((struct task_struct*)bpf_get_current_task(), false, cgroup_id,
                         serial_nr);
}

/**
 * handle_new_cgroup_from_css - Process a new cgroup from css and update tracking maps
 * @css: The cgroup_subsys_state pointer to extract cgroup info from
 * @cgroup_serial_numbers: Map storing serial numbers for cgroup tracking
 * @cgroup_info_ringbuf: Ringbuf for passing cgroup info to userspace
 *
 * This function is similar to handle_new_cgroup but works with a css pointer
 * directly instead of extracting it from a task_struct.
 *
 * Returns:
 *  - 0 on success
 *  - 1 if not a new cgroup (serial number matches)
 *  - -1 on error (invalid cgroup_id, lookup failure, or ringbuf full —
 *    the serial number is left unchanged so the next event retries)
 */
static __always_inline int handle_new_cgroup_from_css(struct cgroup_subsys_state* css,
                                                      void* cgroup_serial_numbers,
                                                      void* cgroup_info_ringbuf) {
    int cgroup_id = BPF_CORE_READ(css, id);
    u64 serial_nr = BPF_CORE_READ(css, serial_nr);

    if (cgroup_id < 0) {
        return -1;
    }

    return handle_new_cgroup_read(css, cgroup_id, serial_nr, cgroup_serial_numbers,
                                  cgroup_info_ringbuf);
}

#endif // CGROUP_H
