use metriken::*;

use crate::agent::timing::AcquisitionGroup;
use linkme::distributed_slice;

/// Hard series cap for local filesystems; excess ones are skipped, with a
/// warning when their count changes. Costs memory only: the member bound limits
/// each snapshot to occupied slots.
pub const MAX_MOUNTS: usize = 256;

/// Hard series cap for tmpfs mounts, held apart from [`MAX_MOUNTS`] so tmpfs
/// mounts never take a local filesystem's slot. A host's own tmpfs mounts
/// (`/run`, `/dev/shm`, `/tmp`, `/run/user/<uid>`, systemd credential mounts)
/// fit with room to spare; a container host adds one or more per container,
/// and the cap is what bounds those.
pub const MAX_TMPFS: usize = 64;

/// Slots in every gauge family: local filesystems and tmpfs mounts share one
/// slot space, so one member bound and one set of labels serve both.
pub const MAX_SLOTS: usize = MAX_MOUNTS + MAX_TMPFS;

// The sampler the group belongs to: `filesystem` where a sampler compiles this
// file in its own module (Linux and macOS), `unattributed` elsewhere.
#[cfg(any(target_os = "linux", target_os = "macos"))]
const GROUP_SAMPLER: &str = "filesystem";
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
const GROUP_SAMPLER: &str = crate::agent::samplers::bpf_sampler_name("filesystem");

// Must remain in stats.rs so non-Linux builds register the group too.
/// Shared window for discovery and every gauge family; see linux/mod.rs.
pub static FILESYSTEM_SWEEP_ACQ: AcquisitionGroup =
    AcquisitionGroup::new(GROUP_SAMPLER, "filesystem_sweep");

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static FILESYSTEM_SWEEP_ACQ_REG: &'static AcquisitionGroup = &FILESYSTEM_SWEEP_ACQ;

#[metric(
    name = "filesystem_total",
    description = "The size of a locally mounted filesystem in bytes (f_blocks * f_frsize). Labeled with the `mount` point, the `fstype`, the `devnum` (major:minor) and, for a block-backed filesystem, the kernel `block_device` name of its partition or mapped device.",
    metadata = { unit = "bytes", acq_group = "filesystem_sweep" }
)]
pub static FILESYSTEM_TOTAL: GaugeGroup = GaugeGroup::new(MAX_SLOTS);

#[metric(
    name = "filesystem_free",
    description = "Bytes not allocated on a locally mounted filesystem, including the blocks reserved for the superuser (f_bfree * f_frsize).",
    metadata = { unit = "bytes", acq_group = "filesystem_sweep" }
)]
pub static FILESYSTEM_FREE: GaugeGroup = GaugeGroup::new(MAX_SLOTS);

#[metric(
    name = "filesystem_available",
    description = "Bytes an unprivileged process can still write on a locally mounted filesystem (f_bavail * f_frsize). This is the number `df` reports as available and the one to alert on.",
    metadata = { unit = "bytes", acq_group = "filesystem_sweep" }
)]
pub static FILESYSTEM_AVAILABLE: GaugeGroup = GaugeGroup::new(MAX_SLOTS);

#[metric(
    name = "filesystem_inodes_total",
    description = "The number of inodes a locally mounted filesystem reports it can hold (f_files). Fixed at mkfs time on ext4; an estimate that moves with free space on XFS and ZFS, which allocate inodes on demand; absent on filesystems that report no inode limit, such as btrfs and vfat.",
    metadata = { acq_group = "filesystem_sweep" }
)]
pub static FILESYSTEM_INODES_TOTAL: GaugeGroup = GaugeGroup::new(MAX_SLOTS);

#[metric(
    name = "filesystem_inodes_free",
    description = "The number of free inodes on a locally mounted filesystem (f_ffree). A filesystem full of small files runs out of these before it runs out of bytes. Absent where the filesystem reports no inode limit.",
    metadata = { acq_group = "filesystem_sweep" }
)]
pub static FILESYSTEM_INODES_FREE: GaugeGroup = GaugeGroup::new(MAX_SLOTS);

