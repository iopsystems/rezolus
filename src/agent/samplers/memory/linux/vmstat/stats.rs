use metriken::*;

use crate::agent::timing::AcquisitionGroup;
use linkme::distributed_slice;

// See the identical comment on `memory_meminfo`'s `MEMINFO_ACQ` (this
// file is also `include!`d cross-platform via `memory/mod.rs`'s
// `#[cfg(not(target_os = "linux"))] mod stats` fallback).
//
/// Brackets the single `/proc/vmstat` read + parse (single writer: this
/// sampler's own `refresh()`).
pub static VMSTAT_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("memory_vmstat"),
    "memory_vmstat_read",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static VMSTAT_ACQ_REG: &'static AcquisitionGroup = &VMSTAT_ACQ;

#[metric(
    name = "memory_numa_hit",
    description = "The number of allocations that succeeded on the intended node",
    metadata = { acq_group = "memory_vmstat_read" }
)]
pub static MEMORY_NUMA_HIT: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "memory_numa_miss",
    description = "The number of allocations that did not succeed on the intended node",
    metadata = { acq_group = "memory_vmstat_read" }
)]
pub static MEMORY_NUMA_MISS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "memory_numa_foreign",
    description = "The number of allocations that were not intended for a node that were serviced by this node",
    metadata = { acq_group = "memory_vmstat_read" }
)]
pub static MEMORY_NUMA_FOREIGN: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "memory_numa_interleave",
    description = "The number of interleave policy allocations that succeeded on the intended node",
    metadata = { acq_group = "memory_vmstat_read" }
)]
pub static MEMORY_NUMA_INTERLEAVE: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "memory_numa_local",
    description = "The number of allocations that succeeded on the local node",
    metadata = { acq_group = "memory_vmstat_read" }
)]
pub static MEMORY_NUMA_LOCAL: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "memory_numa_other",
    description = "The number of allocations that on this node that were allocated by a process on another node",
    metadata = { acq_group = "memory_vmstat_read" }
)]
pub static MEMORY_NUMA_OTHER: LazyCounter = LazyCounter::new(Counter::default);

/*
 * page-cache writeback totals. Complete counts from the mm layer: every
 * page dirtied and every page written back by any path, including integrity
 * syncs, which the flusher's own `writeback_pages_written` tracepoint (in
 * `memory_writeback`) does not account for.
 */

#[metric(
    name = "memory_pages_dirtied",
    description = "Page-cache pages dirtied by writes (nr_dirtied). A page dirtied again before it is written back counts once",
    metadata = { unit = "pages", acq_group = "memory_vmstat_read" }
)]
pub static MEMORY_PAGES_DIRTIED: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "memory_pages_written",
    description = "Page-cache pages written back to storage by any path (nr_written): the flusher threads, direct reclaim, and integrity syncs alike",
    metadata = { unit = "pages", acq_group = "memory_vmstat_read" }
)]
pub static MEMORY_PAGES_WRITTEN: LazyCounter = LazyCounter::new(Counter::default);

/*
 * dirty limits: the page counts the writeback throttle compares Dirty
 * against, exported in bytes so they sit on the same axis as memory_dirty.
 */

#[metric(
    name = "memory_dirty_threshold",
    description = "The dirty-page limit at which writers are throttled (nr_dirty_threshold): vm.dirty_ratio or vm.dirty_bytes applied to the memory available for the page cache",
    metadata = { unit = "bytes", kind = "hard", acq_group = "memory_vmstat_read" }
)]
pub static MEMORY_DIRTY_THRESHOLD_HARD: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "memory_dirty_threshold",
    description = "The dirty-page level at which background writeback starts (nr_dirty_background_threshold): vm.dirty_background_ratio or vm.dirty_background_bytes applied to the memory available for the page cache",
    metadata = { unit = "bytes", kind = "background", acq_group = "memory_vmstat_read" }
)]
pub static MEMORY_DIRTY_THRESHOLD_BACKGROUND: LazyGauge = LazyGauge::new(Gauge::default);

/*
 * reclaim
 */

