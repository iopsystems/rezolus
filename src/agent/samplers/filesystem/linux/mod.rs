//! Occupancy and read-only state of local filesystems, one series per
//! superblock, sampled from `/proc/self/mountinfo` and `fstatvfs`: total, free
//! and available bytes, total and free inodes, and the superblock read-only flag.
//! For an ext4 filesystem the same sweep also reads its sysfs error count and
//! lifetime device writes from `/sys/fs/ext4/<block_device>/`. tmpfs mounts are
//! read on the same sweep into their own families; see "tmpfs" below.
//!
//! ```text
//! mountinfo -> resolve visible mounts / classify -> assign slots
//!           -> open / verify mount id / fstatvfs -> gauges -> window
//!           -> (ext4) /sys/fs/ext4/<block_device>/{errors_count,lifetime_write_kbytes}
//! ```
//!
//! # Scope and blocking
//!
//! [`mounts::local_mounts`] selects `/dev`-backed filesystems and ZFS,
//! excluding known network types, FUSE, autofs and pseudo-filesystems. It also
//! skips a local filesystem whose path passes through one of those blocking
//! types, since looking the path up walks that mount, and fails closed on a
//! path that starts in a mount the table omits: a chroot's root, whose type is
//! unknown. Candidates skipped for such a reason, or for an ambiguous table, are
//! logged when the reasons change: a warning when skipping starts, debug for
//! candidates skipped while others already are, info when all resolve again.
//! Network `statvfs` calls can block on an unavailable server; autofs path
//! lookup can trigger a mount. Classification must precede path lookup.
//! Mount-id validation detects replacement after discovery when the kernel
//! supplies `STATX_MNT_ID`, but cannot prevent a lookup from blocking if a
//! network mount is stacked over the path between discovery and `open`.
//! Such a lookup leaves later sweeps skipped while it stays blocked; a sweep
//! still running after three intervals is logged as a warning.
//!
//! Network support is deferred in `docs/backlog.md`; any opt-in must bound
//! both blocking workers and per-mount wait time.
//!
//! # tmpfs
//!
//! A tmpfs mount's files are memory: meminfo counts their resident pages in
//! `Shmem`, which says how much and not where. The sweep also selects visible tmpfs mounts
//! ([`mounts::select_sampled`]) and publishes used and size bytes and inodes
//! per mount as `filesystem_tmpfs_*`, kept out of the `filesystem_*` families
//! because a tmpfs size is a limit on memory, not device capacity, and summing
//! it with disks would be wrong. `statvfs` on tmpfs reads the superblock's
//! counters and cannot block. `ramfs` is not read: it reports no usage.
//! `hugetlbfs` is meminfo's (`memory_hugetlb`).
//!
//! tmpfs mounts come and go with logins (`/run/user/<uid>`), services
//! (systemd credential mounts) and containers (a `/dev/shm` per container, a
//! secret volume per pod). They share the slot space with local filesystems
//! but have their own cap, [`MAX_TMPFS`], so they never take a local
//! filesystem's slot. When more are visible than the cap, the ones with the
//! fewest path components are placed first (`/tmp`, `/dev/shm`, `/run` before
//! a container's), and a slot once held is kept until its mount goes. The
//! first mount left out is a warning, changes in the count are debug, and
//! fitting again is info, so a churning container host does not warn every
//! sweep.
//!
//! # Cadence and publication
//!
//! Superblock counters have no BPF or perf hook, so the sweep reads procfs
//! and `fstatvfs`: a principle 15 exception (`docs/principles.md`).
//! Rescanning the table each sweep discovers mounts added after startup.
//! Consumer-driven `refresh()` dispatches every sweep, the first included, to
//! the blocking pool, at most once per configured interval (60s by default);
//! until the first lands the group is bounded to zero members.
//!
//! Each series is labeled `mount`, `fstype`, `devnum` (`major:minor`) and, for a
//! block-backed filesystem, `block_device`: the kernel's name for the partition
//! or mapped device, from `/sys/dev/block`, which is not the drive's name.
//!
//! Sweeps never overlap. Each sweep brackets discovery and every gauge group
//! with one acquisition window, stamped after successful publication;
//! an empty or failed sweep leaves the previous window unchanged, and a sweep
//! with no successful read also keeps the values that window describes. This is
//! principle 18's device-sweep shape. [`Slots`] owns membership and labels;
//! the member bound limits snapshot traversal to slots below the highest
//! occupied one, where vacant slots read as absent. It is revised every sweep,
//! which principle 18 allows for a changing population.
//! Window publication does not make values, labels and membership atomic.
//!
//! Phase measurements and scale limits live in
//! `docs/journal/2026-09-12-filesystem-sampler.md`.

const NAME: &str = "filesystem";

// 60s matches drivehealth; measured sweep cost is recorded in the journal.
const DEFAULT_READ_INTERVAL: Duration = Duration::from_secs(60);

const MOUNTINFO: &str = "/proc/self/mountinfo";

const SYS_DEV_BLOCK: &str = "/sys/dev/block";

/// ext4's per-filesystem sysfs directory, one entry per mounted filesystem
/// named by its block device (`/sys/fs/ext4/nvme0n1p5/`).
const SYS_FS_EXT4: &str = "/sys/fs/ext4";

/// A sweep still running after this many intervals is reported as stuck.
const STUCK_INTERVALS: u32 = 3;

use crate::agent::samplers::{count_change, CountChange};
use crate::agent::*;
use metriken::CounterGroup;
use metriken::GaugeGroup;

use std::collections::HashMap;
use std::ffi::CString;
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub(crate) mod mounts;
mod stats;

use mounts::MountEntry;
use stats::*;

// Every published gauge family must participate in slot clearing and relabeling.
static GROUPS: &[&GaugeGroup] = &[
    &FILESYSTEM_TOTAL,
    &FILESYSTEM_FREE,
    &FILESYSTEM_AVAILABLE,
    &FILESYSTEM_INODES_TOTAL,
    &FILESYSTEM_INODES_FREE,
    &FILESYSTEM_READONLY,
    &FILESYSTEM_ERRORS,
    &FILESYSTEM_TMPFS_USED,
    &FILESYSTEM_TMPFS_SIZE,
    &FILESYSTEM_TMPFS_INODES_USED,
    &FILESYSTEM_TMPFS_INODES_TOTAL,
];

// The one counter family, cleared and relabeled alongside GROUPS. Kept apart
// because a CounterGroup's absent sentinel is u64::MAX, not i64::MIN.
static COUNTER_GROUPS: &[&CounterGroup] = &[&FILESYSTEM_WRITTEN_BYTES];

/// What a filesystem slot means. Unlike a GPU or a drive this genuinely moves
/// at runtime — a mount appears, a filesystem is unmounted and its slot is
/// reused — so labels written only at startup would be wrong within
/// minutes.
static MOUNT_IDENTITY: metriken::group::SlotIdentity =
    metriken::group::SlotIdentity::grouped(MOUNT_IDENTITY_GROUPS);

