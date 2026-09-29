use metriken::*;

use crate::agent::timing::AcquisitionGroup;
use crate::agent::{MAX_CGROUPS, MAX_FILESYSTEMS};
use linkme::distributed_slice;

// Registered here (not in mod.rs) because this file is also `include!`d
// directly on non-Linux platforms (see `memory/mod.rs`'s
// `#[cfg(not(target_os = "linux"))] mod stats` fallback) to keep metric
// identity stable across platforms, while `mod.rs`'s BPF sampler code is
// Linux-only.
//
// Two groups (principle 18): every per-filesystem counter lives in one
// `counters` map read in one sweep; the per-cgroup counters are mmap-attached
// and reader-stamped, as `ext4_ops`'s are, and exist only with
// `cgroup_attribution = true`.
pub static COUNTERS_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("memory_pagecache"),
    "memory_pagecache_counters",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static COUNTERS_ACQ_REG: &'static AcquisitionGroup = &COUNTERS_ACQ;

pub static CGROUP_ACQ: AcquisitionGroup = AcquisitionGroup::new_reader_stamped(
    crate::agent::samplers::bpf_sampler_name("memory_pagecache"),
    "memory_pagecache_cgroup",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static CGROUP_ACQ_REG: &'static AcquisitionGroup = &CGROUP_ACQ;

/*
 * bpf prog stats
 */

#[metric(
    name = "rezolus_bpf_run_count",
    description = "The number of times Rezolus BPF programs have been run",
    metadata = { sampler = "memory_pagecache"}
)]
pub static BPF_RUN_COUNT: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "rezolus_bpf_run_time",
    description = "The amount of time Rezolus BPF programs have been executing",
    metadata = { unit = "nanoseconds", sampler = "memory_pagecache"}
)]
pub static BPF_RUN_TIME: LazyCounter = LazyCounter::new(Counter::default);

/*
 * per filesystem: one `CounterGroup` per metric, one entry per filesystem slot
 * (`bpf/filesystems.rs`): slot 0 is `mount="other"`, which here also holds
 * every filesystem the registry does not assign slots to and the block
 * devices' own page cache. Order here is documentation; the `counters` vec in
 * mod.rs must match the C_* indices in mod.bpf.c.
 */

#[metric(
    name = "pagecache_reads",
    description = "Buffered read calls into the page cache (filemap_read), hits and misses alike",
    metadata = { unit = "operations", acq_group = "memory_pagecache_counters" }
)]
pub static PAGECACHE_READS: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "pagecache_read_bytes",
    description = "Bytes those read calls asked for; pages filled during reads times the page size over this is the page-level miss ratio, readahead included",
    metadata = { unit = "bytes", acq_group = "memory_pagecache_counters" }
)]
pub static PAGECACHE_READ_BYTES: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "pagecache_pages_added",
    description = "Pages added to the page cache while the adding task was in a read syscall: read misses and the readahead they triggered",
    metadata = { unit = "pages", reason = "read", acq_group = "memory_pagecache_counters" }
)]
pub static PAGECACHE_PAGES_ADDED_READ: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "pagecache_pages_added",
    description = "Pages added to the page cache while the adding task was in a write syscall: buffered writes of pages not yet cached",
    metadata = { unit = "pages", reason = "write", acq_group = "memory_pagecache_counters" }
)]
pub static PAGECACHE_PAGES_ADDED_WRITE: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "pagecache_pages_added",
    description = "Pages added to the page cache from a page fault: mmap reads of pages not yet cached and their fault-around",
    metadata = { unit = "pages", reason = "fault", acq_group = "memory_pagecache_counters" }
)]
pub static PAGECACHE_PAGES_ADDED_FAULT: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "pagecache_pages_added",
    description = "Pages added to the page cache from any other context (kernel threads, other syscalls; every fill on kernels before 5.15, which cannot classify)",
    metadata = { unit = "pages", reason = "other", acq_group = "memory_pagecache_counters" }
)]
pub static PAGECACHE_PAGES_ADDED_OTHER: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "pagecache_pages_evicted",
    description = "Pages removed from the page cache: reclaim, truncation, invalidation",
    metadata = { unit = "pages", acq_group = "memory_pagecache_counters" }
)]
pub static PAGECACHE_PAGES_EVICTED: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "pagecache_faults",
    description = "mmap page faults served by the page cache (filemap_fault), whether the page was resident or had to be read",
    metadata = { unit = "operations", acq_group = "memory_pagecache_counters" }
)]
pub static PAGECACHE_FAULTS: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

/*
 * per cgroup: reads, bytes requested and pages filled, by the cgroup of the
 * task doing it; only with `cgroup_attribution = true`
 */

#[metric(
    name = "cgroup_pagecache_reads",
    description = "Buffered read calls into the page cache, by the reading task's cgroup",
    metadata = { unit = "operations", acq_group = "memory_pagecache_cgroup" }
)]
pub static CGROUP_PAGECACHE_READS: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_pagecache_read_bytes",
    description = "Bytes a cgroup's read calls asked of the page cache",
    metadata = { unit = "bytes", acq_group = "memory_pagecache_cgroup" }
)]
pub static CGROUP_PAGECACHE_READ_BYTES: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_pagecache_pages_added",
    description = "Pages a cgroup's tasks added to the page cache, every reason: its fills from the device",
    metadata = { unit = "pages", acq_group = "memory_pagecache_cgroup" }
)]
pub static CGROUP_PAGECACHE_PAGES_ADDED: CounterGroup = CounterGroup::new(MAX_CGROUPS);
