// SPDX-License-Identifier: GPL-2.0
// Copyright (c) 2026 The Rezolus Authors

// The page cache: reads into it, pages filled and pages evicted, per
// filesystem (and per cgroup when asked). Design and the probe that shaped
// it: docs/journal/2026-09-29-pagecache-hit-ratio.md.
//
// The read side is ONE fentry on filemap_read, the buffered read path's
// entry: calls and bytes requested. No fexit and no task storage, so a read
// pays one crossing. Fills are the mm_filemap_add_to_page_cache tracepoint,
// once per folio added, at the rate pages come in from the device, and each
// is classified by what the adding task was doing -- a read syscall, a write
// syscall, a page fault, or something else -- from the task's saved syscall
// number. Evictions are the delete tracepoint. mmap faults into the cache are
// fentry on filemap_fault. Pages filled during reads over pages requested is
// the page-level miss ratio, readahead included.

#include <vmlinux.h>
#include "../../../agent/bpf/cgroup.h"
#include "../../../agent/bpf/helpers.h"
#include "../../../agent/bpf/filesystem.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_tracing.h>

#define COUNTER_GROUP_WIDTH 8
#define MAX_CPUS 1024
#define MAX_SYSCALL_ID 1024
#define PF_KTHREAD 0x00200000
// What the syscall_lut says about a syscall number (see mod.rs).
#define LUT_READ 1
#define LUT_WRITE 2

// Why a page was added: the adding task's context. The order of the `reason`
// label's counters.
#define REASON_READ 0
#define REASON_WRITE 1
#define REASON_FAULT 2
#define REASON_OTHER 3
#define REASON_COUNT 4

// per-filesystem counters: one bank of COUNTER_GROUP_WIDTH per (CPU, slot).
// The order MUST match the `counters` vec in mod.rs.
#define C_READS 0
#define C_READ_BYTES 1
#define C_ADDED 2  // + reason
#define C_EVICTED 6
#define C_FAULTS 7

// Per-cgroup attribution is the config option `cgroup_attribution`, off by
// default (see ext4_ops for the measurement behind the default). Written into
// read-only data before load, so with it off the verifier removes the path.
const volatile __u8 cgroup_attribution = 0;

// syscall number -> LUT_READ / LUT_WRITE / 0, written by userspace from the
// running architecture's syscall table. Mmapable because that is how
// BpfBuilder::map writes it; the builder writes it after the programs attach,
// so fills in the first instant classify as `other`.
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_SYSCALL_ID);
} syscall_lut SEC(".maps");

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

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CPUS* MAX_FILESYSTEMS* COUNTER_GROUP_WIDTH);
} counters SEC(".maps");

/*
 * per-cgroup counters: read calls, bytes requested, pages filled
 */

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CGROUPS);
} cgroup_reads SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CGROUPS);
} cgroup_read_bytes SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CGROUPS);
} cgroup_pages_added SEC(".maps");

static __always_inline void counter_add(u32 slot, u32 counter, u64 value) {
    array_add(&counters, fs_counter_idx(slot, counter, COUNTER_GROUP_WIDTH), value);
}

// The device under a kiocb's file, or 0 (slot 0) for a NULL pointer.
static __always_inline u32 kiocb_dev(struct kiocb* iocb) {
    if (!iocb) {
        return 0;
    }

    return BPF_CORE_READ(iocb, ki_filp, f_inode, i_sb, s_dev);
}

// The filesystem a page-cache folio belongs to: its mapping's inode's
// superblock. Before 5.16 the tracepoints hand over a struct page, whose
// mapping sits where the folio's does; CO-RE picks the shape the running
// kernel has, and the other branch is dead code at load. A block device's
// page cache (inode tables, directory blocks read through the buffer cache)
// has the bdev pseudo-filesystem's device and lands in slot 0, "other".
static __always_inline u32 folio_dev(struct folio* folio) {
    if (!folio) {
        return 0;
    }

    if (bpf_core_type_exists(struct folio)) {
        return BPF_CORE_READ(folio, mapping, host, i_sb, s_dev);
    }

    struct page* page = (struct page*)folio;
    return BPF_CORE_READ(page, mapping, host, i_sb, s_dev);
}

