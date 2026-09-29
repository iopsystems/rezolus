use metriken::*;

use crate::agent::timing::AcquisitionGroup;
use crate::agent::MAX_FILESYSTEMS;
use linkme::distributed_slice;

// Registered here (not in mod.rs) because this file is also `include!`d
// directly on non-Linux platforms (see `xfs/mod.rs`'s
// `#[cfg(not(target_os = "linux"))] mod stats` fallback) to keep metric
// identity stable across platforms, while `mod.rs`'s sampler code is
// Linux-only.
//
/// Brackets one sweep over every XFS mount's `/sys/fs/xfs/<dev>/stats/stats`:
/// principle 18's device-sweep shape, one window for every counter family,
/// stamped after the last mount is published. The single writer is the
/// blocking sweep task.
pub static XFS_STATS_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("xfs_stats"),
    "xfs_stats_sweep",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static XFS_STATS_ACQ_REG: &'static AcquisitionGroup = &XFS_STATS_ACQ;

/*
 * Every counter is one `CounterGroup` per metric, one entry per filesystem
 * slot (`bpf/filesystems.rs`), labeled `mount`, `fstype`, `devnum` and
 * `block_device` as the `filesystem` and ext4 samplers label the same mount.
 * The values are the kernel's own `xfsstats` counters, copied from sysfs;
 * the field each comes from is named in its description.
 */

/*
 * log
 */

#[metric(
    name = "xfs_log_writes",
    description = "Log writes (in-core log buffers written to the journal device): xfsstats log/writes",
    metadata = { unit = "writes", acq_group = "xfs_stats_sweep" }
)]
pub static XFS_LOG_WRITES: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "xfs_log_blocks_written",
    description = "512-byte blocks written to the journal device: xfsstats log/blocks. Times 512 is the journal's share of device writes",
    metadata = { unit = "blocks", acq_group = "xfs_stats_sweep" }
)]
pub static XFS_LOG_BLOCKS_WRITTEN: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "xfs_log_iclog_stalls",
    description = "Times a log write had to wait for a free in-core log buffer (all `logbufs` in flight): xfsstats log/noiclogs. Rising means the log device is the bottleneck",
    metadata = { unit = "stalls", acq_group = "xfs_stats_sweep" }
)]
pub static XFS_LOG_ICLOG_STALLS: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "xfs_log_forces",
    description = "Log forces, the synchronous flush of the log an fsync or a synchronous transaction demands: xfsstats log/force",
    metadata = { unit = "forces", acq_group = "xfs_stats_sweep" }
)]
pub static XFS_LOG_FORCES: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "xfs_log_force_sleeps",
    description = "Log-force callers that found a force already in progress and slept for it: xfsstats log/force_sleep. Per force, how often fsyncs are queuing behind each other",
    metadata = { unit = "sleeps", acq_group = "xfs_stats_sweep" }
)]
pub static XFS_LOG_FORCE_SLEEPS: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

/*
 * log space (the first two fields of the push_ail line)
 */

#[metric(
    name = "xfs_log_space_requests",
    description = "Transactions that reserved log space: xfsstats push_ail/try_logspace",
    metadata = { unit = "requests", acq_group = "xfs_stats_sweep" }
)]
pub static XFS_LOG_SPACE_REQUESTS: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "xfs_log_space_sleeps",
    description = "Transactions that slept because the log had no space until the AIL pushed the tail forward: xfsstats push_ail/sleep_logspace. Any rate here is the journal being too small or too slow for the write rate; the XFS analogue of ext4_journal_checkpoint_forced_to_close",
    metadata = { unit = "sleeps", acq_group = "xfs_stats_sweep" }
)]
pub static XFS_LOG_SPACE_SLEEPS: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

/*
 * AIL (active item list) pushing: writing logged metadata to its final
 * location so the log tail can move
 */

