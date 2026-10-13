// SPDX-License-Identifier: GPL-2.0
// Copyright (c) 2026 The Rezolus Authors

// This BPF program instruments the ext4 journal (jbd2) and ext4's own sync
// and error tracepoints. Design and decisions:
// docs/journal/2026-09-28-ext4-sampler.md.
//
// Every tracepoint hook has a tp_btf twin and a raw_tp twin sharing one
// handler. mod.rs disables the unused twin PER HOOK on whether the kernel's
// BTF -- vmlinux or a module's -- carries the tracepoint's btf_trace_* type
// (kernel_btf_has_tracepoints), because ext4 and jbd2 are modules on some
// kernels and a tp_btf target that is not in BTF fails at load, for the whole
// skeleton. The three handle-start entry points are functions, not
// tracepoints, and have fentry and kprobe twins: mod.rs keeps fentry where it
// can attach to that function (kernel_can_fentry), else the kprobe.
//
// Every latency except lock-buffer stalls is timed with bpf_ktime_get_ns().
// jbd2's own phase and checkpoint times are differences of two jiffies reads,
// so a phase shorter than one tick (1-10 ms) reads as 0 or 1 tick; they are
// not used.

#include <vmlinux.h>
#include "../../../agent/bpf/helpers.h"
#include "../../../agent/bpf/filesystem.h"
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_core_read.h>
#include <bpf/bpf_tracing.h>

#define COUNTER_GROUP_WIDTH 16
#define HISTOGRAM_BUCKETS HISTOGRAM_BUCKETS_POW_3
#define HISTOGRAM_POWER 3
#define MAX_CPUS 1024

#define MAX_PID 4194304

#define NS_PER_MS 1000000ULL

// Per-journal state is kept in a fixed array indexed by a hash of the
// journal's device number, each entry carrying the device as its key (the
// hashed-slot pattern of docs/principles.md, principle 5). Two journals that
// hash to the same entry overwrite each other's stamps, and the device check
// then drops the sample; with 256 entries and a handful of journals that is
// rare. The fs_slot() mapping is not used here because slot 0 is shared by
// every journal the mount table does not know.
#define JOURNAL_SLOTS 256

// Checkpoint start stamps per journal entry, indexed by tid. Only the
// transaction at the head of the checkpoint list is stamped, and the head
// changes only when that transaction is dropped, so one is in flight per
// journal.
#define CHECKPOINT_RING 8

// jbd2's per-commit and per-checkpoint statistics, as passed to
// jbd2_run_stats and jbd2_checkpoint_stats. Declared here as CO-RE "flavors"
// -- libbpf strips the ___rz suffix when matching against the running kernel's
// BTF -- because the vendored x86_64 vmlinux.h was generated from a kernel
// with ext4 built as a module and carries neither struct, while the aarch64
// header carries both and a plain redeclaration would collide there. Fields
// are read with BPF_CORE_READ, so the offsets come from the running kernel,
// never from this declaration.
struct transaction_run_stats_s___rz {
    unsigned long rs_wait;
    unsigned long rs_request_delay;
    unsigned long rs_running;
    unsigned long rs_locked;
    unsigned long rs_flushing;
    unsigned long rs_logging;
    __u32 rs_handle_count;
    __u32 rs_blocks;
    __u32 rs_blocks_logged;
} __attribute__((preserve_access_index));

struct transaction_chp_stats_s___rz {
    unsigned long cs_chp_time;
    __u32 cs_forced_to_close;
    __u32 cs_written;
    __u32 cs_dropped;
} __attribute__((preserve_access_index));

// The transaction and journal fields the phase and checkpoint hooks read,
// declared as flavors for the same reason. t_start_time is ktime_get() when
// jbd2_get_transaction created the transaction (present since before 5.8),
// the clock bpf_ktime_get_ns() reads.
struct transaction_s___rz {
    unsigned int t_tid;
    s64 t_start_time;
    struct transaction_chp_stats_s___rz t_chp_stats;
} __attribute__((preserve_access_index));