// Where a large folio's order lives has moved three times; these flavors
// name the older shapes (libbpf strips the ___suffix when relocating):
// 6.1-6.5 keep it in `folio->_folio_order`, 5.16-6.0 in the second page's
// `compound_order`. From 6.6 it is the low byte of `folio->_flags_1`, which
// 6.1-6.5 also have (holding second-page flags), so `_folio_order` is tested
// first.
struct folio___order_field {
    unsigned char _folio_order;
} __attribute__((preserve_access_index));

struct page___compound_order {
    unsigned char compound_order;
} __attribute__((preserve_access_index));

// Pages in a folio: 1 << order for a large folio (PG_head set), 1 otherwise.
// Before folios (5.16) every page-cache page is one page. An order past any
// the page cache allocates is treated as a misread and counted as one.
static __always_inline u64 folio_pages(struct folio* folio) {
    if (!bpf_core_type_exists(struct folio)) {
        return 1;
    }

    unsigned long flags = BPF_CORE_READ(folio, flags);
    if (!(flags & (1UL << bpf_core_enum_value(enum pageflags, PG_head)))) {
        return 1;
    }

    struct folio___order_field* f61 = (void*)folio;
    struct page___compound_order* tail =
        (void*)((unsigned long)folio + bpf_core_type_size(struct page));
    u32 order = 0;

    if (bpf_core_field_exists(f61->_folio_order)) {
        order = BPF_CORE_READ(f61, _folio_order);
    } else if (bpf_core_field_exists(folio->_flags_1)) {
        order = BPF_CORE_READ(folio, _flags_1) & 0xff;
    } else if (bpf_core_field_exists(tail->compound_order)) {
        order = BPF_CORE_READ(tail, compound_order);
    }

    if (order > 20) {
        return 1;
    }

    return 1ULL << order;
}

// What the current task was doing when it added a page: the syscall it is
// in, read from its saved registers, or a page fault (the entry code stores
// -1 as the syscall number for exceptions on both x86 and arm64). Kernel
// threads have no syscall of their own. Only the `_classified` program calls
// this: bpf_task_pt_regs is a 5.15 helper, and mod.rs loads the `_plain`
// twin, which counts every fill as `other`, where the kernel lacks it.
static __always_inline u32 fill_reason(void) {
    struct task_struct* task = bpf_get_current_task_btf();

    if (BPF_CORE_READ(task, flags) & PF_KTHREAD) {
        return REASON_OTHER;
    }

    struct pt_regs* regs = (struct pt_regs*)bpf_task_pt_regs(task);
    if (!regs) {
        return REASON_OTHER;
    }

#if defined(__TARGET_ARCH_x86)
    long nr = (long)BPF_CORE_READ(regs, orig_ax);
#elif defined(__TARGET_ARCH_arm64)
    long nr = (long)BPF_CORE_READ(regs, syscallno);
#else
    long nr = -1;
#endif

    if (nr < 0) {
        return REASON_FAULT;
    }
    if (nr >= MAX_SYSCALL_ID) {
        return REASON_OTHER;
    }

    u32 key = nr;
    u64* class = bpf_map_lookup_elem(&syscall_lut, &key);
    if (!class) {
        return REASON_OTHER;
    }
    if (*class == LUT_READ) {
        return REASON_READ;
    }
    if (*class == LUT_WRITE) {
        return REASON_WRITE;
    }
    return REASON_OTHER;
}