#[metric(
    name = "xfs_ail_pushes",
    description = "AIL push attempts (the xfsaild pass over items whose log space is wanted back): xfsstats push_ail/pushes",
    metadata = { unit = "pushes", acq_group = "xfs_stats_sweep" }
)]
pub static XFS_AIL_PUSHES: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "xfs_ail_push_items",
    description = "Items the AIL pusher visited, by what happened: success (written out), pushbuf (a buffer queued for writeback), pinned (still pinned in the log, a log force is needed first), locked (held by someone else, retried later), flushing (already being written): xfsstats push_ail/success, pushbuf, pinned, locked, flushing",
    metadata = { unit = "items", outcome = "success", acq_group = "xfs_stats_sweep" }
)]
pub static XFS_AIL_PUSH_SUCCESS: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "xfs_ail_push_items",
    description = "AIL items whose buffer was queued for writeback: xfsstats push_ail/pushbuf",
    metadata = { unit = "items", outcome = "pushbuf", acq_group = "xfs_stats_sweep" }
)]
pub static XFS_AIL_PUSH_PUSHBUF: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "xfs_ail_push_items",
    description = "AIL items still pinned in the log when pushed, so a log force was needed before they could move: xfsstats push_ail/pinned",
    metadata = { unit = "items", outcome = "pinned", acq_group = "xfs_stats_sweep" }
)]
pub static XFS_AIL_PUSH_PINNED: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "xfs_ail_push_items",
    description = "AIL items locked by someone else when pushed, retried later: xfsstats push_ail/locked",
    metadata = { unit = "items", outcome = "locked", acq_group = "xfs_stats_sweep" }
)]
pub static XFS_AIL_PUSH_LOCKED: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "xfs_ail_push_items",
    description = "AIL items already being written when pushed: xfsstats push_ail/flushing",
    metadata = { unit = "items", outcome = "flushing", acq_group = "xfs_stats_sweep" }
)]
pub static XFS_AIL_PUSH_FLUSHING: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "xfs_ail_push_restarts",
    description = "AIL pushes that gave up and restarted after too many pinned or locked items: xfsstats push_ail/restarts",
    metadata = { unit = "restarts", acq_group = "xfs_stats_sweep" }
)]
pub static XFS_AIL_PUSH_RESTARTS: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "xfs_ail_flushes",
    description = "Times the AIL pusher forced the log because everything it found was pinned: xfsstats push_ail/flush",
    metadata = { unit = "flushes", acq_group = "xfs_stats_sweep" }
)]
pub static XFS_AIL_FLUSHES: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

/*
 * transactions
 */

#[metric(
    name = "xfs_transactions",
    description = "Transactions committed synchronously (the caller waited for the log): xfsstats trans/sync",
    metadata = { unit = "transactions", kind = "sync", acq_group = "xfs_stats_sweep" }
)]
pub static XFS_TRANSACTIONS_SYNC: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "xfs_transactions",
    description = "Transactions committed asynchronously, to be written with the next log buffer: xfsstats trans/async",
    metadata = { unit = "transactions", kind = "async", acq_group = "xfs_stats_sweep" }
)]
pub static XFS_TRANSACTIONS_ASYNC: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "xfs_transactions",
    description = "Transactions that were committed with nothing in them: xfsstats trans/empty",
    metadata = { unit = "transactions", kind = "empty", acq_group = "xfs_stats_sweep" }
)]
pub static XFS_TRANSACTIONS_EMPTY: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

/*
 * inode cache
 */

#[metric(
    name = "xfs_inode_cache_lookups",
    description = "Inode-cache lookups that found the inode resident: xfsstats ig/found",
    metadata = { unit = "lookups", outcome = "found", acq_group = "xfs_stats_sweep" }
)]
pub static XFS_INODE_CACHE_FOUND: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "xfs_inode_cache_lookups",
    description = "Inode-cache lookups that missed and read the inode from disk: xfsstats ig/missed. The XFS analogue of ext4_inode_loads: the synchronous metadata cost of a cold inode cache",
    metadata = { unit = "lookups", outcome = "missed", acq_group = "xfs_stats_sweep" }
)]
pub static XFS_INODE_CACHE_MISSED: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "xfs_inode_cache_lookups",
    description = "Inode-cache lookups that recycled an inode on its way to reclaim: xfsstats ig/frecycle",
    metadata = { unit = "lookups", outcome = "recycled", acq_group = "xfs_stats_sweep" }
)]
pub static XFS_INODE_CACHE_RECYCLED: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "xfs_inode_cache_lookups",
    description = "Inode-cache lookups that raced another insertion of the same inode: xfsstats ig/dup",
    metadata = { unit = "lookups", outcome = "duplicate", acq_group = "xfs_stats_sweep" }
)]
pub static XFS_INODE_CACHE_DUPLICATE: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "xfs_inode_reclaims",
    description = "Inodes reclaimed from the cache: xfsstats ig/reclaims. With lookups missed, the cache's turnover",
    metadata = { unit = "inodes", acq_group = "xfs_stats_sweep" }
)]
pub static XFS_INODE_RECLAIMS: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

/*
 * allocator
 */

