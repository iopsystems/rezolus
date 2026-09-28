use metriken::*;

use crate::agent::timing::AcquisitionGroup;
use linkme::distributed_slice;

// Registered here (not in mod.rs) because this file is also `include!`d
// directly on non-Linux platforms (see `memory/mod.rs`'s
// `#[cfg(not(target_os = "linux"))] mod stats` fallback) to keep metric
// identity stable across platforms, while `mod.rs`'s sampler code is
// Linux-only. A metric declaring `acq_group = "memory_meminfo_read"` must
// find its group registered on every platform that compiles this file, not
// just the one that actually drives it — see
// `crate::agent::samplers::bpf_sampler_name`'s doc comment (the mechanism
// applies to any sampler whose `stats.rs` is compiled cross-platform for
// metric-identity continuity, not just BPF ones).
//
/// Brackets the single `/proc/meminfo` read + parse (single writer: this
/// sampler's own `refresh()`).
pub static MEMINFO_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("memory_meminfo"),
    "memory_meminfo_read",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static MEMINFO_ACQ_REG: &'static AcquisitionGroup = &MEMINFO_ACQ;

/*
 * capacity
 */

#[metric(
    name = "memory_total",
    description = "The total amount of system memory",
    metadata = { unit = "bytes", acq_group = "memory_meminfo_read" }
)]
pub static MEMORY_TOTAL: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "memory_free",
    description = "The amount of system memory that is currently free",
    metadata = { unit = "bytes", acq_group = "memory_meminfo_read" }
)]
pub static MEMORY_FREE: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "memory_available",
    description = "The amount of system memory that is available for allocation",
    metadata = { unit = "bytes", acq_group = "memory_meminfo_read" }
)]
pub static MEMORY_AVAILABLE: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "memory_buffers",
    description = "The amount of system memory used for buffers",
    metadata = { unit = "bytes", acq_group = "memory_meminfo_read" }
)]
pub static MEMORY_BUFFERS: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "memory_cached",
    description = "The amount of system memory used by the page cache",
    metadata = { unit = "bytes", acq_group = "memory_meminfo_read" }
)]
pub static MEMORY_CACHED: LazyGauge = LazyGauge::new(Gauge::default);

/*
 * writeback
 */

#[metric(
    name = "memory_dirty",
    description = "Page-cache memory dirtied and not yet written back (Dirty). Grows between writeback runs and is what the writeback throttle acts on",
    metadata = { unit = "bytes", acq_group = "memory_meminfo_read" }
)]
pub static MEMORY_DIRTY: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "memory_writeback",
    description = "Page-cache memory currently being written back to storage (Writeback)",
    metadata = { unit = "bytes", acq_group = "memory_meminfo_read" }
)]
pub static MEMORY_WRITEBACK: LazyGauge = LazyGauge::new(Gauge::default);

/*
 * page cache and anonymous memory by LRU list
 */

#[metric(
    name = "memory_active",
    description = "File-backed memory on the active LRU list: recently used page cache the kernel will reclaim last (Active(file))",
    metadata = { unit = "bytes", kind = "file", acq_group = "memory_meminfo_read" }
)]
pub static MEMORY_ACTIVE_FILE: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "memory_active",
    description = "Anonymous memory on the active LRU list (Active(anon))",
    metadata = { unit = "bytes", kind = "anon", acq_group = "memory_meminfo_read" }
)]
pub static MEMORY_ACTIVE_ANON: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "memory_inactive",
    description = "File-backed memory on the inactive LRU list: page cache the kernel will reclaim first (Inactive(file))",
    metadata = { unit = "bytes", kind = "file", acq_group = "memory_meminfo_read" }
)]
pub static MEMORY_INACTIVE_FILE: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "memory_inactive",
    description = "Anonymous memory on the inactive LRU list, the first candidate for swap (Inactive(anon))",
    metadata = { unit = "bytes", kind = "anon", acq_group = "memory_meminfo_read" }
)]
pub static MEMORY_INACTIVE_ANON: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "memory_unevictable",
    description = "Memory the kernel cannot reclaim: locked, ramfs, or otherwise pinned (Unevictable)",
    metadata = { unit = "bytes", acq_group = "memory_meminfo_read" }
)]
pub static MEMORY_UNEVICTABLE: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "memory_mlocked",
    description = "Memory locked with mlock (Mlocked)",
    metadata = { unit = "bytes", acq_group = "memory_meminfo_read" }
)]
pub static MEMORY_MLOCKED: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "memory_shmem",
    description = "Shared memory: tmpfs, shm segments and shared anonymous mappings (Shmem). Counted inside Cached",
    metadata = { unit = "bytes", acq_group = "memory_meminfo_read" }
)]
pub static MEMORY_SHMEM: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "memory_mapped",
    description = "Page cache mapped into process address spaces with mmap (Mapped)",
    metadata = { unit = "bytes", acq_group = "memory_meminfo_read" }
)]
pub static MEMORY_MAPPED: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "memory_anon",
    description = "Anonymous memory mapped into process address spaces: heaps, stacks, private data (AnonPages)",
    metadata = { unit = "bytes", acq_group = "memory_meminfo_read" }
)]
pub static MEMORY_ANON: LazyGauge = LazyGauge::new(Gauge::default);

/*
 * kernel memory
 */