// The current task's cgroup slot, with the counters zeroed when it is new;
// MAX_CGROUPS when attribution is off or the task has no group.
static __always_inline u32 cgroup_slot(void) {
    if (!cgroup_attribution) {
        return MAX_CGROUPS;
    }

    u32 cgroup_id = 0;
    u64 serial_nr = 0;
    struct task_group* tg =
        task_group_of(bpf_get_current_task_btf(), true, &cgroup_id, &serial_nr);
    if (!tg || cgroup_id >= MAX_CGROUPS) {
        return MAX_CGROUPS;
    }

    if (handle_new_cgroup_read(&tg->css, cgroup_id, serial_nr, &cgroup_serial_numbers,
                               &cgroup_info) == 0) {
        // New cgroup detected, zero all counters
        u64 zero = 0;
        bpf_map_update_elem(&cgroup_reads, &cgroup_id, &zero, BPF_ANY);
        bpf_map_update_elem(&cgroup_read_bytes, &cgroup_id, &zero, BPF_ANY);
        bpf_map_update_elem(&cgroup_pages_added, &cgroup_id, &zero, BPF_ANY);
    }

    return cgroup_id;
}

// One buffered read call: the filesystem it is against and the bytes it can
// return, which is what it asks for clamped at end of file. Unclamped, a
// `cat` of a cold 4 KiB file would count a 128 KiB request against one page
// filled and understate the miss ratio 32-fold. filemap_read(iocb, iter,
// already_read) is the entry on 5.12+; generic_file_buffered_read had the
// same arguments before it.
static __always_inline int read_call(struct kiocb* iocb, struct iov_iter* iter) {
    u32 slot = fs_slot(kiocb_dev(iocb));
    u64 bytes = BPF_CORE_READ(iter, count);

    if (iocb) {
        long long pos = BPF_CORE_READ(iocb, ki_pos);
        long long size = BPF_CORE_READ(iocb, ki_filp, f_inode, i_size);
        if (pos >= size) {
            bytes = 0;
        } else if (bytes > (u64)(size - pos)) {
            bytes = size - pos;
        }
    }

    counter_add(slot, C_READS, 1);
    counter_add(slot, C_READ_BYTES, bytes);

    u32 cg = cgroup_slot();
    if (cg < MAX_CGROUPS) {
        array_incr(&cgroup_reads, cg);
        array_add(&cgroup_read_bytes, cg, bytes);
    }

    return 0;
}

SEC("fentry/filemap_read")
int BPF_PROG(filemap_read_fentry, struct kiocb* iocb, struct iov_iter* iter, ssize_t already_read) {
    return read_call(iocb, iter);
}

SEC("fentry/generic_file_buffered_read")
int BPF_PROG(generic_file_buffered_read_fentry, struct kiocb* iocb, struct iov_iter* iter,
             ssize_t written) {
    return read_call(iocb, iter);
}

// One folio added to a file's page cache, by reason.
static __always_inline int fill(struct folio* folio, u32 reason) {
    u32 slot = fs_slot(folio_dev(folio));
    u64 pages = folio_pages(folio);

    if (reason >= REASON_COUNT) {
        reason = REASON_OTHER;
    }
    counter_add(slot, C_ADDED + reason, pages);

    u32 cg = cgroup_slot();
    if (cg < MAX_CGROUPS) {
        array_add(&cgroup_pages_added, cg, pages);
    }

    return 0;
}

SEC("tp_btf/mm_filemap_add_to_page_cache")
int BPF_PROG(filemap_add_classified, struct folio* folio) {
    return fill(folio, fill_reason());
}

SEC("tp_btf/mm_filemap_add_to_page_cache")
int BPF_PROG(filemap_add_plain, struct folio* folio) {
    return fill(folio, REASON_OTHER);
}

SEC("tp_btf/mm_filemap_delete_from_page_cache")
int BPF_PROG(filemap_delete, struct folio* folio) {
    counter_add(fs_slot(folio_dev(folio)), C_EVICTED, folio_pages(folio));
    return 0;
}

// An mmap fault served from (or filling) the page cache.
SEC("fentry/filemap_fault")
int BPF_PROG(filemap_fault_fentry, struct vm_fault* vmf) {
    u32 dev = 0;

    if (vmf) {
        dev = BPF_CORE_READ(vmf, vma, vm_file, f_inode, i_sb, s_dev);
    }
    counter_add(fs_slot(dev), C_FAULTS, 1);
    return 0;
}

char LICENSE[] SEC("license") = "GPL";