struct journal_s___rz {
    struct block_device* j_fs_dev;
    struct transaction_s___rz* j_checkpoint_transactions;
} __attribute__((preserve_access_index));

// counters: one bank of COUNTER_GROUP_WIDTH per (CPU, filesystem slot); the
// slot comes from fs_slot() on the device each hook has in hand, 0 for a
// device the mount table does not know (bpf/filesystems.rs). The order MUST
// match the `counters` vec in mod.rs.
#define C_COMMITS 0
#define C_COMMIT_HANDLES 1
#define C_COMMIT_BLOCKS_DIRTIED 2
#define C_COMMIT_BLOCKS_LOGGED 3
#define C_CHECKPOINTS 4
#define C_CHECKPOINT_WRITTEN 5
#define C_CHECKPOINT_DROPPED 6
#define C_CHECKPOINT_FORCED_TO_CLOSE 7
#define C_SYNC_FSYNC 8
#define C_SYNC_FDATASYNC 9
#define C_SYNC_ERRORS 10
#define C_ERRORS 11
#define C_SHUTDOWNS 12
#define C_LOCK_BUFFER_STALLS 13

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, MAX_CPUS* MAX_FILESYSTEMS* COUNTER_GROUP_WIDTH);
} counters SEC(".maps");

// Per journal entry, the commit in progress: the journal's device, the tid
// and the time each phase began. A journal runs one commit at a time (from
// kjournald2, or from the unmounting thread once kjournald2 has stopped), so
// one entry per journal is enough.
struct commit_state {
    u64 locked;
    u64 flushing;
    u64 logging;
    u32 dev;
    u32 tid;
};

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __type(key, u32);
    __type(value, struct commit_state);
    __uint(max_entries, JOURNAL_SLOTS);
} commits SEC(".maps");

// Per journal entry, a ring of checkpoint start stamps indexed by tid.
struct checkpoint_start {
    u64 ns;
    u32 dev;
    u32 tid;
};

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __type(key, u32);
    __type(value, struct checkpoint_start);
    __uint(max_entries, JOURNAL_SLOTS * CHECKPOINT_RING);
} checkpoint_starts SEC(".maps");

// Per thread, at pid_slot(tid), when it entered a handle-start function.
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __uint(max_entries, MAX_PID);
    __type(key, u32);
    __type(value, u64);
} handle_starts SEC(".maps");

// Commit phases: four LIKE ENTITIES of one family (the `phase` label), read as
// one acquisition group on the userspace side (see stats.rs).
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS * HISTOGRAM_SLICES);
} commit_running SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS * HISTOGRAM_SLICES);
} commit_locked SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS * HISTOGRAM_SLICES);
} commit_flushing SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS * HISTOGRAM_SLICES);
} commit_logging SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS * HISTOGRAM_SLICES);
} checkpoint_latency SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS * HISTOGRAM_SLICES);
} handle_start_latency SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(map_flags, BPF_F_MMAPABLE);
    __type(key, u32);
    __type(value, u64);
    __uint(max_entries, HISTOGRAM_BUCKETS * HISTOGRAM_SLICES);
} lock_buffer_stall_latency SEC(".maps");

static __always_inline void counter_add(u32 slot, u32 counter, u64 value) {
    array_add(&counters, fs_counter_idx(slot, counter, COUNTER_GROUP_WIDTH), value);
}

static __always_inline void counter_incr(u32 slot, u32 counter) {
    counter_add(slot, counter, 1);
}

static __always_inline u32 journal_dev(struct journal_s___rz* journal) {
    return BPF_CORE_READ(journal, j_fs_dev, bd_dev);
}

static __always_inline u32 journal_entry(u32 dev) {
    return (dev * 2654435761U) >> 24;
}

// The commit-phase tracepoints fire in order, from the thread running the
// commit, each just before the jiffies read jbd2 takes for the same boundary
// (fs/jbd2/commit.c). Each records the phase that just ended and stamps the
// one that begins.
//
// A tp_btf pointer argument is trusted_ptr_or_null to the verifier, and
// BPF_CORE_READ's field-offset arithmetic on it is refused until it has been
// null-checked ("pointer arithmetic on trusted_ptr_or_null_ prohibited").
// jbd2 never passes NULL here; the checks are for the verifier.

