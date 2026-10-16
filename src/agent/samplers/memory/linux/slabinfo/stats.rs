use metriken::*;

use crate::agent::timing::AcquisitionGroup;
use linkme::distributed_slice;

// Registered here (not in mod.rs) because this file is also `include!`d
// directly on non-Linux platforms (see `memory/mod.rs`'s
// `#[cfg(not(target_os = "linux"))] mod stats` fallback) to keep metric
// identity stable across platforms, while `mod.rs`'s sampler code is
// Linux-only.
//
/// ONE group for the whole `/proc/slabinfo` sweep: one read, one parse, every
/// cache's gauges set from it, bracketed inside the `spawn_blocking` task in
/// `mod.rs`, which is this group's single writer.
pub static SLABINFO_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("memory_slabinfo"),
    "memory_slabinfo_sweep",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static SLABINFO_ACQ_REG: &'static AcquisitionGroup = &SLABINFO_ACQ;

/// Slots in each family below. A kernel lists a few hundred caches (196 on
/// the 56-vCPU guest measured in docs/metrics.md); `slab_nomerge` raises
/// that. Caches past the cap are not published, with a warning when the first
/// one is left out. The member bound limits each snapshot to occupied slots, so
/// unused capacity costs memory only.
pub const MAX_CACHES: usize = 1024;

// The three families share one slot space (`CACHE_IDENTITY` in mod.rs): a
// cache's slot carries its `cache` label in all three, and a cache that goes
// reads absent in all three.

#[metric(
    name = "memory_slab_cache_bytes",
    description = "Memory held by one slab cache: slabs times pages per slab times the page size, free objects within those slabs included. An upper bound: /proc/slabinfo gives pages per slab at the cache's preferred order, and slabs allocated at its smaller fallback order under memory pressure hold fewer pages. Labeled with the `cache` name as /proc/slabinfo prints it. Every cache by default; with `caches = \"fixed\"`, only the caches memory_slab_cache_objects follows.",
    metadata = { unit = "bytes", acq_group = "memory_slabinfo_sweep" }
)]
pub static SLAB_CACHE_BYTES: GaugeGroup = GaugeGroup::new(MAX_CACHES);

#[metric(
    name = "memory_slab_cache_objects",
    description = "Objects in use in one of the followed slab caches: dentry, inode_cache, ext4_inode_cache, ext4_extent_status, jbd2_journal_head, buffer_head, xfs_inode and radix_tree_node. Absent for other caches and while the cache is not listed.",
    metadata = { unit = "objects", state = "active", acq_group = "memory_slabinfo_sweep" }
)]
pub static SLAB_CACHE_OBJECTS_ACTIVE: GaugeGroup = GaugeGroup::new(MAX_CACHES);

#[metric(
    name = "memory_slab_cache_objects",
    description = "Objects allocated to one of the followed slab caches, in use or free within its slabs. Absent for other caches and while the cache is not listed.",
    metadata = { unit = "objects", state = "total", acq_group = "memory_slabinfo_sweep" }
)]
pub static SLAB_CACHE_OBJECTS_TOTAL: GaugeGroup = GaugeGroup::new(MAX_CACHES);

/// The caches whose object counts are published, as the kernel spells them.
/// Their size answers a question the agent's other samplers raise: whether
/// the filesystem metadata caches and the page-cache index fit.
// Read by the Linux sweep only; this file is also `include!`d on other
// platforms for metric registration.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub const FOLLOWED: &[&str] = &[
    "dentry",
    "inode_cache",
    "ext4_inode_cache",
    "ext4_extent_status",
    "jbd2_journal_head",
    "buffer_head",
    "xfs_inode",
    "radix_tree_node",
];
