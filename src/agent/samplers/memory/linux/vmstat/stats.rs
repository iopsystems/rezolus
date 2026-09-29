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