// jbd2_commit_locking: the transaction stops running. Running is measured
// from t_start_time, which jbd2 set with ktime_get() when it created the
// transaction.
static int __always_inline handle_commit_locking(void* j, void* t) {
    struct journal_s___rz* journal = j;
    struct transaction_s___rz* txn = t;

    if (!journal || !txn) {
        return 0;
    }

    u64 now = bpf_ktime_get_ns();
    u32 dev = journal_dev(journal);
    u32 entry = journal_entry(dev);
    struct commit_state* st = bpf_map_lookup_elem(&commits, &entry);

    if (!st) {
        return 0;
    }

    st->dev = dev;
    st->tid = BPF_CORE_READ(txn, t_tid);
    st->locked = now;
    st->flushing = 0;
    st->logging = 0;

    s64 started = BPF_CORE_READ(txn, t_start_time);

    if (started > 0 && now > (u64)started) {
        histogram_incr(&commit_running, HISTOGRAM_POWER, now - (u64)started);
    }

    return 0;
}

// jbd2_commit_flushing: locked ends, flushing begins.
static int __always_inline handle_commit_flushing(void* j, void* t) {
    struct journal_s___rz* journal = j;
    struct transaction_s___rz* txn = t;

    if (!journal || !txn) {
        return 0;
    }

    u64 now = bpf_ktime_get_ns();
    u32 dev = journal_dev(journal);
    u32 entry = journal_entry(dev);
    struct commit_state* st = bpf_map_lookup_elem(&commits, &entry);

    if (!st || st->dev != dev || st->tid != BPF_CORE_READ(txn, t_tid) || !st->locked) {
        return 0;
    }

    if (now > st->locked) {
        histogram_incr(&commit_locked, HISTOGRAM_POWER, now - st->locked);
    }
    st->locked = 0;
    st->flushing = now;

    return 0;
}

// jbd2_commit_logging: flushing ends, logging begins.
static int __always_inline handle_commit_logging(void* j, void* t) {
    struct journal_s___rz* journal = j;
    struct transaction_s___rz* txn = t;

    if (!journal || !txn) {
        return 0;
    }

    u64 now = bpf_ktime_get_ns();
    u32 dev = journal_dev(journal);
    u32 entry = journal_entry(dev);
    struct commit_state* st = bpf_map_lookup_elem(&commits, &entry);

    if (!st || st->dev != dev || st->tid != BPF_CORE_READ(txn, t_tid) || !st->flushing) {
        return 0;
    }

    if (now > st->flushing) {
        histogram_incr(&commit_flushing, HISTOGRAM_POWER, now - st->flushing);
    }
    st->flushing = 0;
    st->logging = now;

    return 0;
}

// jbd2_run_stats fires once per commit, a few statements after jbd2's last
// jiffies read for the logging phase: it ends logging, and carries the
// per-commit counts.
static int __always_inline handle_run_stats(u32 dev, u32 tid, void* stats) {
    u32 slot = fs_slot(dev);
    struct transaction_run_stats_s___rz* s = stats;
    u64 now = bpf_ktime_get_ns();

    if (!s) {
        return 0;
    }

    counter_incr(slot, C_COMMITS);
    counter_add(slot, C_COMMIT_HANDLES, BPF_CORE_READ(s, rs_handle_count));
    counter_add(slot, C_COMMIT_BLOCKS_DIRTIED, BPF_CORE_READ(s, rs_blocks));
    counter_add(slot, C_COMMIT_BLOCKS_LOGGED, BPF_CORE_READ(s, rs_blocks_logged));

    u32 entry = journal_entry(dev);
    struct commit_state* st = bpf_map_lookup_elem(&commits, &entry);

    if (!st || st->dev != dev || st->tid != tid || !st->logging) {
        return 0;
    }

    if (now > st->logging) {
        histogram_incr(&commit_logging, HISTOGRAM_POWER, now - st->logging);
    }
    st->logging = 0;

    return 0;
}