#[metric(
    name = "memory_slab",
    description = "Slab allocator memory the kernel can reclaim under pressure: dentry and inode caches among others (SReclaimable)",
    metadata = { unit = "bytes", kind = "reclaimable", acq_group = "memory_meminfo_read" }
)]
pub static MEMORY_SLAB_RECLAIMABLE: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "memory_slab",
    description = "Slab allocator memory the kernel cannot reclaim (SUnreclaim)",
    metadata = { unit = "bytes", kind = "unreclaimable", acq_group = "memory_meminfo_read" }
)]
pub static MEMORY_SLAB_UNRECLAIMABLE: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "memory_kernel_reclaimable",
    description = "Kernel allocations the kernel will try to reclaim under pressure: reclaimable slab plus other reclaimable kernel pages (KReclaimable)",
    metadata = { unit = "bytes", acq_group = "memory_meminfo_read" }
)]
pub static MEMORY_KERNEL_RECLAIMABLE: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "memory_kernel_stack",
    description = "Memory used by kernel stacks, one per thread (KernelStack)",
    metadata = { unit = "bytes", acq_group = "memory_meminfo_read" }
)]
pub static MEMORY_KERNEL_STACK: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "memory_page_tables",
    description = "Memory used by process page tables (PageTables)",
    metadata = { unit = "bytes", acq_group = "memory_meminfo_read" }
)]
pub static MEMORY_PAGE_TABLES: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "memory_percpu",
    description = "Memory allocated to the per-CPU allocator (Percpu)",
    metadata = { unit = "bytes", acq_group = "memory_meminfo_read" }
)]
pub static MEMORY_PERCPU: LazyGauge = LazyGauge::new(Gauge::default);

/*
 * swap
 */

#[metric(
    name = "memory_swap_total",
    description = "Total swap space (SwapTotal)",
    metadata = { unit = "bytes", acq_group = "memory_meminfo_read" }
)]
pub static MEMORY_SWAP_TOTAL: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "memory_swap_free",
    description = "Unused swap space (SwapFree)",
    metadata = { unit = "bytes", acq_group = "memory_meminfo_read" }
)]
pub static MEMORY_SWAP_FREE: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "memory_swap_cached",
    description = "Memory that was swapped out and is back in RAM while its swap slot is still allocated (SwapCached)",
    metadata = { unit = "bytes", acq_group = "memory_meminfo_read" }
)]
pub static MEMORY_SWAP_CACHED: LazyGauge = LazyGauge::new(Gauge::default);

/*
 * overcommit
 */

#[metric(
    name = "memory_commit_limit",
    description = "The amount of memory the kernel will allow to be committed under the current overcommit policy (CommitLimit). Only enforced with vm.overcommit_memory=2",
    metadata = { unit = "bytes", acq_group = "memory_meminfo_read" }
)]
pub static MEMORY_COMMIT_LIMIT: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "memory_committed",
    description = "Memory currently committed to processes: the sum of every allocation that could be touched, whether or not it has been (Committed_AS)",
    metadata = { unit = "bytes", acq_group = "memory_meminfo_read" }
)]
pub static MEMORY_COMMITTED: LazyGauge = LazyGauge::new(Gauge::default);

/*
 * huge pages
 */

#[metric(
    name = "memory_hugepages_anon",
    description = "Anonymous memory backed by transparent huge pages (AnonHugePages)",
    metadata = { unit = "bytes", acq_group = "memory_meminfo_read" }
)]
pub static MEMORY_HUGEPAGES_ANON: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "memory_hugepages_shmem",
    description = "Shared memory backed by transparent huge pages (ShmemHugePages)",
    metadata = { unit = "bytes", acq_group = "memory_meminfo_read" }
)]
pub static MEMORY_HUGEPAGES_SHMEM: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "memory_hugepages_file",
    description = "Page cache backed by transparent huge pages (FileHugePages)",
    metadata = { unit = "bytes", acq_group = "memory_meminfo_read" }
)]
pub static MEMORY_HUGEPAGES_FILE: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "memory_hugetlb",
    description = "Memory reserved for hugetlbfs pages of every size, whether in use or not (Hugetlb). Not part of MemAvailable",
    metadata = { unit = "bytes", acq_group = "memory_meminfo_read" }
)]
pub static MEMORY_HUGETLB: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "memory_hugetlb_pages",
    description = "hugetlbfs pages of the default size in the pool (HugePages_Total)",
    metadata = { unit = "pages", state = "total", acq_group = "memory_meminfo_read" }
)]
pub static MEMORY_HUGETLB_PAGES_TOTAL: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "memory_hugetlb_pages",
    description = "hugetlbfs pages of the default size not yet allocated (HugePages_Free)",
    metadata = { unit = "pages", state = "free", acq_group = "memory_meminfo_read" }
)]
pub static MEMORY_HUGETLB_PAGES_FREE: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "memory_hugetlb_pages",
    description = "hugetlbfs pages of the default size reserved by a mapping but not yet faulted in (HugePages_Rsvd)",
    metadata = { unit = "pages", state = "reserved", acq_group = "memory_meminfo_read" }
)]
pub static MEMORY_HUGETLB_PAGES_RESERVED: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "memory_hugetlb_pages",
    description = "hugetlbfs pages of the default size allocated beyond the pool size under overcommit (HugePages_Surp)",
    metadata = { unit = "pages", state = "surplus", acq_group = "memory_meminfo_read" }
)]
pub static MEMORY_HUGETLB_PAGES_SURPLUS: LazyGauge = LazyGauge::new(Gauge::default);

/*
 * errors
 */

#[metric(
    name = "memory_hardware_corrupted",
    description = "Memory the kernel has taken out of use after a hardware error was reported for it (HardwareCorrupted). Non-zero is a failing DIMM",
    metadata = { unit = "bytes", acq_group = "memory_meminfo_read" }
)]
pub static MEMORY_HARDWARE_CORRUPTED: LazyGauge = LazyGauge::new(Gauge::default);
