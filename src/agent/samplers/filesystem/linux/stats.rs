use metriken::*;

use crate::agent::timing::AcquisitionGroup;
use linkme::distributed_slice;

/// Maximum number of local filesystems tracked. Mounts discovered beyond this
/// cap are dropped by the sweep (logged once per sweep with the count).
pub const MAX_MOUNTS: usize = 64;

// Registered here (not in `linux/mod.rs`) because this file is also
// `include!`d directly on non-Linux platforms (see `filesystem/mod.rs`'s
// `#[cfg(not(target_os = "linux"))] mod stats` fallback) to keep metric
// identity stable across platforms. Same cross-platform-name mechanism as
// `drivehealth` and the BPF samplers — see
// `crate::agent::samplers::bpf_sampler_name`'s doc comment.
//
/// ONE group for the whole sweep: the mount-table read plus every
/// `statvfs` and the per-mount `set()` calls that follow, bracketed inside
/// the sweep in `linux/mod.rs`, which is this group's single writer. The
/// five metrics below are five fields of one `statvfs` answer per mount —
/// one source per entity, decoded once — so this is principle 18's "device
/// sweep" read-section shape (one group over like entities), as
/// `drivehealth` is, not five groups for five families.
pub static FILESYSTEM_SWEEP_ACQ: AcquisitionGroup = AcquisitionGroup::new(
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
    description = "Bytes an unprivileged process can still write on a locally mounted filesystem (f_bavail * f_frsize). This is the number `df` reports as available and the one a full-disk alert should watch.",
    metadata = { unit = "bytes", acq_group = "filesystem_sweep" }
)]
pub static FILESYSTEM_AVAILABLE: GaugeGroup = GaugeGroup::new(MAX_MOUNTS);

#[metric(
    name = "filesystem_inodes_total",
    description = "The number of inodes a locally mounted filesystem can hold (f_files). Zero on filesystems that allocate inodes dynamically.",
    metadata = { acq_group = "filesystem_sweep" }
)]
pub static FILESYSTEM_INODES_TOTAL: GaugeGroup = GaugeGroup::new(MAX_MOUNTS);

#[metric(
    name = "filesystem_inodes_free",
    description = "The number of free inodes on a locally mounted filesystem (f_ffree). A filesystem full of small files runs out of these before it runs out of bytes.",
    metadata = { acq_group = "filesystem_sweep" }
)]
pub static FILESYSTEM_INODES_FREE: GaugeGroup = GaugeGroup::new(MAX_MOUNTS);