// jbd2_checkpoint fires in jbd2_log_do_checkpoint just before it stamps the
// head transaction's cs_chp_time, which it does only while that is still 0
// (fs/jbd2/checkpoint.c). Reading the same field here picks the same
// transaction and the same first pass. The caller holds j_checkpoint_mutex,
// but the list head is read without j_list_lock, so the head can be dropped
// between this read and jbd2's (by a commit's checkpoint-list cleaning, the
// checkpoint shrinker, or buffer release). If it is dropped before this stamp
// is written, the stamp is never matched; if after, its
// jbd2_checkpoint_stats matches and records a short latency for a
// transaction jbd2 never checkpointed. Either way jbd2 then stamps the new
// head, which this hook did not, and that transaction is not measured.
static int __always_inline handle_checkpoint(void* j, int result) {
    struct journal_s___rz* journal = j;

    if (!journal || result <= 0) {
        return 0;
    }

    struct transaction_s___rz* txn = BPF_CORE_READ(journal, j_checkpoint_transactions);

    if (!txn || BPF_CORE_READ(txn, t_chp_stats.cs_chp_time) != 0) {
        return 0;
    }

    u32 dev = journal_dev(journal);
    u32 tid = BPF_CORE_READ(txn, t_tid);
    u32 idx = journal_entry(dev) * CHECKPOINT_RING + (tid % CHECKPOINT_RING);
    struct checkpoint_start* cs = bpf_map_lookup_elem(&checkpoint_starts, &idx);

    if (cs) {
        cs->dev = dev;
        cs->tid = tid;
        cs->ns = bpf_ktime_get_ns();
    }

    return 0;
}

// jbd2_checkpoint_stats fires when a checkpointed transaction is dropped. It
// also fires for transactions dropped without ever being checkpointed, whose
// cs_chp_time jbd2 reports as 0; those have no start stamp here and are left
// out of the histogram, though the counters include them.
static int __always_inline handle_checkpoint_stats(u32 dev, u32 tid, void* stats) {
    u32 slot = fs_slot(dev);
    struct transaction_chp_stats_s___rz* s = stats;
    u64 now = bpf_ktime_get_ns();

    if (!s) {
        return 0;
    }

    counter_incr(slot, C_CHECKPOINTS);
    counter_add(slot, C_CHECKPOINT_WRITTEN, BPF_CORE_READ(s, cs_written));
    counter_add(slot, C_CHECKPOINT_DROPPED, BPF_CORE_READ(s, cs_dropped));
    counter_add(slot, C_CHECKPOINT_FORCED_TO_CLOSE, BPF_CORE_READ(s, cs_forced_to_close));

    u32 idx = journal_entry(dev) * CHECKPOINT_RING + (tid % CHECKPOINT_RING);
    struct checkpoint_start* cs = bpf_map_lookup_elem(&checkpoint_starts, &idx);

    if (!cs || cs->dev != dev || cs->tid != tid || !cs->ns) {
        return 0;
    }

    if (now > cs->ns) {
        histogram_incr(&checkpoint_latency, HISTOGRAM_POWER, now - cs->ns);
    }
    cs->ns = 0;

    return 0;
}

// Handle start: jbd2__journal_start, jbd2_journal_start (which calls it from
// the same file, so the call may be inlined) and jbd2_journal_start_reserved
// stamp the thread on entry; jbd2_handle_start fires when the handle has been
// granted. Every successful call stamps before its tracepoint, so the
// tracepoint never reads another call's stamp. jbd2__journal_restart fires
// jbd2_handle_restart, which is not timed. A nested start (the thread
// already holds a handle) or a failed one stamps and fires no tracepoint; the
// next entry overwrites that stamp.
static int __always_inline handle_start_enter(void) {
    u32 slot = pid_slot(bpf_get_current_pid_tgid());
    u64 now = bpf_ktime_get_ns();

    bpf_map_update_elem(&handle_starts, &slot, &now, 0);

    return 0;
}

