// SPDX-License-Identifier: GPL-2.0
// Copyright (c) 2026 The Rezolus Authors

// This BPF program instruments ext4's block allocator, its writeback results,
// inode allocation and freeing, discard/trim, and the synchronous metadata
// reads (inode-table and bitmap blocks) that land on a request's own thread.
// Design: docs/journal/2026-09-28-ext4-sampler.md (phase 2) and
// docs/journal/2026-09-28-filesystem-telemetry-gaps.md (C2, C3).
//
// Every hook is a tracepoint with a tp_btf twin and a raw_tp twin sharing one
// handler; mod.rs disables the unused twin per hook on
// kernel_btf_has_tracepoints (module BTF included: ext4 is a module on most
// x86_64 distribution kernels).

#include <vmlinux.h>
#include "../../../agent/bpf/helpers.h"
#include "../../../agent/bpf/filesystem.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_tracing.h>

#define COUNTER_GROUP_WIDTH 24
#define HISTOGRAM_BUCKETS HISTOGRAM_BUCKETS_POW_3
#define HISTOGRAM_POWER 3
#define MAX_CPUS 1024

// The allocator's context, as passed to ext4_mballoc_alloc. Declared as CO-RE
// flavors (the ___rz suffix is stripped when libbpf matches against the
// running kernel's BTF) because the vendored x86_64 vmlinux.h lacks the ext4
// types. Only the fields read are declared; offsets come from the kernel.
//
// ac_criteria was a __u8 until the allocator criteria became an enum (4
// bytes) in 6.5. It is read with BPF_CORE_READ_BITFIELD_PROBED, which takes
// the field's size from the kernel's BTF, so the declaration below is a
// placeholder for the name, not a claim about the width.
struct ext4_free_extent___rz {
    __u32 fe_logical;
    int fe_start;
    unsigned int fe_group;
    int fe_len;
} __attribute__((preserve_access_index));

struct ext4_allocation_context___rz {
    struct super_block* ac_sb;
    struct ext4_free_extent___rz ac_o_ex;
    struct ext4_free_extent___rz ac_b_ex;
    __u32 ac_flags;
    __u16 ac_groups_scanned;
    __u16 ac_found;
    __u8 ac_criteria;
} __attribute__((preserve_access_index));

// counters: one bank of COUNTER_GROUP_WIDTH per (CPU, filesystem slot); the
// slot comes from fs_slot() on the superblock's device, 0 for a device the
// mount table does not know (bpf/filesystems.rs). The order MUST match the
// `counters` vec in mod.rs.
#define C_ALLOCATIONS 0
#define C_ALLOC_BLOCKS_REQUESTED 1
#define C_ALLOC_BLOCKS_ALLOCATED 2
#define C_ALLOC_GROUPS_SCANNED 3
// allocations by the criterion the allocator finished at: slots for 0, 1,
// 2, 3 and "4 or higher" (kernels from 6.5 have five criteria, older four).
#define C_ALLOC_CR_BASE 4
#define ALLOC_CR_SLOTS 5
#define C_FREED_BLOCKS 9
#define C_INODES_ALLOCATED 10
#define C_INODES_FREED 11
#define C_WRITEPAGES 12
#define C_WRITEPAGES_PAGES_WRITTEN 13
#define C_WRITEPAGES_PAGES_SKIPPED 14
#define C_WRITEPAGES_ERRORS 15
#define C_TRIMMED_BLOCKS 16
#define C_PREALLOC_DISCARDS 17
#define C_PREALLOC_DISCARDED_BLOCKS 18
#define C_INODE_LOADS 19
#define C_BLOCK_BITMAP_LOADS 20
#define C_INODE_BITMAP_LOADS 21

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CPUS* MAX_FILESYSTEMS* COUNTER_GROUP_WIDTH);
} counters SEC(".maps");

// Allocated extent length, in filesystem blocks.
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS);
} allocation_size SEC(".maps");

static __always_inline void counter_add(u32 slot, u32 counter, u64 value) {
    array_add(&counters, fs_counter_idx(slot, counter, COUNTER_GROUP_WIDTH), value);
}

static __always_inline void counter_incr(u32 slot, u32 counter) {
    counter_add(slot, counter, 1);
}

