use crate::common::HISTOGRAM_GROUPING_POWER;
use metriken::*;

use crate::agent::timing::AcquisitionGroup;
use crate::agent::MAX_FILESYSTEMS;
use linkme::distributed_slice;

// this is hard-coded still and must match the BPF histograms which are fixed to
// use 2^64-1 as the max value
static HISTOGRAM_MAX: u8 = 64;

// Registered here (not in mod.rs) because this file is also `include!`d
// directly on non-Linux platforms (see `ext4/mod.rs`'s
// `#[cfg(not(target_os = "linux"))] mod stats` fallback) to keep metric
// identity stable across platforms, while `mod.rs`'s BPF sampler code is
// Linux-only.
//
// Two groups (principle 18): every counter lives in one `counters` map read
// in one sweep; the allocation-size histogram is its own family.
pub static COUNTERS_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("ext4_alloc"),
    "ext4_alloc_counters",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static COUNTERS_ACQ_REG: &'static AcquisitionGroup = &COUNTERS_ACQ;

pub static ALLOCATION_SIZES_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("ext4_alloc"),
    "ext4_alloc_allocation_sizes",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static ALLOCATION_SIZES_ACQ_REG: &'static AcquisitionGroup = &ALLOCATION_SIZES_ACQ;

/*
 * bpf prog stats
 */

#[metric(
    name = "rezolus_bpf_run_count",
    description = "The number of times Rezolus BPF programs have been run",
    metadata = { sampler = "ext4_alloc"}
)]
pub static BPF_RUN_COUNT: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "rezolus_bpf_run_time",
    description = "The amount of time Rezolus BPF programs have been executing",
    metadata = { unit = "nanoseconds", sampler = "ext4_alloc"}
)]
pub static BPF_RUN_TIME: LazyCounter = LazyCounter::new(Counter::default);

/*
 * Every counter below is one `CounterGroup` per metric, one entry per
 * filesystem slot (`bpf/filesystems.rs`): slot 0 is `mount="other"`, the rest
 * carry the mount's labels. Totals are `sum(...)` over slots.
 *
 * block allocator
 */

#[metric(
    name = "ext4_allocation_size",
    description = "Distribution of the length of each extent the block allocator returned, in filesystem blocks. Allocations shorter than the request are a file being split across extents",
    metadata = { unit = "blocks", acq_group = "ext4_alloc_allocation_sizes" }
)]
pub static EXT4_ALLOCATION_SIZE: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, HISTOGRAM_MAX);

#[metric(
    name = "ext4_allocations",
    description = "Extent allocations by the ext4 block allocator (ext4_mballoc_alloc). More than one per file created is a file split across extents",
    metadata = { unit = "operations", acq_group = "ext4_alloc_counters" }
)]
pub static EXT4_ALLOCATIONS: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "ext4_allocation_blocks",
    description = "Blocks the allocator was asked for, summed over allocations",
    metadata = { unit = "blocks", kind = "requested", acq_group = "ext4_alloc_counters" }
)]
pub static EXT4_ALLOCATION_BLOCKS_REQUESTED: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "ext4_allocation_blocks",
    description = "Blocks the allocator returned, summed over allocations. Falling short of requested means free space is too fragmented to satisfy requests in one extent",
    metadata = { unit = "blocks", kind = "allocated", acq_group = "ext4_alloc_counters" }
)]
pub static EXT4_ALLOCATION_BLOCKS_ALLOCATED: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "ext4_allocation_groups_scanned",
    description = "Block groups the allocator scanned, summed over allocations. Divided by allocations it is the allocator's effort per request, and it rises as free space fragments",
    metadata = { unit = "operations", acq_group = "ext4_alloc_counters" }
)]
pub static EXT4_ALLOCATION_GROUPS_SCANNED: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "ext4_allocations_by_criterion",
    description = "Allocations satisfied at criterion 0, the allocator's first and cheapest pass: a power-of-two aligned free extent of the goal length",
    metadata = { unit = "operations", criterion = "0", acq_group = "ext4_alloc_counters" }
)]
pub static EXT4_ALLOCATIONS_CR0: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "ext4_allocations_by_criterion",
    description = "Allocations satisfied at criterion 1: a free extent of the goal length found through the groups' free-extent lists. Where a healthy filesystem's allocations land",
    metadata = { unit = "operations", criterion = "1", acq_group = "ext4_alloc_counters" }
)]
pub static EXT4_ALLOCATIONS_CR1: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "ext4_allocations_by_criterion",
    description = "Allocations satisfied at criterion 2. On kernels from 6.5 this is the best-available-length pass, which trims the request to fit; before 6.5 it is the slow scan of every group",
    metadata = { unit = "operations", criterion = "2", acq_group = "ext4_alloc_counters" }
)]
pub static EXT4_ALLOCATIONS_CR2: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "ext4_allocations_by_criterion",
    description = "Allocations satisfied at criterion 3. On kernels from 6.5 this is the slow scan of every group's bitmaps; before 6.5 it is the last resort that takes any free block. A rising share at 2 or above is free space too fragmented for the fast paths",
    metadata = { unit = "operations", criterion = "3", acq_group = "ext4_alloc_counters" }
)]
pub static EXT4_ALLOCATIONS_CR3: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "ext4_allocations_by_criterion",
    description = "Allocations satisfied at criterion 4 or higher: on kernels from 6.5, the last resort that takes any free block. Older kernels have four criteria and never report this",
    metadata = { unit = "operations", criterion = "4", acq_group = "ext4_alloc_counters" }
)]
pub static EXT4_ALLOCATIONS_CR4: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "ext4_freed_blocks",
    description = "Blocks returned to the free pool by truncates, unlinks and failed allocations (ext4_free_blocks)",
    metadata = { unit = "blocks", acq_group = "ext4_alloc_counters" }
)]
pub static EXT4_FREED_BLOCKS: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