#[metric(
    name = "xfs_extents",
    description = "Extents allocated: xfsstats extent_alloc/allocx. More than one per file written is a file split across extents",
    metadata = { unit = "extents", op = "allocated", acq_group = "xfs_stats_sweep" }
)]
pub static XFS_EXTENTS_ALLOCATED: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "xfs_extents",
    description = "Extents freed: xfsstats extent_alloc/freex",
    metadata = { unit = "extents", op = "freed", acq_group = "xfs_stats_sweep" }
)]
pub static XFS_EXTENTS_FREED: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "xfs_extent_blocks",
    description = "Filesystem blocks allocated: xfsstats extent_alloc/allocb. Over extents allocated it is the mean extent length",
    metadata = { unit = "blocks", op = "allocated", acq_group = "xfs_stats_sweep" }
)]
pub static XFS_EXTENT_BLOCKS_ALLOCATED: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "xfs_extent_blocks",
    description = "Filesystem blocks freed: xfsstats extent_alloc/freeb",
    metadata = { unit = "blocks", op = "freed", acq_group = "xfs_stats_sweep" }
)]
pub static XFS_EXTENT_BLOCKS_FREED: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

/*
 * directories
 */

#[metric(
    name = "xfs_directory_ops",
    description = "Directory lookups: xfsstats dir/lookup",
    metadata = { unit = "operations", op = "lookup", acq_group = "xfs_stats_sweep" }
)]
pub static XFS_DIRECTORY_LOOKUPS: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "xfs_directory_ops",
    description = "Directory entries created: xfsstats dir/create",
    metadata = { unit = "operations", op = "create", acq_group = "xfs_stats_sweep" }
)]
pub static XFS_DIRECTORY_CREATES: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "xfs_directory_ops",
    description = "Directory entries removed: xfsstats dir/remove",
    metadata = { unit = "operations", op = "remove", acq_group = "xfs_stats_sweep" }
)]
pub static XFS_DIRECTORY_REMOVES: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "xfs_directory_ops",
    description = "Directory reads (getdents): xfsstats dir/getdents",
    metadata = { unit = "operations", op = "getdents", acq_group = "xfs_stats_sweep" }
)]
pub static XFS_DIRECTORY_GETDENTS: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

/*
 * file I/O at the XFS layer
 */

#[metric(
    name = "xfs_file_calls",
    description = "Write calls into XFS: xfsstats rw/write_calls",
    metadata = { unit = "calls", op = "write", acq_group = "xfs_stats_sweep" }
)]
pub static XFS_FILE_WRITE_CALLS: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "xfs_file_calls",
    description = "Read calls into XFS: xfsstats rw/read_calls",
    metadata = { unit = "calls", op = "read", acq_group = "xfs_stats_sweep" }
)]
pub static XFS_FILE_READ_CALLS: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "xfs_file_bytes",
    description = "Bytes applications wrote into XFS: xfsstats xpc/write_bytes. The first term of write amplification, against xfs_log_blocks_written and the device's bytes",
    metadata = { unit = "bytes", op = "written", acq_group = "xfs_stats_sweep" }
)]
pub static XFS_FILE_BYTES_WRITTEN: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "xfs_file_bytes",
    description = "Bytes applications read from XFS: xfsstats xpc/read_bytes",
    metadata = { unit = "bytes", op = "read", acq_group = "xfs_stats_sweep" }
)]
pub static XFS_FILE_BYTES_READ: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

/*
 * metadata buffer cache
 */

#[metric(
    name = "xfs_buffer_lookups",
    description = "Metadata buffer lookups: xfsstats buf/get",
    metadata = { unit = "lookups", acq_group = "xfs_stats_sweep" }
)]
pub static XFS_BUFFER_LOOKUPS: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "xfs_buffer_creates",
    description = "Metadata buffers created because the lookup found none: xfsstats buf/create",
    metadata = { unit = "buffers", acq_group = "xfs_stats_sweep" }
)]
pub static XFS_BUFFER_CREATES: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "xfs_buffer_lock_waits",
    description = "Metadata buffer lookups that waited for the buffer's lock: xfsstats buf/get_locked_waited",
    metadata = { unit = "waits", acq_group = "xfs_stats_sweep" }
)]
pub static XFS_BUFFER_LOCK_WAITS: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "xfs_buffer_busy_locks",
    description = "Metadata buffer trylocks that found the buffer busy: xfsstats buf/busy_locked",
    metadata = { unit = "locks", acq_group = "xfs_stats_sweep" }
)]
pub static XFS_BUFFER_BUSY_LOCKS: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "xfs_buffer_misses",
    description = "Metadata buffer lookups that missed the cache: xfsstats buf/miss_locked",
    metadata = { unit = "misses", acq_group = "xfs_stats_sweep" }
)]
pub static XFS_BUFFER_MISSES: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "xfs_buffer_reads",
    description = "Metadata buffers read from the device: xfsstats buf/get_read. The synchronous metadata reads a cold buffer cache imposes, the XFS analogue of ext4_bitmap_loads",
    metadata = { unit = "reads", acq_group = "xfs_stats_sweep" }
)]
pub static XFS_BUFFER_READS: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);