// ext4_mballoc_alloc fires once per extent allocation, after the allocator
// has chosen. ac_o_ex is what was asked for, ac_b_ex what was found; the
// difference between their lengths, and the number of block groups scanned
// and the criterion reached to find it, are the allocator's effort and the
// free-space fragmentation seen from inside the filesystem.
static int __always_inline handle_mballoc_alloc(void* ctx) {
    struct ext4_allocation_context___rz* ac = ctx;
    int requested, allocated;
    u32 cr, slot;

    // tp_btf pointer arguments are trusted_ptr_or_null to the verifier and
    // must be null-checked before BPF_CORE_READ's offset arithmetic.
    if (!ac) {
        return 0;
    }

    slot = fs_slot(sb_dev(BPF_CORE_READ(ac, ac_sb)));
    requested = BPF_CORE_READ(ac, ac_o_ex.fe_len);
    allocated = BPF_CORE_READ(ac, ac_b_ex.fe_len);
    cr = BPF_CORE_READ_BITFIELD_PROBED(ac, ac_criteria);

    counter_incr(slot, C_ALLOCATIONS);
    if (requested > 0) {
        counter_add(slot, C_ALLOC_BLOCKS_REQUESTED, (u64)requested);
    }
    if (allocated > 0) {
        counter_add(slot, C_ALLOC_BLOCKS_ALLOCATED, (u64)allocated);
        histogram_incr(&allocation_size, HISTOGRAM_POWER, (u64)allocated);
    }
    counter_add(slot, C_ALLOC_GROUPS_SCANNED, BPF_CORE_READ(ac, ac_groups_scanned));

    if (cr >= ALLOC_CR_SLOTS - 1) {
        cr = ALLOC_CR_SLOTS - 1;
    }
    counter_incr(slot, C_ALLOC_CR_BASE + cr);

    return 0;
}

// ext4_writepages_result fires once per writeback pass over an inode.
static int __always_inline handle_writepages_result(u32 slot, struct writeback_control* wbc,
                                                     int ret, int pages_written) {
    long skipped;

    counter_incr(slot, C_WRITEPAGES);

    if (pages_written > 0) {
        counter_add(slot, C_WRITEPAGES_PAGES_WRITTEN, (u64)pages_written);
    }

    if (ret < 0) {
        counter_incr(slot, C_WRITEPAGES_ERRORS);
    }

    if (!wbc) {
        return 0;
    }

    skipped = BPF_CORE_READ(wbc, pages_skipped);
    if (skipped > 0) {
        counter_add(slot, C_WRITEPAGES_PAGES_SKIPPED, (u64)skipped);
    }

    return 0;
}

// tp_btf and raw_tp twins share the handlers above. Argument lists are the
// tracepoints' TP_PROTO. ext4_read_block_bitmap_load gained a third argument
// (bool prefetch) in 5.9; the programs below take only the two that every
// kernel passes, so one program fits both arities.

SEC("tp_btf/ext4_mballoc_alloc")
int BPF_PROG(ext4_mballoc_alloc_btf, void* ac) {
    return handle_mballoc_alloc(ac);
}

SEC("raw_tp/ext4_mballoc_alloc")
int BPF_PROG(ext4_mballoc_alloc_raw, void* ac) {
    return handle_mballoc_alloc(ac);
}

SEC("tp_btf/ext4_free_blocks")
int BPF_PROG(ext4_free_blocks_btf, struct inode* inode, __u64 block, unsigned long count,
             int flags) {
    counter_add(fs_slot(inode_dev(inode)), C_FREED_BLOCKS, count);
    return 0;
}

SEC("raw_tp/ext4_free_blocks")
int BPF_PROG(ext4_free_blocks_raw, struct inode* inode, __u64 block, unsigned long count,
             int flags) {
    counter_add(fs_slot(inode_dev(inode)), C_FREED_BLOCKS, count);
    return 0;
}

SEC("tp_btf/ext4_allocate_inode")
int BPF_PROG(ext4_allocate_inode_btf, struct inode* inode, struct inode* dir, int mode) {
    counter_incr(fs_slot(inode_dev(inode)), C_INODES_ALLOCATED);
    return 0;
}

SEC("raw_tp/ext4_allocate_inode")
int BPF_PROG(ext4_allocate_inode_raw, struct inode* inode, struct inode* dir, int mode) {
    counter_incr(fs_slot(inode_dev(inode)), C_INODES_ALLOCATED);
    return 0;
}

SEC("tp_btf/ext4_free_inode")
int BPF_PROG(ext4_free_inode_btf, struct inode* inode) {
    counter_incr(fs_slot(inode_dev(inode)), C_INODES_FREED);
    return 0;
}