#[metric(
    name = "filesystem_readonly",
    description = "1 when a locally mounted filesystem is read-only as a whole, 0 otherwise: its superblock is read-only (a read-only mount, or btrfs forced read-only after an error), or ext4 has gone emergency read-only after an error (emergency_ro). A read-only bind of a writable filesystem reads 0 and a full filesystem stays writable; an XFS shutdown sets neither signal, so 0 does not prove the filesystem is writable.",
    metadata = { acq_group = "filesystem_sweep" }
)]
pub static FILESYSTEM_READONLY: GaugeGroup = GaugeGroup::new(MAX_SLOTS);

// ext4-only, from /sys/fs/ext4/<block_device>/; absent on every other type.
// Read on the same sweep as statvfs: docs/journal/2026-09-28-ext4-sampler.md
// ("Errors: count with the tracepoint, and also read the sysfs gauge").

#[metric(
    name = "filesystem_errors",
    description = "Errors an ext4 filesystem has recorded in its superblock (sysfs errors_count). The count persists across remounts and reboots until e2fsck clears it, so a non-zero value means the filesystem has hit an error since its last check, whether or not it has since gone read-only. Absent on other filesystem types.",
    metadata = { acq_group = "filesystem_sweep" }
)]
pub static FILESYSTEM_ERRORS: GaugeGroup = GaugeGroup::new(MAX_SLOTS);

#[metric(
    name = "filesystem_written_bytes",
    description = "Bytes written to the block device beneath an ext4 filesystem over the filesystem's lifetime, journal included (sysfs lifetime_write_kbytes: the total the superblock persists plus the device's write sectors since mount). Its rate is the write bandwidth the filesystem puts on the device, the last term of write amplification. Absent on other filesystem types.",
    metadata = { unit = "bytes", acq_group = "filesystem_sweep" }
)]
pub static FILESYSTEM_WRITTEN_BYTES: CounterGroup = CounterGroup::new(MAX_SLOTS);

// tmpfs, read on the same sweep with the same statvfs. Kept out of the
// filesystem_* families above: a tmpfs size is a limit on memory, not
// capacity on a device. Absent on a slot that holds a local filesystem, and
// the filesystem_* families are absent on a tmpfs slot.

#[metric(
    name = "filesystem_tmpfs_used",
    description = "Bytes of files held by a tmpfs mount, resident or swapped out ((f_blocks - f_bfree) * f_frsize). The resident part is in meminfo's Shmem and the swapped part is not. Labeled with the `mount` point and `devnum`. Absent on a tmpfs mounted without a size limit (size=0), which reports no usage.",
    metadata = { unit = "bytes", acq_group = "filesystem_sweep" }
)]
pub static FILESYSTEM_TMPFS_USED: GaugeGroup = GaugeGroup::new(MAX_SLOTS);

#[metric(
    name = "filesystem_tmpfs_size",
    description = "The size limit of a tmpfs mount in bytes (f_blocks * f_frsize): the `size=` mount option, by default half of RAM. A limit only; tmpfs holds no memory it is not using. Absent without a size limit.",
    metadata = { unit = "bytes", acq_group = "filesystem_sweep" }
)]
pub static FILESYSTEM_TMPFS_SIZE: GaugeGroup = GaugeGroup::new(MAX_SLOTS);

#[metric(
    name = "filesystem_tmpfs_inodes_used",
    description = "Inodes in use on a tmpfs mount (f_files - f_ffree): its files, directories and links. Absent without an inode limit (nr_inodes=0).",
    metadata = { acq_group = "filesystem_sweep" }
)]
pub static FILESYSTEM_TMPFS_INODES_USED: GaugeGroup = GaugeGroup::new(MAX_SLOTS);

#[metric(
    name = "filesystem_tmpfs_inodes_total",
    description = "The inode limit of a tmpfs mount (f_files): the `nr_inodes=` mount option. Absent without an inode limit.",
    metadata = { acq_group = "filesystem_sweep" }
)]
pub static FILESYSTEM_TMPFS_INODES_TOTAL: GaugeGroup = GaugeGroup::new(MAX_SLOTS);
