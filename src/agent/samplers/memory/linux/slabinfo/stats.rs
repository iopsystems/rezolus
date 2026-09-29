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

/// One slab cache the sampler follows: the `/proc/slabinfo` name and the
/// three gauges filled from its line. The list is fixed at the caches whose
/// size answers a question the agent's other samplers raise; a cache the
/// running kernel does not have leaves its gauges never set, so absent.
pub struct Cache {
    pub name: &'static str,
    pub active_objects: &'static LazyGauge,
    pub total_objects: &'static LazyGauge,
    pub bytes: &'static LazyGauge,
}

macro_rules! cache {
    ($cache:literal, $active:ident, $total:ident, $bytes:ident, $what:literal) => {
        #[metric(
            name = "memory_slab_cache_objects",
            description = concat!("Objects in use in the ", $cache, " slab cache: ", $what),
            metadata = { unit = "objects", cache = $cache, state = "active", acq_group = "memory_slabinfo_sweep" }
        )]
        pub static $active: LazyGauge = LazyGauge::new(Gauge::default);

        #[metric(
            name = "memory_slab_cache_objects",
            description = concat!("Objects allocated to the ", $cache, " slab cache, in use or free within its slabs: ", $what),
            metadata = { unit = "objects", cache = $cache, state = "total", acq_group = "memory_slabinfo_sweep" }
        )]
        pub static $total: LazyGauge = LazyGauge::new(Gauge::default);

        #[metric(
            name = "memory_slab_cache_bytes",
            description = concat!("Memory held by the ", $cache, " slab cache (slabs times pages per slab times the page size): ", $what),
            metadata = { unit = "bytes", cache = $cache, acq_group = "memory_slabinfo_sweep" }
        )]
        pub static $bytes: LazyGauge = LazyGauge::new(Gauge::default);
    };
}

cache!(
    "dentry",
    DENTRY_ACTIVE,
    DENTRY_TOTAL,
    DENTRY_BYTES,
    "the directory-entry cache, one per path component the kernel remembers"
);
cache!(
    "inode_cache",
    INODE_CACHE_ACTIVE,
    INODE_CACHE_TOTAL,
    INODE_CACHE_BYTES,
    "VFS inodes for filesystems without their own inode cache"
);
cache!(
    "ext4_inode_cache",
    EXT4_INODE_CACHE_ACTIVE,
    EXT4_INODE_CACHE_TOTAL,
    EXT4_INODE_CACHE_BYTES,
    "ext4 in-memory inodes. Whether this stays resident decides whether stat and atime updates on a large filesystem read the inode table from disk (see ext4_inode_loads)"
);
cache!(
    "ext4_extent_status",
    EXT4_EXTENT_STATUS_ACTIVE,
    EXT4_EXTENT_STATUS_TOTAL,
    EXT4_EXTENT_STATUS_BYTES,
    "ext4's extent status tree entries, its cache of block mappings"
);
cache!(
    "jbd2_journal_head",
    JBD2_JOURNAL_HEAD_ACTIVE,
    JBD2_JOURNAL_HEAD_TOTAL,
    JBD2_JOURNAL_HEAD_BYTES,
    "jbd2 journal heads, one per buffer the journal is tracking"
);
cache!(
    "buffer_head",
    BUFFER_HEAD_ACTIVE,
    BUFFER_HEAD_TOTAL,
    BUFFER_HEAD_BYTES,
    "buffer heads, one per block of cached filesystem metadata and of page-cache pages mapped through buffers"
);
cache!(
    "xfs_inode",
    XFS_INODE_ACTIVE,
    XFS_INODE_TOTAL,
    XFS_INODE_BYTES,
    "XFS in-memory inodes"
);
cache!(
    "radix_tree_node",
    RADIX_TREE_NODE_ACTIVE,
    RADIX_TREE_NODE_TOTAL,
    RADIX_TREE_NODE_BYTES,
    "xarray nodes, chiefly the page-cache index: a large one is a large page cache"
);

/// The caches followed, in `/proc/slabinfo` name order of no significance.
pub static CACHES: &[Cache] = &[
    Cache {
        name: "dentry",
        active_objects: &DENTRY_ACTIVE,
        total_objects: &DENTRY_TOTAL,
        bytes: &DENTRY_BYTES,
    },
    Cache {
        name: "inode_cache",
        active_objects: &INODE_CACHE_ACTIVE,
        total_objects: &INODE_CACHE_TOTAL,
        bytes: &INODE_CACHE_BYTES,
    },
    Cache {
        name: "ext4_inode_cache",
        active_objects: &EXT4_INODE_CACHE_ACTIVE,
        total_objects: &EXT4_INODE_CACHE_TOTAL,
        bytes: &EXT4_INODE_CACHE_BYTES,
    },
    Cache {
        name: "ext4_extent_status",
        active_objects: &EXT4_EXTENT_STATUS_ACTIVE,
        total_objects: &EXT4_EXTENT_STATUS_TOTAL,
        bytes: &EXT4_EXTENT_STATUS_BYTES,
    },
    Cache {
        name: "jbd2_journal_head",
        active_objects: &JBD2_JOURNAL_HEAD_ACTIVE,
        total_objects: &JBD2_JOURNAL_HEAD_TOTAL,
        bytes: &JBD2_JOURNAL_HEAD_BYTES,
    },
    Cache {
        name: "buffer_head",
        active_objects: &BUFFER_HEAD_ACTIVE,
        total_objects: &BUFFER_HEAD_TOTAL,
        bytes: &BUFFER_HEAD_BYTES,
    },
    Cache {
        name: "xfs_inode",
        active_objects: &XFS_INODE_ACTIVE,
        total_objects: &XFS_INODE_TOTAL,
        bytes: &XFS_INODE_BYTES,
    },
    Cache {
        name: "radix_tree_node",
        active_objects: &RADIX_TREE_NODE_ACTIVE,
        total_objects: &RADIX_TREE_NODE_TOTAL,
        bytes: &RADIX_TREE_NODE_BYTES,
    },
];
