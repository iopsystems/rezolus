/* SPDX-License-Identifier: (LGPL-2.1 OR BSD-2-Clause) */
/* Copyright (c) 2021 Hengqi Chen */

#ifndef __CORE_FIXES_BPF_H
#define __CORE_FIXES_BPF_H

#include "vmlinux.h"
#include <bpf/bpf_core_read.h>

/**
 * commit 2f064a59a1 ("sched: Change task_struct::state") changes
 * the name of task_struct::state to task_struct::__state
 * see:
 *     https://github.com/torvalds/linux/commit/2f064a59a1
 */
struct task_struct___o {
    volatile long int state;
} __attribute__((preserve_access_index));

struct task_struct___x {
    unsigned int __state;
} __attribute__((preserve_access_index));

static __always_inline __s64 get_task_state(void* task) {
    struct task_struct___x* t = task;

    if (bpf_core_field_exists(t->__state))
        return BPF_CORE_READ(t, __state);
    return BPF_CORE_READ((struct task_struct___o*)task, state);
}

/**
 * commit 309dca309fc3 ("block: store a block_device pointer in struct bio")
 * adds a new member bi_bdev which is a pointer to struct block_device
 * see:
 *     https://github.com/torvalds/linux/commit/309dca309fc3
 */
struct bio___o {
    struct gendisk* bi_disk;
} __attribute__((preserve_access_index));

struct bio___x {
    struct block_device* bi_bdev;
} __attribute__((preserve_access_index));

static __always_inline struct gendisk* get_gendisk(void* bio) {
    struct bio___x* b = bio;

    if (bpf_core_field_exists(b->bi_bdev))
        return BPF_CORE_READ(b, bi_bdev, bd_disk);
    return BPF_CORE_READ((struct bio___o*)bio, bi_disk);
}

/**
 * commit d152c682f03c ("block: add an explicit ->disk backpointer to the
 * request_queue") and commit f3fa33acca9f ("block: remove the ->rq_disk
 * field in struct request") make some changes to `struct request` and
 * `struct request_queue`. Now, to get the `struct gendisk *` field in a CO-RE
 * way, we need both `struct request` and `struct request_queue`.
 * see:
 *     https://github.com/torvalds/linux/commit/d152c682f03c
 *     https://github.com/torvalds/linux/commit/f3fa33acca9f
 */
struct request_queue___x {
    struct gendisk* disk;
} __attribute__((preserve_access_index));

struct request___x {
    struct request_queue___x* q;
    struct gendisk* rq_disk;
} __attribute__((preserve_access_index));

static __always_inline struct gendisk* get_disk(void* request) {
    struct request___x* r = request;

    if (bpf_core_field_exists(r->rq_disk))
        return BPF_CORE_READ(r, rq_disk);
    return BPF_CORE_READ(r, q, disk);
}

/**
 * commit 6521f8917082("namei: prepare for idmapped mounts") add `struct
 * user_namespace *mnt_userns` as vfs_create() and vfs_unlink() first argument.
 * At the same time, struct renamedata {} add `struct user_namespace
 * *old_mnt_userns` item. Now, to kprobe vfs_create()/vfs_unlink() in a CO-RE
 * way, determine whether there is a `old_mnt_userns` field for `struct
 * renamedata` to decide which input parameter of the vfs_create() to use as
 * `dentry`.
 * see:
 *     https://github.com/torvalds/linux/commit/6521f8917082
 */
struct renamedata___x {
    struct user_namespace* old_mnt_userns;
} __attribute__((preserve_access_index));

static __always_inline bool renamedata_has_old_mnt_userns_field(void) {
    if (bpf_core_field_exists(struct renamedata___x, old_mnt_userns))
        return true;
    return false;
}

/**
 * commit 6b03518505da ("kernfs: hide new members of struct kernfs_node")
 * renames kernfs_node::parent to kernfs_node::__parent in kernel v6.12+
 * to prevent out-of-tree modules from accessing it directly.
 * see:
 *     https://github.com/torvalds/linux/commit/6b03518505da
 */
struct kernfs_node___old {
    struct kernfs_node* parent;
} __attribute__((preserve_access_index));

struct kernfs_node___new {
    struct kernfs_node* __parent;
} __attribute__((preserve_access_index));

static __always_inline struct kernfs_node* get_kernfs_node_parent(void* kn) {
    struct kernfs_node___new* node = kn;

    if (bpf_core_field_exists(node->__parent))
        return BPF_CORE_READ(node, __parent);
    return BPF_CORE_READ((struct kernfs_node___old*)kn, parent);
}

#endif /* __CORE_FIXES_BPF_H */