/*
 * inodes
 */

#[metric(
    name = "ext4_inodes",
    description = "Inodes allocated: files, directories and other objects created",
    metadata = { unit = "inodes", op = "allocated", acq_group = "ext4_alloc_counters" }
)]
pub static EXT4_INODES_ALLOCATED: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "ext4_inodes",
    description = "Inodes freed: the last link to an object removed and the object gone",
    metadata = { unit = "inodes", op = "freed", acq_group = "ext4_alloc_counters" }
)]
pub static EXT4_INODES_FREED: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

/*
 * writeback, as ext4 sees it
 */

#[metric(
    name = "ext4_writepages",
    description = "Writeback passes ext4 ran over an inode's dirty pages (ext4_writepages_result)",
    metadata = { unit = "operations", acq_group = "ext4_alloc_counters" }
)]
pub static EXT4_WRITEPAGES: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "ext4_writepages_pages",
    description = "Pages ext4 writeback passes wrote",
    metadata = { unit = "pages", outcome = "written", acq_group = "ext4_alloc_counters" }
)]
pub static EXT4_WRITEPAGES_PAGES_WRITTEN: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "ext4_writepages_pages",
    description = "Pages ext4 writeback passes skipped, left dirty for a later pass. Rising means writeback is falling behind the dirtying rate",
    metadata = { unit = "pages", outcome = "skipped", acq_group = "ext4_alloc_counters" }
)]
pub static EXT4_WRITEPAGES_PAGES_SKIPPED: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "ext4_writepages_errors",
    description = "ext4 writeback passes that ended in an error",
    metadata = { unit = "operations", acq_group = "ext4_alloc_counters" }
)]
pub static EXT4_WRITEPAGES_ERRORS: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

/*
 * discard and preallocation
 */

#[metric(
    name = "ext4_trimmed_blocks",
    description = "Blocks discarded to the device by fstrim or online discard (ext4_trim_extent)",
    metadata = { unit = "blocks", acq_group = "ext4_alloc_counters" }
)]
pub static EXT4_TRIMMED_BLOCKS: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "ext4_preallocation_discards",
    description = "Times an inode's preallocated blocks were released back to the free pool (ext4_discard_preallocations), which happens on close, truncate and unlink",
    metadata = { unit = "operations", acq_group = "ext4_alloc_counters" }
)]
pub static EXT4_PREALLOCATION_DISCARDS: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "ext4_preallocation_discarded_blocks",
    description = "Preallocated blocks released back to the free pool, summed over discards",
    metadata = { unit = "blocks", acq_group = "ext4_alloc_counters" }
)]
pub static EXT4_PREALLOCATION_DISCARDED_BLOCKS: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

/*
 * metadata reads on the request path
 */

#[metric(
    name = "ext4_inode_loads",
    description = "Inode-table reads from the device because an inode was not cached (ext4_load_inode). Each is a synchronous read on the calling thread of up to inode_readahead_blks blocks (32 by default), so one read serves the neighbouring inodes too. On a filesystem with tens of millions of files this is the atime and stat cost, and vm.vfs_cache_pressure is the knob that moves it",
    metadata = { unit = "operations", acq_group = "ext4_alloc_counters" }
)]
pub static EXT4_INODE_LOADS: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "ext4_bitmap_loads",
    description = "Block-allocation bitmaps read from the device (ext4_read_block_bitmap_load), including the allocator's prefetches. The allocator's cold-metadata cost",
    metadata = { unit = "operations", kind = "block", acq_group = "ext4_alloc_counters" }
)]
pub static EXT4_BITMAP_LOADS_BLOCK: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "ext4_bitmap_loads",
    description = "Inode-allocation bitmaps read from the device (ext4_load_inode_bitmap): the cost of creating a file in a block group whose inode bitmap is cold",
    metadata = { unit = "operations", kind = "inode", acq_group = "ext4_alloc_counters" }
)]
pub static EXT4_BITMAP_LOADS_INODE: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);