#[metric(
    name = "memory_reclaim_scanned",
    description = "Pages scanned by the kswapd background reclaimer (pgscan_kswapd)",
    metadata = { unit = "pages", kind = "kswapd", acq_group = "memory_vmstat_read" }
)]
pub static MEMORY_RECLAIM_SCANNED_KSWAPD: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "memory_reclaim_scanned",
    description = "Pages scanned by direct reclaim, on the allocating task's own thread (pgscan_direct). Non-zero means an allocation could not be satisfied without the caller reclaiming first",
    metadata = { unit = "pages", kind = "direct", acq_group = "memory_vmstat_read" }
)]
pub static MEMORY_RECLAIM_SCANNED_DIRECT: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "memory_reclaim_reclaimed",
    description = "Pages reclaimed by the kswapd background reclaimer (pgsteal_kswapd)",
    metadata = { unit = "pages", kind = "kswapd", acq_group = "memory_vmstat_read" }
)]
pub static MEMORY_RECLAIM_RECLAIMED_KSWAPD: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "memory_reclaim_reclaimed",
    description = "Pages reclaimed by direct reclaim on the allocating task's own thread (pgsteal_direct)",
    metadata = { unit = "pages", kind = "direct", acq_group = "memory_vmstat_read" }
)]
pub static MEMORY_RECLAIM_RECLAIMED_DIRECT: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "memory_allocation_stalls",
    description = "Page allocations that stalled to run direct reclaim, summed over the machine's zones (allocstall_*). Each is a synchronous delay on the allocating thread",
    metadata = { unit = "operations", acq_group = "memory_vmstat_read" }
)]
pub static MEMORY_ALLOCATION_STALLS: LazyCounter = LazyCounter::new(Counter::default);

/*
 * faults
 */

#[metric(
    name = "memory_page_faults",
    description = "Page faults of every kind (pgfault): first touches, copy-on-write, and the major faults counted separately",
    metadata = { unit = "faults", acq_group = "memory_vmstat_read" }
)]
pub static MEMORY_PAGE_FAULTS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "memory_major_page_faults",
    description = "Page faults that had to read the page from storage (pgmajfault): a file page not in the page cache, or a swapped-out page. Each is a synchronous I/O on the faulting thread",
    metadata = { unit = "faults", acq_group = "memory_vmstat_read" }
)]
pub static MEMORY_MAJOR_PAGE_FAULTS: LazyCounter = LazyCounter::new(Counter::default);

/*
 * swap
 */

#[metric(
    name = "memory_swap_in",
    description = "Pages read back from swap (pswpin)",
    metadata = { unit = "pages", acq_group = "memory_vmstat_read" }
)]
pub static MEMORY_SWAP_IN: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "memory_swap_out",
    description = "Pages written to swap (pswpout)",
    metadata = { unit = "pages", acq_group = "memory_vmstat_read" }
)]
pub static MEMORY_SWAP_OUT: LazyCounter = LazyCounter::new(Counter::default);

/*
 * working set. A refault is a page that was reclaimed and then needed again
 * soon enough that the kernel's shadow entry for it still existed: the
 * direct measure of a page cache or anonymous working set that does not fit.
 * The file/anon split exists from kernel 5.9; older kernels print one
 * unsplit line, which is not mapped, so these are absent there.
 */

#[metric(
    name = "memory_workingset_refaults",
    description = "File pages evicted and then faulted back in while their shadow entry survived (workingset_refault_file): page-cache working set that does not fit",
    metadata = { unit = "pages", kind = "file", acq_group = "memory_vmstat_read" }
)]
pub static MEMORY_WORKINGSET_REFAULTS_FILE: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "memory_workingset_refaults",
    description = "Anonymous pages swapped out and then faulted back in while their shadow entry survived (workingset_refault_anon)",
    metadata = { unit = "pages", kind = "anon", acq_group = "memory_vmstat_read" }
)]
pub static MEMORY_WORKINGSET_REFAULTS_ANON: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "memory_workingset_activations",
    description = "Refaulted file pages the kernel placed straight on the active list because they refaulted quickly (workingset_activate_file)",
    metadata = { unit = "pages", kind = "file", acq_group = "memory_vmstat_read" }
)]
pub static MEMORY_WORKINGSET_ACTIVATIONS_FILE: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "memory_workingset_activations",
    description = "Refaulted anonymous pages the kernel placed straight on the active list (workingset_activate_anon)",
    metadata = { unit = "pages", kind = "anon", acq_group = "memory_vmstat_read" }
)]
pub static MEMORY_WORKINGSET_ACTIVATIONS_ANON: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "memory_workingset_restores",
    description = "Refaulted file pages restored to the active list they were evicted from (workingset_restore_file)",
    metadata = { unit = "pages", kind = "file", acq_group = "memory_vmstat_read" }
)]
pub static MEMORY_WORKINGSET_RESTORES_FILE: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "memory_workingset_restores",
    description = "Refaulted anonymous pages restored to the active list they were evicted from (workingset_restore_anon)",
    metadata = { unit = "pages", kind = "anon", acq_group = "memory_vmstat_read" }
)]
pub static MEMORY_WORKINGSET_RESTORES_ANON: LazyCounter = LazyCounter::new(Counter::default);