static int __always_inline handle_start_granted(void) {
    u32 slot = pid_slot(bpf_get_current_pid_tgid());
    u64* start = bpf_map_lookup_elem(&handle_starts, &slot);

    if (!start || !*start) {
        return 0;
    }

    u64 now = bpf_ktime_get_ns();

    if (now > *start) {
        histogram_incr(&handle_start_latency, HISTOGRAM_POWER, now - *start);
    }
    *start = 0;

    return 0;
}

// jbd2_lock_buffer_stall reports the stall in whole milliseconds, a scalar
// argument: no struct read, so this hook works even where the jbd2 structs are
// not in BTF.
static int __always_inline handle_lock_buffer_stall(u32 slot, unsigned long stall_ms) {
    counter_incr(slot, C_LOCK_BUFFER_STALLS);
    histogram_incr(&lock_buffer_stall_latency, HISTOGRAM_POWER, (u64)stall_ms * NS_PER_MS);

    return 0;
}

static int __always_inline handle_sync_file_enter(u32 slot, int datasync) {
    counter_incr(slot, datasync ? C_SYNC_FDATASYNC : C_SYNC_FSYNC);

    return 0;
}

static int __always_inline handle_sync_file_exit(u32 slot, int ret) {
    if (ret < 0) {
        counter_incr(slot, C_SYNC_ERRORS);
    }

    return 0;
}

static int __always_inline handle_error(u32 slot) {
    counter_incr(slot, C_ERRORS);

    return 0;
}

static int __always_inline handle_shutdown(u32 slot) {
    counter_incr(slot, C_SHUTDOWNS);

    return 0;
}

// tp_btf and raw_tp twins share the handlers above; the unused variant of
// each hook is disabled at load time by mod.rs. Argument lists are the
// tracepoints' TP_PROTO; the jbd2 journal, transaction and stats pointers are
// taken as void* because the struct types are CO-RE flavors declared above,
// not the kernel's names.

SEC("tp_btf/jbd2_commit_locking")
int BPF_PROG(jbd2_commit_locking_btf, void* journal, void* txn) {
    return handle_commit_locking(journal, txn);
}

SEC("raw_tp/jbd2_commit_locking")
int BPF_PROG(jbd2_commit_locking_raw, void* journal, void* txn) {
    return handle_commit_locking(journal, txn);
}

SEC("tp_btf/jbd2_commit_flushing")
int BPF_PROG(jbd2_commit_flushing_btf, void* journal, void* txn) {
    return handle_commit_flushing(journal, txn);
}

SEC("raw_tp/jbd2_commit_flushing")
int BPF_PROG(jbd2_commit_flushing_raw, void* journal, void* txn) {
    return handle_commit_flushing(journal, txn);
}

SEC("tp_btf/jbd2_commit_logging")
int BPF_PROG(jbd2_commit_logging_btf, void* journal, void* txn) {
    return handle_commit_logging(journal, txn);
}

SEC("raw_tp/jbd2_commit_logging")
int BPF_PROG(jbd2_commit_logging_raw, void* journal, void* txn) {
    return handle_commit_logging(journal, txn);
}

SEC("tp_btf/jbd2_run_stats")
int BPF_PROG(jbd2_run_stats_btf, dev_t dev, unsigned int tid, void* stats) {
    return handle_run_stats(dev, tid, stats);
}

SEC("raw_tp/jbd2_run_stats")
int BPF_PROG(jbd2_run_stats_raw, dev_t dev, unsigned int tid, void* stats) {
    return handle_run_stats(dev, tid, stats);
}

SEC("tp_btf/jbd2_checkpoint")
int BPF_PROG(jbd2_checkpoint_btf, void* journal, int result) {
    return handle_checkpoint(journal, result);
}

SEC("raw_tp/jbd2_checkpoint")
int BPF_PROG(jbd2_checkpoint_raw, void* journal, int result) {
    return handle_checkpoint(journal, result);
}

SEC("tp_btf/jbd2_checkpoint_stats")
int BPF_PROG(jbd2_checkpoint_stats_btf, dev_t dev, unsigned int tid, void* stats) {
    return handle_checkpoint_stats(dev, tid, stats);
}