static MOUNT_IDENTITY_GROUPS: &[&[&dyn metriken::group::SlotMetadata]] = &[&[
    &FILESYSTEM_TOTAL,
    &FILESYSTEM_FREE,
    &FILESYSTEM_AVAILABLE,
    &FILESYSTEM_INODES_TOTAL,
    &FILESYSTEM_INODES_FREE,
    &FILESYSTEM_READONLY,
    &FILESYSTEM_ERRORS,
    &FILESYSTEM_WRITTEN_BYTES,
    &FILESYSTEM_TMPFS_USED,
    &FILESYSTEM_TMPFS_SIZE,
    &FILESYSTEM_TMPFS_INODES_USED,
    &FILESYSTEM_TMPFS_INODES_TOTAL,
]];

fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    let interval = config
        .sampler_interval(NAME)
        .unwrap_or(DEFAULT_READ_INTERVAL);

    Ok(Some(Box::new(Filesystem::new(interval))))
}

#[distributed_slice(SAMPLERS)]
static SAMPLER_ENTRY: crate::agent::samplers::SamplerEntry = crate::agent::samplers::SamplerEntry {
    name: NAME,
    module: module_path!(),
    init,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Usage {
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub available_bytes: u64,
    pub total_inodes: u64,
    pub free_inodes: u64,
}

/// Read a mount selected by [`mounts::local_mounts`], directory or file.
///
/// Callers must resolve visibility and classify before this path lookup.
/// Rejects a mount-id mismatch when `STATX_MNT_ID` is available; kernels
/// before 5.8 lack that check. Lookup itself has no deadline.
pub fn read_usage(path: &str, mount_id: u64) -> std::io::Result<Usage> {
    let c_path = CString::new(path)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "NUL in path"))?;
    let raw = unsafe {
        // Must not require a directory: container file bind mounts such as
        // /etc/hosts are local mounts.
        libc::open(
            c_path.as_ptr(),
            libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if raw < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };

    let mut stx: libc::statx = unsafe { std::mem::zeroed() };
    // AT_EMPTY_PATH applies statx to the open descriptor, not a second lookup.
    let rc = unsafe {
        libc::statx(
            fd.as_raw_fd(),
            c"".as_ptr(),
            libc::AT_EMPTY_PATH,
            libc::STATX_MNT_ID,
            &mut stx,
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // btrfs subvolumes have distinct st_dev values; mount id identifies the mount.
    if stx.stx_mask & libc::STATX_MNT_ID != 0 && stx.stx_mnt_id != mount_id {
        return Err(std::io::Error::other(format!(
            "{path} now resolves to mount {}, not mount {mount_id}",
            stx.stx_mnt_id
        )));
    }

    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // Must read the validated descriptor; reopening the path reintroduces replacement races.
    let rc = unsafe { libc::fstatvfs(fd.as_raw_fd(), &mut st) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // Block counts use f_frsize; f_bsize is the preferred I/O size.
    let frsize = st.f_frsize as u64;
    Ok(Usage {
        total_bytes: (st.f_blocks as u64).saturating_mul(frsize),
        free_bytes: (st.f_bfree as u64).saturating_mul(frsize),
        available_bytes: (st.f_bavail as u64).saturating_mul(frsize),
        total_inodes: st.f_files as u64,
        free_inodes: st.f_ffree as u64,
    })
}

/// What ext4 exports about one filesystem in `/sys/fs/ext4/<block_device>/`.
/// Each field is `None` when its file is missing or unparsable, so a kernel
/// lacking one attribute leaves the other readable.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Ext4Sysfs {
    /// `errors_count`: errors recorded in the superblock since e2fsck last
    /// cleared them.
    pub errors: Option<u64>,
    /// `lifetime_write_kbytes` in bytes: device writes over the filesystem's
    /// life, journal included.
    pub written_bytes: Option<u64>,
}

/// Read one ext4 filesystem's sysfs attributes from `dir/<block_device>/`.
/// Two small reads of kernel-generated text, once per sweep per ext4 mount;
/// neither can block on a device.
pub fn read_ext4_sysfs(dir: &Path, block_device: &str) -> Ext4Sysfs {
    let attr = |name: &str| -> Option<u64> {
        let text = std::fs::read_to_string(dir.join(block_device).join(name)).ok()?;
        text.trim().parse().ok()
    };
    Ext4Sysfs {
        errors: attr("errors_count"),
        written_bytes: attr("lifetime_write_kbytes").map(|kb| kb.saturating_mul(1024)),
    }
}

/// One slot per `major:minor` device id while mounted; freed slots are reused.
/// The selected path can change without changing the device, so label state
/// tracks `(mount, fstype)` separately from slot ownership.
pub struct Slots {
    by_device: HashMap<String, usize>,
    occupied: Vec<Option<String>>,
    labeled: Vec<Option<(String, String)>>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Assignment {
    pub freed: Vec<usize>,
    /// Per class passed to [`Slots::assign_within`], the devices not placed.
    pub dropped: Vec<usize>,
}

impl Slots {
    pub fn new(capacity: usize) -> Self {
        Self {
            by_device: HashMap::new(),
            occupied: vec![None; capacity],
            labeled: vec![None; capacity],
        }
    }

    /// One class of devices, limited only by capacity.
    #[cfg(test)]
    pub fn assign(&mut self, devices: &[String]) -> Assignment {
        let capacity = self.occupied.len();
        self.assign_within(&[(devices, capacity)])
    }

    /// Free the slots of devices in no class, then place each class's new
    /// devices in order while it holds fewer than its quota and a slot is free.
    /// A device already placed keeps its slot. Quotas that sum to the capacity
    /// keep one class from taking another's slots.
    pub fn assign_within(&mut self, classes: &[(&[String], usize)]) -> Assignment {
        let mut outcome = Assignment {
            freed: Vec::new(),
            dropped: vec![0; classes.len()],
        };

        for (slot, held) in self.occupied.iter_mut().enumerate() {
            if let Some(device) = held {
                if !classes.iter().any(|(devices, _)| devices.contains(device)) {
                    self.by_device.remove(device);
                    *held = None;
                    self.labeled[slot] = None;
                    outcome.freed.push(slot);
                }
            }
        }

        for (class, (devices, quota)) in classes.iter().enumerate() {
            let mut held = devices
                .iter()
                .filter(|d| self.by_device.contains_key(*d))
                .count();
            for device in devices.iter() {
                if self.by_device.contains_key(device) {
                    continue;
                }
                let free = self.occupied.iter().position(Option::is_none);
                match free.filter(|_| held < *quota) {
                    Some(slot) => {
                        self.occupied[slot] = Some(device.clone());
                        self.by_device.insert(device.clone(), slot);
                        held += 1;
                    }
                    None => outcome.dropped[class] += 1,
                }
            }
        }

        outcome
    }

    pub fn slot_of(&self, device: &str) -> Option<usize> {
        self.by_device.get(device).copied()
    }

    /// Records the selected labels; a true result requires the caller to apply them.
    pub fn relabel(&mut self, slot: usize, mount: &MountEntry) -> bool {
        let current = (mount.mount_point.as_str(), mount.fstype.as_str());
        let applied = self.labeled[slot]
            .as_ref()
            .map(|(point, fstype)| (point.as_str(), fstype.as_str()));
        if applied == Some(current) {
            return false;
        }
        self.labeled[slot] = Some((mount.mount_point.clone(), mount.fstype.clone()));
        true
    }

    pub fn bound(&self) -> usize {
        self.occupied
            .iter()
            .rposition(Option::is_some)
            .map_or(0, |slot| slot + 1)
    }
}

/// Reuses the procfs buffer to avoid repeated allocation; measured wall time
/// was unchanged (see the filesystem sampler journal).
struct SweepState {
    slots: Slots,
    table: String,
    /// Resolution problems last logged, so a steady condition logs once.
    reported: Reported,
    /// Local filesystems and tmpfs mounts over their caps at the last sweep,
    /// so each warning fires on change.
    dropped: [usize; 2],
}

/// How many mounts could hold the process root when not one, and the skipped
/// candidates.
type Reported = (Option<usize>, Vec<mounts::Skipped>);

impl SweepState {
    fn new() -> Self {
        Self {
            slots: Slots::new(MAX_SLOTS),
            table: String::with_capacity(16 * 1024),
            reported: (None, Vec::new()),
            dropped: [0; 2],
        }
    }
}

struct Filesystem {
    interval: Duration,
    /// When the running or last sweep was dispatched.
    last_read: Mutex<Option<Instant>>,
    reading: Arc<AtomicBool>,
    stuck_warned: AtomicBool,
    state: Arc<Mutex<SweepState>>,
}

impl Filesystem {
    fn new(interval: Duration) -> Self {
        // No member until the first sweep lands. Every sweep runs on the
        // blocking pool, so a lookup cannot hang startup.
        FILESYSTEM_SWEEP_ACQ.set_member_bound(0);
        debug!("{NAME}: sweeping every {interval:?}");

        Self {
            interval,
            last_read: Mutex::new(None),
            reading: Arc::new(AtomicBool::new(false)),
            stuck_warned: AtomicBool::new(false),
            state: Arc::new(Mutex::new(SweepState::new())),
        }
    }

    /// Warn once per stuck sweep: one still running after `STUCK_INTERVALS`
    /// intervals is most likely parked in a path lookup, and the gauges hold
    /// their last values until it returns.
    fn warn_if_stuck(&self) {
        let Some(dispatched) = *self.last_read.lock().unwrap() else {
            return;
        };
        let running = dispatched.elapsed();
        if running > self.interval * STUCK_INTERVALS
            && !self.stuck_warned.swap(true, Ordering::AcqRel)
        {
            warn!(
                "{NAME}: a sweep has been running for {running:?}, over {STUCK_INTERVALS} \
                 intervals; filesystem gauges hold their last values until it returns"
            );
        }
    }
}

fn vacate(slot: usize) {
    unset(slot);
    // And clear its labels. Left in place, they would name a filesystem that
    // is no longer mounted, and the slot is reusable immediately.
    MOUNT_IDENTITY.release(slot);
}

/// Every value family reads absent for `slot`; labels and identity stay.
fn unset(slot: usize) {
    for group in GROUPS {
        // Must use the absent sentinel, not zero, when a filesystem leaves.
        let _ = group.set(slot, i64::MIN);
    }
    for group in COUNTER_GROUPS {
        // u64::MAX is CounterGroup's never-written sentinel.
        let _ = group.set(slot, u64::MAX);
    }
}

fn label(slot: usize, mount: &MountEntry, block_device: Option<&str>) {
    let mut labels = std::collections::BTreeMap::new();
    labels.insert("mount".to_string(), mount.mount_point.clone());
    labels.insert("fstype".to_string(), mount.fstype.clone());
    labels.insert("devnum".to_string(), mount.device.clone());
    if let Some(name) = block_device {
        labels.insert("block_device".to_string(), name.to_string());
    }
    MOUNT_IDENTITY.assign(slot, labels);
}

/// The kernel's name for block device `devnum` (`nvme0n1p5`, `dm-0`), from its
/// `/sys/dev/block` link. `None` for a filesystem with no block device, such as
/// a ZFS dataset or btrfs, whose `major:minor` is anonymous.
fn block_device_name(sys_dev_block: &Path, devnum: &str) -> Option<String> {
    let target = std::fs::read_link(sys_dev_block.join(devnum)).ok()?;
    target.file_name()?.to_str().map(str::to_string)
}

/// Label `slot` for `mount` when its identity changed. A new identity starts
/// with no reading, so a failed read cannot leave the previous identity's
/// values under the new labels.
fn apply_identity(slots: &mut Slots, slot: usize, mount: &MountEntry) {
    if slots.relabel(slot, mount) {
        vacate(slot);
        let name = block_device_name(Path::new(SYS_DEV_BLOCK), &mount.device);
        label(slot, mount, name.as_deref());
    }
}

/// Log why filesystem candidates are not sampled, comparing this sweep's
/// reasons with the last. Skips starting is a warning per candidate; on a host
/// whose mounts churn the set moves every sweep, so a candidate newly skipped
/// while others already are is logged at debug, and every candidate resolving
/// again is info.
fn report_selection(previous: &Reported, current: &Reported) {
    let (starts, skipped) = current;
    if let Some(n) = starts {
        if previous.0 != *starts {
            warn!(
                "{NAME}: the mount table names {n} mounts that could hold the process root, \
                 not one; no filesystem is sampled"
            );
        }
    }
    let already = !previous.1.is_empty();
    for skip in skipped.iter().filter(|s| !previous.1.contains(s)) {
        if already {
            debug!("{NAME}: not sampling {skip}");
        } else {
            warn!("{NAME}: not sampling {skip}");
        }
    }
    if starts.is_none() && skipped.is_empty() {
        info!("{NAME}: every filesystem candidate resolves again");
    } else if already {
        debug!("{NAME}: {} candidate(s) skipped", skipped.len());
    }
}

/// Log a change in how many mounts of one kind are over their cap; see
/// [`count_change`].
fn report_dropped(what: &str, cap: usize, last: usize, now: usize) {
    match count_change(last, now) {
        Some(CountChange::Began(n)) => {
            warn!("{NAME}: {n} {what}(s) beyond the {cap}-mount cap are not sampled")
        }
        Some(CountChange::Moved(n)) => {
            debug!("{NAME}: {n} {what}(s) beyond the {cap}-mount cap are not sampled")
        }
        Some(CountChange::Ended) => {
            info!("{NAME}: every {what} fits within the {cap}-mount cap again")
        }
        None => {}
    }
}

fn gauge(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// An inode gauge for a filesystem reporting `total` inodes. Zero means no inode
/// limit (btrfs, vfat), so the value is absent rather than 0 and no consumer
/// divides by it.
fn inode_gauge(total: u64, value: u64) -> i64 {
    if total == 0 {
        i64::MIN
    } else {
        gauge(value)
    }
}

/// Returns the number of mounts whose readings were published.
fn sweep(state: &mut SweepState) -> usize {
    sweep_table(state, MOUNTINFO)
}

/// One sweep over the mount table at `path`.
fn sweep_table(state: &mut SweepState, path: &str) -> usize {
    let started = Instant::now();
    let guard = FILESYSTEM_SWEEP_ACQ.acquire();

    let SweepState {
        slots,
        table,
        reported,
        dropped,
    } = state;
    table.clear();
    if let Err(e) = std::fs::File::open(path).and_then(|mut f| f.read_to_string(table)) {
        warn!("{NAME}: could not read {path}: {e}");
        // The last known population, 0 before any sweep succeeds: a failed first
        // read must not leave the group walking its full capacity.
        FILESYSTEM_SWEEP_ACQ.set_member_bound(slots.bound());
        guard.discard();
        return 0;
    }
    let read_done = Instant::now();

    let selection = mounts::select_sampled(table);
    let parse_done = Instant::now();
    let now = (selection.unresolvable_starts, selection.skipped.clone());
    if *reported != now {
        report_selection(reported, &now);
        *reported = now;
    }
    let mounts = selection.mounts;
    let (local, tmpfs) = classes(&mounts);
    let assignment = slots.assign_within(&[
        (local.as_slice(), MAX_MOUNTS),
        (tmpfs.as_slice(), MAX_TMPFS),
    ]);

    // Reused slots must be cleared before any new labels or readings are published.
    for slot in &assignment.freed {
        vacate(*slot);
    }
    for (class, (what, cap)) in [("local filesystem", MAX_MOUNTS), ("tmpfs mount", MAX_TMPFS)]
        .into_iter()
        .enumerate()
    {
        report_dropped(what, cap, dropped[class], assignment.dropped[class]);
        dropped[class] = assignment.dropped[class];
    }

    let mut reads = Vec::with_capacity(mounts.len());
    for mount in &mounts {
        let Some(slot) = slots.slot_of(&mount.device) else {
            continue;
        };
        apply_identity(slots, slot, mount);
        let read = read_usage(&mount.mount_point, mount.id);
        if let Err(e) = &read {
            debug!("{NAME}: statvfs {} failed: {e}", mount.mount_point);
        }
        if mount.is_tmpfs() {
            reads.push(Reading::Tmpfs { slot, read });
            continue;
        }
        // sysfs names the directory after the block device, the same link
        // the label resolves; a filesystem without one has no directory.
        let ext4 = (mount.fstype == "ext4")
            .then(|| block_device_name(Path::new(SYS_DEV_BLOCK), &mount.device))
            .flatten()
            .map(|name| read_ext4_sysfs(Path::new(SYS_FS_EXT4), &name));
        reads.push(Reading::Local {
            slot,
            readonly: mount.readonly,
            read,
            ext4,
        });
    }
    let published = publish(&reads);

    // The sweep is the sole writer; the bound must be stored before finish().
    FILESYSTEM_SWEEP_ACQ.set_member_bound(slots.bound());

    // A sweep with no published readings must not advance the acquisition window.
    if published > 0 {
        guard.finish();
    } else {
        guard.discard();
    }
    debug!(
        "{NAME}: sweep published {published}/{} filesystem(s) ({} local, {} tmpfs selected; \
         {} local, {} tmpfs over the cap) in {} us (read {} us, parse {} us, statvfs+publish {} us)",
        mounts.len(),
        local.len(),
        tmpfs.len(),
        dropped[0],
        dropped[1],
        started.elapsed().as_micros(),
        (read_done - started).as_micros(),
        (parse_done - read_done).as_micros(),
        parse_done.elapsed().as_micros()
    );
    published
}

/// One mount's sweep result. A local filesystem carries its superblock
/// read-only flag and, on ext4, its sysfs attributes (`None` off ext4).
pub enum Reading {
    Local {
        slot: usize,
        readonly: bool,
        read: std::io::Result<Usage>,
        ext4: Option<Ext4Sysfs>,
    },
    Tmpfs {
        slot: usize,
        read: std::io::Result<Usage>,
    },
}

impl Reading {
    fn slot(&self) -> usize {
        match self {
            Reading::Local { slot, .. } | Reading::Tmpfs { slot, .. } => *slot,
        }
    }

    fn succeeded(&self) -> bool {
        match self {
            Reading::Local { read, .. } | Reading::Tmpfs { read, .. } => read.is_ok(),
        }
    }
}

/// The tmpfs gauges for one `statvfs`. A tmpfs mounted with `size=0` reports
/// zero blocks and no usage, and one with `nr_inodes=0` zero inodes: absent,
/// not 0, so no consumer reads a limitless mount as empty.
fn tmpfs_gauges(usage: &Usage) -> [i64; 4] {
    let blocks = usage.total_bytes != 0;
    let inodes = usage.total_inodes != 0;
    let or_absent = |present: bool, value: u64| if present { gauge(value) } else { i64::MIN };
    [
        or_absent(blocks, usage.total_bytes.saturating_sub(usage.free_bytes)),
        or_absent(blocks, usage.total_bytes),
        or_absent(inodes, usage.total_inodes.saturating_sub(usage.free_inodes)),
        or_absent(inodes, usage.total_inodes),
    ]
}

/// Publish one sweep's reads and return how many succeeded.
///
/// When anything succeeded, failed slots are unset so the fresh window never
/// pairs with stale values. When nothing did, the caller keeps the previous
/// window, and failed slots must keep the values that window describes.
fn publish(reads: &[Reading]) -> usize {
    let published = reads.iter().filter(|r| r.succeeded()).count();
    for reading in reads {
        match reading {
            Reading::Local {
                slot,
                readonly,
                read: Ok(usage),
                ext4,
            } => {
                let slot = *slot;
                let _ = FILESYSTEM_TOTAL.set(slot, gauge(usage.total_bytes));
                let _ = FILESYSTEM_FREE.set(slot, gauge(usage.free_bytes));
                let _ = FILESYSTEM_AVAILABLE.set(slot, gauge(usage.available_bytes));
                let total = usage.total_inodes;
                let _ = FILESYSTEM_INODES_TOTAL.set(slot, inode_gauge(total, total));
                let _ = FILESYSTEM_INODES_FREE.set(slot, inode_gauge(total, usage.free_inodes));
                // From the superblock options, not statvfs `f_flag`: its
                // ST_RDONLY is also set by this path's own mount flag.
                let _ = FILESYSTEM_READONLY.set(slot, i64::from(*readonly));
                // Absent, not 0, off ext4 or when the attribute is missing.
                let ext4 = ext4.unwrap_or_default();
                let _ = FILESYSTEM_ERRORS.set(slot, ext4.errors.map_or(i64::MIN, gauge));
                let _ = FILESYSTEM_WRITTEN_BYTES.set(slot, ext4.written_bytes.unwrap_or(u64::MAX));
            }
            Reading::Tmpfs {
                slot,
                read: Ok(usage),
            } => {
                let [used, size, inodes_used, inodes_total] = tmpfs_gauges(usage);
                let _ = FILESYSTEM_TMPFS_USED.set(*slot, used);
                let _ = FILESYSTEM_TMPFS_SIZE.set(*slot, size);
                let _ = FILESYSTEM_TMPFS_INODES_USED.set(*slot, inodes_used);
                let _ = FILESYSTEM_TMPFS_INODES_TOTAL.set(*slot, inodes_total);
            }
            failed if published > 0 => unset(failed.slot()),
            _ => {}
        }
    }
    published
}

/// The devices of the local filesystems in `mounts`, in mount-point order, and
/// of the tmpfs mounts, fewest path components first: the order each class
/// is placed in when it has more than its cap.
fn classes(mounts: &[MountEntry]) -> (Vec<String>, Vec<String>) {
    let mut tmpfs: Vec<&MountEntry> = mounts.iter().filter(|m| m.is_tmpfs()).collect();
    tmpfs.sort_by_key(|m| depth(&m.mount_point));
    let devices =
        |ms: Vec<&MountEntry>| -> Vec<String> { ms.iter().map(|m| m.device.clone()).collect() };
    (
        devices(mounts.iter().filter(|m| !m.is_tmpfs()).collect()),
        devices(tmpfs),
    )
}

/// Path components below `/`: `/` is 0, `/dev/shm` is 2.
fn depth(mount_point: &str) -> usize {
    mount_point.split('/').filter(|c| !c.is_empty()).count()
}

/// Clears the in-flight latch on drop, so a sweep that panics does not leave
/// every later refresh skipping its sweep.
struct InFlight(Arc<AtomicBool>);

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// Lock the sweep state, starting it afresh if a panicking sweep poisoned it: a
/// slot map abandoned mid-update cannot be trusted.
fn lock_state(state: &Mutex<SweepState>) -> std::sync::MutexGuard<'_, SweepState> {
    state.lock().unwrap_or_else(|poisoned| {
        warn!("{NAME}: a previous sweep panicked; restarting slot assignment");
        state.clear_poison();
        // The abandoned map may not know which slots hold readings, so clear
        // every one before a fresh map reuses them.
        for slot in 0..MAX_SLOTS {
            vacate(slot);
        }
        let mut guard = poisoned.into_inner();
        *guard = SweepState::new();
        guard
    })
}

#[async_trait]
impl Sampler for Filesystem {
    fn name(&self) -> &'static str {
        NAME
    }

    async fn refresh(&self) {
        // Checked before the throttle: a held latch must not advance `last_read`,
        // which times how long the running sweep has been going.
        if self.reading.load(Ordering::Acquire) {
            self.warn_if_stuck();
            return;
        }
        {
            let mut last = self.last_read.lock().unwrap();
            match *last {
                Some(t) if t.elapsed() < self.interval => return,
                _ => *last = Some(Instant::now()),
            }
        }

        if self.reading.swap(true, Ordering::AcqRel) {
            return;
        }
        self.stuck_warned.store(false, Ordering::Release);

        // Only the blocking task may stamp the group; refresh must not stamp it.
        let state = self.state.clone();
        let reading = self.reading.clone();
        tokio::task::spawn_blocking(move || {
            // Must stay held until the sweep returns or unwinds.
            let _in_flight = InFlight(reading);
            sweep(&mut lock_state(&state));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mounts::local_mounts;

    #[test]
    fn slots_are_stable_for_a_mount_that_stays_and_reused_for_one_that_goes() {
        let mut slots = Slots::new(4);
        let first = slots.assign(&["259:2".to_string(), "8:1".to_string()]);
        assert_eq!(slots.slot_of("259:2"), Some(0));
        assert_eq!(slots.slot_of("8:1"), Some(1));
        assert!(first.freed.is_empty());
        assert_eq!(slots.bound(), 2);

        let second = slots.assign(&["259:2".to_string(), "8:17".to_string()]);
        assert_eq!(second.freed, vec![1]);
        assert_eq!(slots.slot_of("259:2"), Some(0));
        assert_eq!(slots.slot_of("8:17"), Some(1));
        assert_eq!(slots.bound(), 2);
    }

    #[test]
    fn the_bound_follows_the_highest_occupied_slot_down_as_well_as_up() {
        let mut slots = Slots::new(4);
        slots.assign(&["a".to_string(), "b".to_string(), "c".to_string()]);
        assert_eq!(slots.bound(), 3);
        slots.assign(&["a".to_string()]);
        assert_eq!(slots.bound(), 1);
    }

    #[test]
    fn mounts_past_the_capacity_are_dropped_and_counted() {
        let mut slots = Slots::new(2);
        let outcome = slots.assign(&["a".to_string(), "b".to_string(), "c".to_string()]);
        assert_eq!(slots.bound(), 2);
        assert_eq!(outcome.dropped, vec![1]);
        assert_eq!(slots.slot_of("c"), None);
    }

    // The visible root of any type: overlay, tmpfs and network roots are
    // excluded from sampling but still resolve.
    fn root_mount() -> MountEntry {
        let table = std::fs::read_to_string(MOUNTINFO).unwrap();
        mounts::visible_mounts(&table)
            .into_iter()
            .find(|m| m.mount_point == "/")
            .expect("the root mount resolves")
    }

    #[test]
    fn statvfs_on_the_root_filesystem_reports_a_consistent_shape() {
        let root = root_mount();
        let usage = read_usage("/", root.id).expect("statvfs on / works on any Linux host");
        assert!(usage.total_bytes > 0);
        assert!(usage.free_bytes <= usage.total_bytes);
        assert!(usage.available_bytes <= usage.free_bytes);
        assert!(usage.total_inodes >= usage.free_inodes);
    }

    #[test]
    fn a_vacated_slot_reads_as_absent_with_no_labels() {
        let _globals = SWEEP_GLOBALS.lock().unwrap_or_else(|p| p.into_inner());
        // Avoids real-sweep slots only on hosts with fewer than MAX_MOUNTS filesystems.
        let slot = MAX_MOUNTS - 1;
        let mount = MountEntry {
            id: 900,
            parent: 1,
            mount_point: "/scratch".to_string(),
            fstype: "xfs".to_string(),
            source: "/dev/sdz1".to_string(),
            device: "8:401".to_string(),
            readonly: false,
        };
        label(slot, &mount, Some("sdz1"));
        for group in GROUPS {
            assert!(group.set(slot, 7));
            let m = group.load_metadata(slot).expect("labels set");
            assert_eq!(m.get("mount").map(String::as_str), Some("/scratch"));
            assert_eq!(m.get("fstype").map(String::as_str), Some("xfs"));
            assert_eq!(m.get("devnum").map(String::as_str), Some("8:401"));
            assert_eq!(m.get("block_device").map(String::as_str), Some("sdz1"));
            assert!(
                !m.contains_key("device"),
                "no label claims a drivehealth join"
            );
        }

        assert!(FILESYSTEM_WRITTEN_BYTES.set(slot, 7));

        vacate(slot);
        for group in GROUPS {
            assert_eq!(group.value(slot), None, "value must read absent, not zero");
            assert!(group.load_metadata(slot).is_none_or(|m| m.is_empty()));
        }
        assert_eq!(
            FILESYSTEM_WRITTEN_BYTES.value(slot),
            None,
            "the counter must read absent, not zero"
        );
    }

    #[test]
    fn ext4_sysfs_attributes_are_read_per_block_device_and_absent_when_missing() {
        let sys = tempfile::tempdir().unwrap();
        let dir = sys.path().join("sdz1");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("errors_count"), "3\n").unwrap();
        std::fs::write(dir.join("lifetime_write_kbytes"), "2048\n").unwrap();
        assert_eq!(
            read_ext4_sysfs(sys.path(), "sdz1"),
            Ext4Sysfs {
                errors: Some(3),
                written_bytes: Some(2 * 1024 * 1024),
            }
        );
        // No directory: the device is not ext4, or sysfs is not mounted.
        assert_eq!(read_ext4_sysfs(sys.path(), "sdz2"), Ext4Sysfs::default());
        // One attribute missing or unparsable leaves the other readable.
        std::fs::write(dir.join("errors_count"), "not a number\n").unwrap();
        assert_eq!(
            read_ext4_sysfs(sys.path(), "sdz1"),
            Ext4Sysfs {
                errors: None,
                written_bytes: Some(2 * 1024 * 1024),
            }
        );
    }

    fn local(slot: usize, read: std::io::Result<Usage>, ext4: Option<Ext4Sysfs>) -> Reading {
        Reading::Local {
            slot,
            readonly: false,
            read,
            ext4,
        }
    }

    #[test]
    fn ext4_attributes_publish_beside_usage_and_read_absent_off_ext4() {
        let _globals = SWEEP_GLOBALS.lock().unwrap_or_else(|p| p.into_inner());
        let (ext4, xfs) = (MAX_MOUNTS - 4, MAX_MOUNTS - 5);
        let usage = Usage {
            total_bytes: 7,
            free_bytes: 7,
            available_bytes: 7,
            total_inodes: 7,
            free_inodes: 7,
        };
        let attrs = Ext4Sysfs {
            errors: Some(2),
            written_bytes: Some(4096),
        };
        assert_eq!(
            publish(&[
                local(ext4, Ok(usage), Some(attrs)),
                local(xfs, Ok(usage), None)
            ]),
            2
        );
        assert_eq!(FILESYSTEM_ERRORS.value(ext4), Some(2));
        assert_eq!(FILESYSTEM_WRITTEN_BYTES.value(ext4), Some(4096));
        assert_eq!(FILESYSTEM_ERRORS.value(xfs), None, "absent off ext4, not 0");
        assert_eq!(FILESYSTEM_WRITTEN_BYTES.value(xfs), None);
        assert_eq!(FILESYSTEM_TOTAL.value(xfs), Some(7));
    }

    /// Verifies gauges, labels and the acquisition window for a readable local
    /// mount; returns early on hosts with none, such as an overlay-root container.
    #[test]
    fn a_sweep_publishes_a_local_filesystem_and_stamps_the_group() {
        let _globals = SWEEP_GLOBALS.lock().unwrap_or_else(|p| p.into_inner());
        let table = std::fs::read_to_string(MOUNTINFO).unwrap();
        let Some(mount) = local_mounts(&table)
            .into_iter()
            .find(|m| read_usage(&m.mount_point, m.id).is_ok())
        else {
            eprintln!("skipped: no readable local filesystem on this host");
            return;
        };

        let mut state = SweepState::new();
        let published = sweep(&mut state);
        assert!(published >= 1, "no local filesystem published");
        let slots = &state.slots;
        let slot = slots
            .slot_of(&mount.device)
            .expect("the mount holds a slot");
        assert!(slot < slots.bound());

        let total = FILESYSTEM_TOTAL.value(slot).expect("total set");
        let free = FILESYSTEM_FREE.value(slot).expect("free set");
        let available = FILESYSTEM_AVAILABLE.value(slot).expect("available set");
        assert!(total > 0 && free <= total && available <= free);
        assert_eq!(
            FILESYSTEM_INODES_TOTAL.value(slot).is_some(),
            FILESYSTEM_INODES_FREE.value(slot).is_some(),
            "inode gauges are present together, or absent together where there is no limit"
        );
        assert!(FILESYSTEM_READONLY
            .value(slot)
            .is_some_and(|v| v == 0 || v == 1));
        if mount.fstype == "ext4" && Path::new(SYS_FS_EXT4).is_dir() {
            assert!(
                FILESYSTEM_ERRORS.value(slot).is_some(),
                "ext4 errors_count read"
            );
            assert!(
                FILESYSTEM_WRITTEN_BYTES.value(slot).is_some(),
                "ext4 lifetime_write_kbytes read"
            );
        } else {
            assert_eq!(FILESYSTEM_ERRORS.value(slot), None);
            assert_eq!(FILESYSTEM_WRITTEN_BYTES.value(slot), None);
        }
        let labels = FILESYSTEM_TOTAL.load_metadata(slot).expect("labels set");
        assert_eq!(
            labels.get("mount").map(String::as_str),
            Some(mount.mount_point.as_str())
        );
        assert_eq!(
            labels.get("devnum").map(String::as_str),
            Some(mount.device.as_str())
        );
        assert!(
            !labels.contains_key("device"),
            "no label claims a drivehealth join"
        );

        let w = FILESYSTEM_SWEEP_ACQ
            .window()
            .expect("the sweep stamped its group");
        assert!(w.width_ns() > 0);
    }

    /// Manual phase measurement:
    ///   cargo test --release --bin rezolus -- filesystem::linux::tests::sweep_phase_timing --ignored --nocapture
    #[test]
    #[ignore]
    fn sweep_phase_timing() {
        let n = 200u32;
        let mut table = String::with_capacity(16 * 1024);
        let (mut t_read, mut t_parse, mut t_stat) = (0u128, 0u128, 0u128);
        let mut mounts_seen = 0;
        for _ in 0..n {
            let t0 = Instant::now();
            table.clear();
            std::fs::File::open(MOUNTINFO)
                .and_then(|mut f| f.read_to_string(&mut table))
                .unwrap();
            let t1 = Instant::now();
            let mounts = local_mounts(&table);
            let t2 = Instant::now();
            for m in &mounts {
                let _ = read_usage(&m.mount_point, m.id);
            }
            let t3 = Instant::now();
            mounts_seen = mounts.len();
            t_read += (t1 - t0).as_nanos();
            t_parse += (t2 - t1).as_nanos();
            t_stat += (t3 - t2).as_nanos();
        }
        let n = u128::from(n) * 1000;
        println!(
            "mountinfo {} bytes, {} local mounts: read {} us, parse {} us, statvfs x{} {} us",
            table.len(),
            mounts_seen,
            t_read / n,
            t_parse / n,
            mounts_seen,
            t_stat / n
        );
    }

    #[test]
    fn statvfs_on_a_missing_path_is_an_error_not_a_zero() {
        assert!(read_usage("/definitely/not/a/mount/point", 0).is_err());
    }

    /// Container file bind mounts such as `/etc/hosts` are files, so a read
    /// must not require a directory.
    #[test]
    fn a_regular_file_reads_through_its_mount() {
        let dir = std::env::temp_dir().canonicalize().unwrap();
        let file = dir.join(format!("rezolus-filesystem-test-{}", std::process::id()));
        std::fs::write(&file, b"x").unwrap();
        let path = file.to_str().unwrap().to_string();

        let table = std::fs::read_to_string(MOUNTINFO).unwrap();
        let holder = mounts::visible_mounts(&table)
            .into_iter()
            .filter(|m| m.mount_point == "/" || path.starts_with(&format!("{}/", m.mount_point)))
            .max_by_key(|m| m.mount_point.len())
            .expect("a visible mount holds the temp dir");
        let result = read_usage(&path, holder.id);
        let _ = std::fs::remove_file(&file);
        result.expect("a regular file resolves to its mount");
    }

    /// Requires STATX_MNT_ID support (Linux 5.8+).
    #[test]
    fn a_path_resolving_to_a_different_mount_is_refused() {
        let root = root_mount();
        let err = read_usage("/", root.id.wrapping_add(1 << 40))
            .expect_err("a mount id mismatch must refuse");
        assert!(err.to_string().contains("now resolves to mount"), "{err}");
    }

    #[test]
    fn a_panicking_sweep_clears_the_in_flight_latch() {
        let reading = Arc::new(AtomicBool::new(true));
        let latch = reading.clone();
        let _ = std::panic::catch_unwind(move || {
            let _in_flight = InFlight(latch);
            panic!("sweep failed");
        });
        assert!(!reading.load(Ordering::Acquire));
    }

    #[test]
    fn a_poisoned_sweep_state_is_restarted_rather_than_left_locked() {
        let _globals = SWEEP_GLOBALS.lock().unwrap_or_else(|p| p.into_inner());
        let stale = MAX_MOUNTS - 5;
        let _ = FILESYSTEM_TOTAL.set(stale, 777);
        let state = Arc::new(Mutex::new(SweepState::new()));
        state.lock().unwrap().slots.assign(&["8:1".to_string()]);
        let poisoner = state.clone();
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.lock().unwrap();
            panic!("sweep failed");
        })
        .join();
        assert!(state.is_poisoned());

        let guard = lock_state(&state);
        assert_eq!(guard.slots.bound(), 0, "the abandoned slot map is replaced");
        drop(guard);
        assert!(!state.is_poisoned());
        assert_eq!(
            FILESYSTEM_TOTAL.value(stale),
            None,
            "a restart clears readings the fresh map cannot account for"
        );
    }

    #[test]
    fn a_filesystem_without_an_inode_limit_publishes_absent_not_zero() {
        assert_eq!(inode_gauge(0, 0), i64::MIN);
        assert_eq!(inode_gauge(1000, 250), 250);
    }

    /// Serializes tests that write the process-wide gauge groups or acquisition
    /// group.
    static SWEEP_GLOBALS: Mutex<()> = Mutex::new(());

    #[test]
    fn a_failed_table_read_bounds_the_group_to_the_last_population() {
        let _globals = SWEEP_GLOBALS.lock().unwrap_or_else(|p| p.into_inner());
        let missing = "/definitely/not/mountinfo";
        let mut state = SweepState::new();
        assert_eq!(sweep_table(&mut state, missing), 0);
        assert_eq!(
            FILESYSTEM_SWEEP_ACQ.member_bound(),
            Some(0),
            "a failed first read is bounded, never left walking capacity"
        );

        state.slots.assign(&["8:1".to_string(), "8:2".to_string()]);
        sweep_table(&mut state, missing);
        assert_eq!(FILESYSTEM_SWEEP_ACQ.member_bound(), Some(2));
    }

    #[test]
    fn a_sweep_with_no_successful_read_keeps_the_previous_values() {
        let _globals = SWEEP_GLOBALS.lock().unwrap_or_else(|p| p.into_inner());
        // The top slots, clear of real sweeps on hosts with few filesystems.
        let (kept, unset) = (MAX_MOUNTS - 2, MAX_MOUNTS - 3);
        let usage = Usage {
            total_bytes: 7,
            free_bytes: 7,
            available_bytes: 7,
            total_inodes: 7,
            free_inodes: 7,
        };
        let missing = || -> std::io::Result<Usage> { Err(std::io::ErrorKind::NotFound.into()) };
        assert_eq!(
            publish(&[local(kept, Ok(usage), None), local(unset, Ok(usage), None)]),
            2
        );

        assert_eq!(publish(&[local(kept, missing(), None)]), 0);
        assert_eq!(
            FILESYSTEM_TOTAL.value(kept),
            Some(7),
            "nothing succeeded: keep"
        );

        assert_eq!(
            publish(&[local(kept, Ok(usage), None), local(unset, missing(), None)]),
            1
        );
        assert_eq!(
            FILESYSTEM_TOTAL.value(unset),
            None,
            "something succeeded: the failed slot is unset"
        );
    }

    /// A slot whose identity changes must not show the previous reading under
    /// the new labels, even if every read in the sweep then fails.
    #[test]
    fn a_new_identity_on_a_retained_slot_starts_without_the_old_reading() {
        let _globals = SWEEP_GLOBALS.lock().unwrap_or_else(|p| p.into_inner());
        let slot = MAX_MOUNTS - 4;
        let mut slots = Slots::new(MAX_MOUNTS);
        apply_identity(&mut slots, slot, &ext4_at("/old"));
        let _ = FILESYSTEM_TOTAL.set(slot, 777);

        apply_identity(&mut slots, slot, &ext4_at("/old"));
        assert_eq!(
            FILESYSTEM_TOTAL.value(slot),
            Some(777),
            "an unchanged identity keeps its reading"
        );

        apply_identity(&mut slots, slot, &ext4_at("/new"));
        assert_eq!(
            FILESYSTEM_TOTAL.value(slot),
            None,
            "a new identity starts absent"
        );
        let labels = FILESYSTEM_TOTAL.load_metadata(slot).expect("labels set");
        assert_eq!(labels.get("mount").map(String::as_str), Some("/new"));
    }

    #[test]
    fn a_block_device_is_named_by_its_sysfs_link_and_an_anonymous_one_is_not() {
        let sys = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(
            "../../devices/pci0000:00/0000:00:01.0/nvme/nvme0/nvme0n1/nvme0n1p5",
            sys.path().join("259:3"),
        )
        .unwrap();
        assert_eq!(
            block_device_name(sys.path(), "259:3").as_deref(),
            Some("nvme0n1p5")
        );
        assert_eq!(block_device_name(sys.path(), "0:42"), None);
    }

    #[tokio::test]
    async fn a_held_latch_neither_dispatches_nor_advances_the_throttle() {
        let fs = {
            let _globals = SWEEP_GLOBALS.lock().unwrap_or_else(|p| p.into_inner());
            Filesystem::new(Duration::from_millis(10))
        };
        let dispatched = Instant::now()
            .checked_sub(Duration::from_secs(1))
            .expect("the host has been up for a second");
        *fs.last_read.lock().unwrap() = Some(dispatched);
        fs.reading.store(true, Ordering::Release);

        fs.refresh().await;
        assert_eq!(*fs.last_read.lock().unwrap(), Some(dispatched));
        assert!(
            fs.stuck_warned.load(Ordering::Acquire),
            "a sweep 100 intervals old is reported as stuck"
        );
    }

    fn ext4_at(point: &str) -> MountEntry {
        MountEntry {
            id: 40,
            parent: 22,
            mount_point: point.to_string(),
            fstype: "ext4".to_string(),
            source: "/dev/sda1".to_string(),
            device: "8:1".to_string(),
            readonly: false,
        }
    }

    /// tmpfs mounts beyond their cap are dropped, the shallowest placed first,
    /// and never take a slot a local filesystem needs.
    #[test]
    fn tmpfs_mounts_have_their_own_cap_and_the_shallowest_are_placed_first() {
        let table = "\
22 1 259:2 / / rw - ext4 /dev/nvme0n1p2 rw
24 22 0:25 / /dev/shm rw - tmpfs tmpfs rw
25 22 0:26 / /run rw - tmpfs tmpfs rw,size=26389620k
26 25 0:27 / /run/lock rw - tmpfs tmpfs rw,size=5120k
27 22 0:28 / /tmp rw - tmpfs tmpfs rw
30 22 0:60 / /var/lib/docker/containers/3f2a/mounts/shm rw - tmpfs shm rw,size=65536k";
        let (local, tmpfs) = classes(&mounts::select_sampled(table).mounts);
        assert_eq!(local, vec!["259:2"]);
        assert_eq!(tmpfs, vec!["0:26", "0:28", "0:25", "0:27", "0:60"]);

        // Two local slots and three tmpfs slots in a space of five.
        let mut slots = Slots::new(5);
        let outcome = slots.assign_within(&[(local.as_slice(), 2), (tmpfs.as_slice(), 3)]);
        assert_eq!(outcome.dropped, vec![0, 2]);
        for (device, placed) in [
            ("0:26", true),
            ("0:28", true),
            ("0:25", true),
            ("0:27", false),
        ] {
            assert_eq!(slots.slot_of(device).is_some(), placed, "{device}");
        }
        // A disk mounted later still finds a slot: tmpfs left one free.
        let disks = vec!["259:2".to_string(), "8:1".to_string()];
        let outcome = slots.assign_within(&[(disks.as_slice(), 2), (tmpfs.as_slice(), 3)]);
        assert_eq!(outcome.dropped, vec![0, 2]);
        assert!(slots.slot_of("8:1").is_some());
        // A held tmpfs slot is kept when a shallower mount would now rank above it.
        let more = [vec!["0:99".to_string()], tmpfs.clone()].concat();
        let outcome = slots.assign_within(&[(disks.as_slice(), 2), (more.as_slice(), 3)]);
        assert_eq!(outcome.dropped, vec![0, 3]);
        assert_eq!(slots.slot_of("0:99"), None);
    }

    #[test]
    fn tmpfs_usage_publishes_to_its_own_families_and_a_limitless_one_is_absent() {
        let _globals = SWEEP_GLOBALS.lock().unwrap_or_else(|p| p.into_inner());
        let (shm, unlimited) = (MAX_SLOTS - 1, MAX_SLOTS - 2);
        let mib = 1 << 20;
        let usage = Usage {
            total_bytes: 64 * mib,
            free_bytes: 16 * mib,
            available_bytes: 16 * mib,
            total_inodes: 1000,
            free_inodes: 900,
        };
        let limitless = Usage {
            total_bytes: 0,
            free_bytes: 0,
            available_bytes: 0,
            total_inodes: 0,
            free_inodes: 0,
        };
        assert_eq!(
            publish(&[
                Reading::Tmpfs {
                    slot: shm,
                    read: Ok(usage)
                },
                Reading::Tmpfs {
                    slot: unlimited,
                    read: Ok(limitless)
                },
            ]),
            2
        );
        assert_eq!(FILESYSTEM_TMPFS_USED.value(shm), Some(48 * mib as i64));
        assert_eq!(FILESYSTEM_TMPFS_SIZE.value(shm), Some(64 * mib as i64));
        assert_eq!(FILESYSTEM_TMPFS_INODES_USED.value(shm), Some(100));
        assert_eq!(FILESYSTEM_TMPFS_INODES_TOTAL.value(shm), Some(1000));
        assert_eq!(FILESYSTEM_TOTAL.value(shm), None, "not a local filesystem");
        assert_eq!(FILESYSTEM_READONLY.value(shm), None);
        for group in [
            &FILESYSTEM_TMPFS_USED,
            &FILESYSTEM_TMPFS_SIZE,
            &FILESYSTEM_TMPFS_INODES_USED,
            &FILESYSTEM_TMPFS_INODES_TOTAL,
        ] {
            assert_eq!(group.value(unlimited), None, "no limit reports no usage");
        }
    }

    /// The agent's own mount table, where it has a tmpfs (`/dev/shm` on most
    /// hosts and containers): the sweep publishes it under tmpfs labels and
    /// leaves the local families absent on its slot.
    #[test]
    fn a_sweep_publishes_a_tmpfs_mount() {
        let _globals = SWEEP_GLOBALS.lock().unwrap_or_else(|p| p.into_inner());
        let table = std::fs::read_to_string(MOUNTINFO).unwrap();
        let Some(mount) = mounts::select_sampled(&table).mounts.into_iter().find(|m| {
            m.is_tmpfs() && read_usage(&m.mount_point, m.id).is_ok_and(|u| u.total_bytes > 0)
        }) else {
            eprintln!("skipped: no readable tmpfs with a size limit on this host");
            return;
        };
        let mut state = SweepState::new();
        assert!(sweep(&mut state) >= 1);
        let slot = state
            .slots
            .slot_of(&mount.device)
            .expect("the tmpfs holds a slot");
        assert!(FILESYSTEM_TMPFS_SIZE.value(slot).is_some_and(|v| v > 0));
        assert!(FILESYSTEM_TMPFS_USED.value(slot).is_some());
        assert_eq!(FILESYSTEM_TOTAL.value(slot), None);
        let labels = FILESYSTEM_TMPFS_USED
            .load_metadata(slot)
            .expect("labels set");
        assert_eq!(labels.get("fstype").map(String::as_str), Some("tmpfs"));
        assert_eq!(labels.get("mount"), Some(&mount.mount_point));
    }

    #[test]
    fn a_retained_slot_is_relabeled_when_its_mount_point_moves() {
        let mut slots = Slots::new(4);
        slots.assign(&["8:1".to_string()]);
        let slot = slots.slot_of("8:1").unwrap();
        assert!(slots.relabel(slot, &ext4_at("/old")), "first mount labels");
        assert!(
            !slots.relabel(slot, &ext4_at("/old")),
            "unchanged keeps labels"
        );

        let moved = slots.assign(&["8:1".to_string()]);
        assert!(moved.freed.is_empty());
        assert_eq!(slots.slot_of("8:1"), Some(slot), "the slot is retained");
        assert!(
            slots.relabel(slot, &ext4_at("/new")),
            "a moved mount relabels"
        );
    }

    #[test]
    fn losing_the_selected_bind_alias_relabels_onto_the_one_that_remains() {
        let both = "\
22 1 259:2 / / rw - ext4 /dev/nvme0n1p2 rw
40 22 8:1 / /a rw - ext4 /dev/sda1 rw
41 22 8:1 / /mnt/longer rw - ext4 /dev/sda1 rw";
        let one = "\
22 1 259:2 / / rw - ext4 /dev/nvme0n1p2 rw
41 22 8:1 / /mnt/longer rw - ext4 /dev/sda1 rw";
        let sweep_labels = |slots: &mut Slots, text: &str| -> (bool, String) {
            let mounts = local_mounts(text);
            let devices: Vec<String> = mounts.iter().map(|m| m.device.clone()).collect();
            slots.assign(&devices);
            let sda1 = mounts.into_iter().find(|m| m.device == "8:1").unwrap();
            let slot = slots.slot_of("8:1").unwrap();
            (slots.relabel(slot, &sda1), sda1.mount_point)
        };

        let mut slots = Slots::new(4);
        assert_eq!(sweep_labels(&mut slots, both), (true, "/a".to_string()));
        assert_eq!(sweep_labels(&mut slots, both), (false, "/a".to_string()));
        assert_eq!(
            sweep_labels(&mut slots, one),
            (true, "/mnt/longer".to_string())
        );
    }

    #[test]
    fn a_reused_slot_is_labeled_afresh() {
        let mut slots = Slots::new(4);
        slots.assign(&["8:1".to_string()]);
        assert!(slots.relabel(0, &ext4_at("/data")));
        slots.assign(&[]);
        slots.assign(&["8:1".to_string()]);
        assert!(
            slots.relabel(0, &ext4_at("/data")),
            "a vacated slot's labels were cleared, so it must label again"
        );
    }
}