/*
 * OOM
 */

#[metric(
    name = "memory_oom_kills",
    description = "Processes killed by the out-of-memory killer (oom_kill)",
    metadata = { unit = "operations", acq_group = "memory_vmstat_read" }
)]
pub static MEMORY_OOM_KILLS: LazyCounter = LazyCounter::new(Counter::default);

/*
 * transparent huge pages and compaction
 */

#[metric(
    name = "memory_thp_faults",
    description = "Page faults served with a transparent huge page (thp_fault_alloc)",
    metadata = { unit = "faults", outcome = "allocated", acq_group = "memory_vmstat_read" }
)]
pub static MEMORY_THP_FAULTS_ALLOCATED: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "memory_thp_faults",
    description = "Page faults that wanted a transparent huge page and fell back to small pages because none could be allocated (thp_fault_fallback): fragmentation, seen from the allocator",
    metadata = { unit = "faults", outcome = "fallback", acq_group = "memory_vmstat_read" }
)]
pub static MEMORY_THP_FAULTS_FALLBACK: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "memory_thp_collapses",
    description = "Small-page ranges khugepaged collapsed into a transparent huge page (thp_collapse_alloc)",
    metadata = { unit = "operations", acq_group = "memory_vmstat_read" }
)]
pub static MEMORY_THP_COLLAPSES: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "memory_thp_splits",
    description = "Transparent huge pages split back into small pages (thp_split_page), each undoing a TLB-reach win",
    metadata = { unit = "operations", acq_group = "memory_vmstat_read" }
)]
pub static MEMORY_THP_SPLITS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "memory_compaction_stalls",
    description = "Allocations that stalled to run memory compaction on the allocating thread (compact_stall), usually for a huge page",
    metadata = { unit = "operations", acq_group = "memory_vmstat_read" }
)]
pub static MEMORY_COMPACTION_STALLS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "memory_compactions",
    description = "Direct compaction runs that produced the requested free block (compact_success)",
    metadata = { unit = "operations", outcome = "success", acq_group = "memory_vmstat_read" }
)]
pub static MEMORY_COMPACTIONS_SUCCESS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "memory_compactions",
    description = "Direct compaction runs that did not produce the requested free block (compact_fail)",
    metadata = { unit = "operations", outcome = "fail", acq_group = "memory_vmstat_read" }
)]
pub static MEMORY_COMPACTIONS_FAIL: LazyCounter = LazyCounter::new(Counter::default);

/*
 * NUMA balancing: the automatic balancer's own activity, distinct from the
 * numa_hit/numa_miss allocation placement counters above.
 */

#[metric(
    name = "memory_numa_balancing_pte_updates",
    description = "Page-table entries the NUMA balancer marked to sample access locality (numa_pte_updates)",
    metadata = { unit = "operations", acq_group = "memory_vmstat_read" }
)]
pub static MEMORY_NUMA_BALANCING_PTE_UPDATES: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "memory_numa_balancing_hint_faults",
    description = "NUMA hinting faults taken on those marked entries (numa_hint_faults), each a minor fault on the accessing thread",
    metadata = { unit = "faults", acq_group = "memory_vmstat_read" }
)]
pub static MEMORY_NUMA_BALANCING_HINT_FAULTS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "memory_numa_balancing_pages_migrated",
    description = "Pages the NUMA balancer migrated to the node that accesses them (numa_pages_migrated)",
    metadata = { unit = "pages", acq_group = "memory_vmstat_read" }
)]
pub static MEMORY_NUMA_BALANCING_PAGES_MIGRATED: LazyCounter = LazyCounter::new(Counter::default);