SEC("raw_tp/jbd2_checkpoint_stats")
int BPF_PROG(jbd2_checkpoint_stats_raw, dev_t dev, unsigned int tid, void* stats) {
    return handle_checkpoint_stats(dev, tid, stats);
}

// No arguments are read: the handle-start latency is host-wide.
SEC("tp_btf/jbd2_handle_start")
int BPF_PROG(jbd2_handle_start_btf) {
    return handle_start_granted();
}

SEC("raw_tp/jbd2_handle_start")
int BPF_PROG(jbd2_handle_start_raw) {
    return handle_start_granted();
}

SEC("fentry/jbd2__journal_start")
int BPF_PROG(jbd2__journal_start_fentry) {
    return handle_start_enter();
}

SEC("kprobe/jbd2__journal_start")
int BPF_KPROBE(jbd2__journal_start_kprobe) {
    return handle_start_enter();
}

SEC("fentry/jbd2_journal_start")
int BPF_PROG(jbd2_journal_start_fentry) {
    return handle_start_enter();
}

SEC("kprobe/jbd2_journal_start")
int BPF_KPROBE(jbd2_journal_start_kprobe) {
    return handle_start_enter();
}

SEC("fentry/jbd2_journal_start_reserved")
int BPF_PROG(jbd2_journal_start_reserved_fentry) {
    return handle_start_enter();
}

SEC("kprobe/jbd2_journal_start_reserved")
int BPF_KPROBE(jbd2_journal_start_reserved_kprobe) {
    return handle_start_enter();
}

SEC("tp_btf/jbd2_lock_buffer_stall")
int BPF_PROG(jbd2_lock_buffer_stall_btf, dev_t dev, unsigned long stall_ms) {
    return handle_lock_buffer_stall(fs_slot(dev), stall_ms);
}

SEC("raw_tp/jbd2_lock_buffer_stall")
int BPF_PROG(jbd2_lock_buffer_stall_raw, dev_t dev, unsigned long stall_ms) {
    return handle_lock_buffer_stall(fs_slot(dev), stall_ms);
}

SEC("tp_btf/ext4_sync_file_enter")
int BPF_PROG(ext4_sync_file_enter_btf, struct file* file, int datasync) {
    return handle_sync_file_enter(fs_slot(file_dev(file)), datasync);
}

SEC("raw_tp/ext4_sync_file_enter")
int BPF_PROG(ext4_sync_file_enter_raw, struct file* file, int datasync) {
    return handle_sync_file_enter(fs_slot(file_dev(file)), datasync);
}

SEC("tp_btf/ext4_sync_file_exit")
int BPF_PROG(ext4_sync_file_exit_btf, struct inode* inode, int ret) {
    return handle_sync_file_exit(fs_slot(inode_dev(inode)), ret);
}

SEC("raw_tp/ext4_sync_file_exit")
int BPF_PROG(ext4_sync_file_exit_raw, struct inode* inode, int ret) {
    return handle_sync_file_exit(fs_slot(inode_dev(inode)), ret);
}

SEC("tp_btf/ext4_error")
int BPF_PROG(ext4_error_btf, struct super_block* sb, const char* function, unsigned int line) {
    return handle_error(fs_slot(sb_dev(sb)));
}

SEC("raw_tp/ext4_error")
int BPF_PROG(ext4_error_raw, struct super_block* sb, const char* function, unsigned int line) {
    return handle_error(fs_slot(sb_dev(sb)));
}

SEC("tp_btf/ext4_shutdown")
int BPF_PROG(ext4_shutdown_btf, struct super_block* sb, unsigned long flags) {
    return handle_shutdown(fs_slot(sb_dev(sb)));
}

SEC("raw_tp/ext4_shutdown")
int BPF_PROG(ext4_shutdown_raw, struct super_block* sb, unsigned long flags) {
    return handle_shutdown(fs_slot(sb_dev(sb)));
}

char LICENSE[] SEC("license") = "GPL";
