use metriken::*;

use crate::agent::timing::AcquisitionGroup;
use linkme::distributed_slice;

/// Hard series cap; excess local filesystems are skipped with a warning.
pub const MAX_MOUNTS: usize = 64;

// Must remain in stats.rs so non-Linux builds register the group too.
/// Shared window for discovery and the five gauge families; see linux/mod.rs.
pub static FILESYSTEM_SWEEP_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    // Must match metric attribution on non-Linux as well as Linux.
    crate::agent::samplers::bpf_sampler_name("filesystem"),
    "filesystem_sweep",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static FILESYSTEM_SWEEP_ACQ_REG: &'static AcquisitionGroup = &FILESYSTEM_SWEEP_ACQ;

#[metric(
    name = "filesystem_total",
    description = "The size of a locally mounted filesystem in bytes (f_blocks * f_frsize). Labeled with the `mount` point, the `fstype`, and the `device` (major:minor).",
    metadata = { unit = "bytes", acq_group = "filesystem_sweep" }
)]
pub static FILESYSTEM_TOTAL: GaugeGroup = GaugeGroup::new(MAX_MOUNTS);

#[metric(
    name = "filesystem_free",
    description = "Bytes not allocated on a locally mounted filesystem, including the blocks reserved for the superuser (f_bfree * f_frsize).",
    metadata = { unit = "bytes", acq_group = "filesystem_sweep" }
)]
pub static FILESYSTEM_FREE: GaugeGroup = GaugeGroup::new(MAX_MOUNTS);

#[metric(
    name = "filesystem_available",
    description = "Bytes an unprivileged process can still write on a locally mounted filesystem (f_bavail * f_frsize). This is the number `df` reports as available and the one to alert on.",
    metadata = { unit = "bytes", acq_group = "filesystem_sweep" }
)]
pub static FILESYSTEM_AVAILABLE: GaugeGroup = GaugeGroup::new(MAX_MOUNTS);

#[metric(
    name = "filesystem_inodes_total",
    description = "The number of inodes a locally mounted filesystem reports it can hold (f_files). Fixed at mkfs time on ext4; an estimate that moves with free space on XFS and ZFS, which allocate inodes on demand; zero on btrfs and vfat, which report no inode limit.",
    metadata = { acq_group = "filesystem_sweep" }
)]
pub static FILESYSTEM_INODES_TOTAL: GaugeGroup = GaugeGroup::new(MAX_MOUNTS);

#[metric(
    name = "filesystem_inodes_free",
    description = "The number of free inodes on a locally mounted filesystem (f_ffree). A filesystem full of small files runs out of these before it runs out of bytes.",
    metadata = { acq_group = "filesystem_sweep" }
)]
pub static FILESYSTEM_INODES_FREE: GaugeGroup = GaugeGroup::new(MAX_MOUNTS);