SEC("raw_tp/ext4_free_inode")
int BPF_PROG(ext4_free_inode_raw, struct inode* inode) {
    counter_incr(fs_slot(inode_dev(inode)), C_INODES_FREED);
    return 0;
}

SEC("tp_btf/ext4_writepages_result")
int BPF_PROG(ext4_writepages_result_btf, struct inode* inode, struct writeback_control* wbc,
             int ret, int pages_written) {
    return handle_writepages_result(fs_slot(inode_dev(inode)), wbc, ret, pages_written);
}

SEC("raw_tp/ext4_writepages_result")
int BPF_PROG(ext4_writepages_result_raw, struct inode* inode, struct writeback_control* wbc,
             int ret, int pages_written) {
    return handle_writepages_result(fs_slot(inode_dev(inode)), wbc, ret, pages_written);
}

SEC("tp_btf/ext4_trim_extent")
int BPF_PROG(ext4_trim_extent_btf, struct super_block* sb, unsigned int group, int start,
             int len) {
    if (len > 0) {
        counter_add(fs_slot(sb_dev(sb)), C_TRIMMED_BLOCKS, (u64)len);
    }
    return 0;
}

SEC("raw_tp/ext4_trim_extent")
int BPF_PROG(ext4_trim_extent_raw, struct super_block* sb, unsigned int group, int start,
             int len) {
    if (len > 0) {
        counter_add(fs_slot(sb_dev(sb)), C_TRIMMED_BLOCKS, (u64)len);
    }
    return 0;
}

SEC("tp_btf/ext4_discard_preallocations")
int BPF_PROG(ext4_discard_preallocations_btf, struct inode* inode, unsigned int len,
             unsigned int needed) {
    u32 slot = fs_slot(inode_dev(inode));

    counter_incr(slot, C_PREALLOC_DISCARDS);
    counter_add(slot, C_PREALLOC_DISCARDED_BLOCKS, len);
    return 0;
}

SEC("raw_tp/ext4_discard_preallocations")
int BPF_PROG(ext4_discard_preallocations_raw, struct inode* inode, unsigned int len,
             unsigned int needed) {
    u32 slot = fs_slot(inode_dev(inode));

    counter_incr(slot, C_PREALLOC_DISCARDS);
    counter_add(slot, C_PREALLOC_DISCARDED_BLOCKS, len);
    return 0;
}

// ext4_load_inode fires in __ext4_get_inode_loc only when the inode-table
// block is not in the buffer cache and must be read: one synchronous 4 KiB
// read on the calling thread per event.
SEC("tp_btf/ext4_load_inode")
int BPF_PROG(ext4_load_inode_btf, struct super_block* sb, unsigned long ino) {
    counter_incr(fs_slot(sb_dev(sb)), C_INODE_LOADS);
    return 0;
}

SEC("raw_tp/ext4_load_inode")
int BPF_PROG(ext4_load_inode_raw, struct super_block* sb, unsigned long ino) {
    counter_incr(fs_slot(sb_dev(sb)), C_INODE_LOADS);
    return 0;
}

SEC("tp_btf/ext4_read_block_bitmap_load")
int BPF_PROG(ext4_read_block_bitmap_load_btf, struct super_block* sb, unsigned long group) {
    counter_incr(fs_slot(sb_dev(sb)), C_BLOCK_BITMAP_LOADS);
    return 0;
}

SEC("raw_tp/ext4_read_block_bitmap_load")
int BPF_PROG(ext4_read_block_bitmap_load_raw, struct super_block* sb, unsigned long group) {
    counter_incr(fs_slot(sb_dev(sb)), C_BLOCK_BITMAP_LOADS);
    return 0;
}

SEC("tp_btf/ext4_load_inode_bitmap")
int BPF_PROG(ext4_load_inode_bitmap_btf, struct super_block* sb, unsigned long group) {
    counter_incr(fs_slot(sb_dev(sb)), C_INODE_BITMAP_LOADS);
    return 0;
}

SEC("raw_tp/ext4_load_inode_bitmap")
int BPF_PROG(ext4_load_inode_bitmap_raw, struct super_block* sb, unsigned long group) {
    counter_incr(fs_slot(sb_dev(sb)), C_INODE_BITMAP_LOADS);
    return 0;
}

char LICENSE[] SEC("license") = "GPL";
