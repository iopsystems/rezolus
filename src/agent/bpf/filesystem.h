// SPDX-License-Identifier: GPL-2.0
// Copyright (c) 2026 The Rezolus Authors

// Per-filesystem counter banks. A hook inside a filesystem has the device
// (dev_t) in hand; this header turns it into a slot and indexes a counter map
// laid out as MAX_CPUS x MAX_FILESYSTEMS cacheline-padded banks, which the
// userspace FilesystemCounters reader (bpf/counters.rs) sums per slot.
// Assignment and the slot-0 rule are documented in bpf/filesystems.rs.

#ifndef FILESYSTEM_H
#define FILESYSTEM_H

#include <vmlinux.h>
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>

// Must match crate::agent::MAX_FILESYSTEMS.
#define MAX_FILESYSTEMS 64

// dev_t -> slot. Written by userspace from the mount table on each rescan;
// BPF only reads it. A HASH rather than an ARRAY (principle 5) because dev_t
// is a sparse 32-bit key (12 bits of major above 20 of minor) and cannot
// index a bounded array; the principle's objection to hash maps -- update
// contention on a hot path -- does not apply to a map BPF never updates.
struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __type(key, u32);
    __type(value, u32);
    __uint(max_entries, MAX_FILESYSTEMS);
} fs_slots SEC(".maps");

// The slot for a device: its assignment, or 0 ("other") for a device the
// table does not know yet, or a stale value past the bound.
static __always_inline u32 fs_slot(u32 dev) {
    u32* slot = bpf_map_lookup_elem(&fs_slots, &dev);

    if (slot && *slot < MAX_FILESYSTEMS) {
        return *slot;
    }

    return 0;
}

// The device of a superblock, or 0 (no mapping, slot 0) for a NULL pointer.
// tp_btf pointer arguments are trusted_ptr_or_null and need the check before
// BPF_CORE_READ's offset arithmetic.
static __always_inline u32 sb_dev(struct super_block* sb) {
    if (!sb) {
        return 0;
    }

    return BPF_CORE_READ(sb, s_dev);
}

static __always_inline u32 inode_dev(struct inode* inode) {
    if (!inode) {
        return 0;
    }

    return BPF_CORE_READ(inode, i_sb, s_dev);
}

// Index of `counter` in this CPU's bank for `slot`; the reader computes the
// same (counters.rs `filesystem_index`).
static __always_inline u32 fs_counter_idx(u32 slot, u32 counter, u32 width) {
    return (bpf_get_smp_processor_id() * MAX_FILESYSTEMS + slot) * width + counter;
}

#endif
