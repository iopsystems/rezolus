//! Occupancy of locally mounted filesystems: total, free and available
//! bytes, and total and free inodes, one `statvfs` per mount.
//!
//! # Local mounts only
//!
//! The mount table is read from `/proc/self/mountinfo` on each sweep and
//! every mount is classified **before** any `statvfs` is issued
//! ([`mounts::MountEntry::is_local`]). Only local filesystems are sampled:
//! block-backed types with a `/dev` source, plus `zfs`. Network filesystems
//! (`nfs`, `cifs`, `ceph`, ...), every FUSE filesystem, autofs triggers and
//! the kernel's pseudo-filesystems are never touched.
//!
//! Neither is a local mount that another mount covers — one stacked on the
//! same path, or mounted on a directory above it — because a path resolves to
//! the mount on top, and `statvfs` on the path of an ext4 mount with an NFS
//! share stacked over it reaches the share ([`mounts::local_mounts`]). The
//! call goes through a descriptor whose mount id must match the table's, so
//! a mount that replaced the sampled one since the table was read is never
//! published. That check follows the path lookup rather than preventing it: a
//! network mount stacked over a local path between the table read and the
//! open can still park the sweep thread, and every later sweep is skipped
//! while it stays parked.
//!
//! The reason is the failure mode, not the value. `statvfs` on a network
//! mount is an RPC, and on a `hard` mount (the default) it blocks until the
//! server answers, with no timeout the calling process can set — the
//! `timeo`/`retrans`/`soft` knobs are mount options owned by whoever mounted
//! the share. A `statvfs` on an autofs trigger starts a mount attempt. Local
//! filesystems answer from in-memory superblock counters and issue no I/O, so
//! the sweep is bounded by construction, which is what lets it run at all
//! without a per-call deadline. `df -l` draws the same line.
//!
//! Network mounts stay out of scope until there is demand for them. An
//! opt-in must bring its own blocking budget — a bounded thread and a
//! deadline per mount — because the classification above is the only thing
//! keeping this sweep off a dead NFS server. See `docs/backlog.md`.
//!
//! # Why procfs, and why its own cadence
//!
//! Filesystem occupancy has no BPF or perf hook; the superblock counters are
//! reachable only through `statvfs` (or the `df` family that wraps it), and
//! the set of mounts only through the mount table. This is the deliberate
//! principle-15 exception (`docs/principles.md`), documented here so a later
//! reviewer does not have to rediscover it.
//!
//! The mount table is re-read on every sweep, not once at startup as
//! `drivehealth` enumerates drives, because a filesystem mounted after the
//! agent started and then filling up is exactly the case this metric exists
//! for. A sweep costs a few hundred µs, most of it the kernel generating the
//! mount table on each open; the cost grows with the size of that table, not
//! with the number of `statvfs` calls, since the mounts a container host has
//! thousands of are filtered out before any call. Measured by phase in
//! `docs/journal/2026-09-12-filesystem-sampler.md`.
//!
//! Even so the sweep does not run on the scrape/TTL sample cycle (principle
//! 17): `refresh()` does a cheap time check and, at most once per `interval`
//! (`[samplers.filesystem]`, default 60s), dispatches the sweep to Tokio's
//! blocking pool and returns immediately. Occupancy moves slowly — even a
//! writer at 1 GB/s consumes 60 GB per interval — and a `statvfs` is a
//! syscall, which does not belong on the async worker however cheap it is.
//!
//! # Membership
//!
//! Each local filesystem, identified by its `major:minor` device id, holds a
//! stable slot in the five gauge groups for as long as it stays mounted; its
//! `mount`, `fstype` and `device` labels are set when the slot is assigned.
//! When it is unmounted the slot's values are unset (`i64::MIN`, which the
//! snapshot walk reads as absent) and its labels cleared, and the slot is
//! reused by the next new mount. The acquisition group's member bound follows
//! the highest occupied slot, updated by the sweep task — the group's single
//! writer — before the window is stamped, so a snapshot never walks
//! `MAX_MOUNTS` slots for a host with three mounts.

const NAME: &str = "filesystem";

/// Built-in read cadence when `[samplers.filesystem] interval` is unset.
/// Matches `drivehealth`: occupancy is a slow-moving gauge, and 60s keeps the
/// amortized cost of the sweep to about 10 µs per second of wall time.
const DEFAULT_READ_INTERVAL: Duration = Duration::from_secs(60);

const MOUNTINFO: &str = "/proc/self/mountinfo";

use crate::agent::*;
use metriken::GaugeGroup;

use std::collections::HashMap;
use std::ffi::CString;
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

mod mounts;
mod stats;

use mounts::{local_mounts, MountEntry};
use stats::*;

/// The five gauge groups a sweep writes, in a fixed order, so labels and
/// unsets are applied uniformly.
static GROUPS: &[&GaugeGroup] = &[
    &FILESYSTEM_TOTAL,
    &FILESYSTEM_FREE,
    &FILESYSTEM_AVAILABLE,
    &FILESYSTEM_INODES_TOTAL,
    &FILESYSTEM_INODES_FREE,
];

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

/// What one `statvfs` says about a filesystem, in the units the metrics use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Usage {
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub available_bytes: u64,
    pub total_inodes: u64,
    pub free_inodes: u64,
}

/// `fstatvfs` on the filesystem mounted at `path`, refused unless `path`
/// still resolves to mount `mount_id`. Callers must pass only a mount that
/// [`mounts::local_mounts`] returned: for anything else the path lookup can
/// block for as long as the mount's options allow, with no deadline
/// available to the caller (see the module doc).
pub fn read_usage(path: &str, mount_id: u64) -> std::io::Result<Usage> {
    let c_path = CString::new(path)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "NUL in path"))?;
    // SAFETY: `c_path` is a valid NUL-terminated string.
    let raw = unsafe {
        libc::open(
            c_path.as_ptr(),
            libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if raw < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `open` just returned this descriptor, and nothing else owns it.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };

    let mut stx: libc::statx = unsafe { std::mem::zeroed() };
    // SAFETY: `fd` is open, an empty path with `AT_EMPTY_PATH` names it, and
    // `stx` is a properly sized, writable `statx`.
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
    // Matched on mount id, not `st_dev`: btrfs reports `st_dev` per
    // subvolume, which need not equal the device mountinfo names. Kernels
    // before 5.8 report no mount id, leaving the table's cover check as the
    // only guard.
    if stx.stx_mask & libc::STATX_MNT_ID != 0 && stx.stx_mnt_id != mount_id {
        return Err(std::io::Error::other(format!(
            "{path} now resolves to mount {}, not mount {mount_id}",
            stx.stx_mnt_id
        )));
    }

    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `fd` is open and `st` is a properly sized, writable `statvfs`.
    let rc = unsafe { libc::fstatvfs(fd.as_raw_fd(), &mut st) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // `f_frsize` is the fragment size the block counts are in units of;
    // `f_bsize` is the preferred I/O size and is the wrong multiplier.
    let frsize = st.f_frsize as u64;
    Ok(Usage {
        total_bytes: (st.f_blocks as u64).saturating_mul(frsize),
        free_bytes: (st.f_bfree as u64).saturating_mul(frsize),
        available_bytes: (st.f_bavail as u64).saturating_mul(frsize),
        total_inodes: st.f_files as u64,
        free_inodes: st.f_ffree as u64,
    })
}

/// Stable slot assignment: one slot per filesystem (by device id) for as long
/// as it stays mounted, freed slots reused lowest-first.
pub struct Slots {
    by_device: HashMap<String, usize>,
    occupied: Vec<Option<String>>,
    /// The `(mount, fstype)` labels last applied to each slot.
    labeled: Vec<Option<(String, String)>>,
}

/// What changed in one [`Slots::assign`]: which slots were vacated, and how
/// many mounts did not fit.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Assignment {
    pub freed: Vec<usize>,
    pub dropped: usize,
}

impl Slots {
    pub fn new(capacity: usize) -> Self {
        Self {
            by_device: HashMap::new(),
            occupied: vec![None; capacity],
            labeled: vec![None; capacity],
        }
    }

    /// Reconcile the slots with the devices currently mounted, in the order
    /// given (sorted by mount point upstream, so a fresh start is
    /// deterministic).
    pub fn assign(&mut self, devices: &[String]) -> Assignment {
        let mut outcome = Assignment::default();

        // Vacate slots whose device is gone.
        for (slot, held) in self.occupied.iter_mut().enumerate() {
            if let Some(device) = held {
                if !devices.contains(device) {
                    self.by_device.remove(device);
                    *held = None;
                    self.labeled[slot] = None;
                    outcome.freed.push(slot);
                }
            }
        }

        // Place new devices in the lowest free slots.
        for device in devices {
            if self.by_device.contains_key(device) {
                continue;
            }
            match self.occupied.iter().position(Option::is_none) {
                Some(slot) => {
                    self.occupied[slot] = Some(device.clone());
                    self.by_device.insert(device.clone(), slot);
                }
                None => outcome.dropped += 1,
            }
        }

        outcome
    }

    pub fn slot_of(&self, device: &str) -> Option<usize> {
        self.by_device.get(device).copied()
    }

    /// Whether `slot` needs labeling for `mount`, recording that it now has
    /// it. True on a slot's first mount, and again whenever a retained slot's
    /// selected mount point or type changes: a filesystem remounted elsewhere,
    /// or its shortest bind alias gone while a longer one remains.
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

    /// One past the highest occupied slot: the member bound the snapshot walk
    /// uses.
    pub fn bound(&self) -> usize {
        self.occupied
            .iter()
            .rposition(Option::is_some)
            .map_or(0, |slot| slot + 1)
    }
}

/// State one sweep hands the next: the slot map, and the buffer the mount
/// table is read into.
///
/// The buffer is reused for the allocation it saves, not for speed: measured,
/// it did not move the sweep's wall time. `/proc/self/mountinfo` reports a
/// zero size, so reading into a fresh `String` doubles from 32 bytes and
/// issues a dozen `read()`s where a warm buffer takes one.
struct SweepState {
    slots: Slots,
    table: String,
}

impl SweepState {
    fn new() -> Self {
        Self {
            slots: Slots::new(MAX_MOUNTS),
            table: String::with_capacity(16 * 1024),
        }
    }
}

struct Filesystem {
    /// Minimum spacing between sweeps.
    interval: Duration,
    /// Timestamp of the last dispatched sweep; `None` until the first.
    last_read: Mutex<Option<Instant>>,
    /// True while a sweep is in flight, so sweeps never overlap.
    reading: Arc<AtomicBool>,
    /// Carried between sweeps. Only the sweep task touches it.
    state: Arc<Mutex<SweepState>>,
}

impl Filesystem {
    fn new(interval: Duration) -> Self {
        let state = Arc::new(Mutex::new(SweepState::new()));

        // The first sweep runs inline at init so the member bound is set
        // before any snapshot walk reads it (the same "before the first
        // walk" contract `drivehealth` meets by enumerating in `new()`), and
        // so the metric is present from the first scrape rather than one
        // interval later. It is the same bounded, local-only sweep the
        // blocking pool runs afterwards.
        let published = sweep(&mut state.lock().unwrap());
        debug!(
            "{NAME}: {published} local filesystem(s) at startup; sweeping every {:?}",
            interval
        );

        Self {
            interval,
            last_read: Mutex::new(Some(Instant::now())),
            reading: Arc::new(AtomicBool::new(false)),
            state,
        }
    }
}

/// Unset every gauge in `slot` and drop its labels: the filesystem is gone.
fn vacate(slot: usize) {
    for group in GROUPS {
        let _ = group.set(slot, i64::MIN);
        group.clear_metadata(slot);
    }
}

/// Label `slot` with the identity of `mount` on every gauge group.
fn label(slot: usize, mount: &MountEntry) {
    for group in GROUPS {
        group.insert_metadata(slot, "mount".to_string(), mount.mount_point.clone());
        group.insert_metadata(slot, "fstype".to_string(), mount.fstype.clone());
        group.insert_metadata(slot, "device".to_string(), mount.device.clone());
    }
}

/// Clamp a `u64` count into the `i64` a gauge holds.
fn gauge(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// One sweep: read the mount table, reconcile slots, `statvfs` every local
/// mount, publish. Returns how many mounts published a value.
///
/// Acquisition-group bracket (principle 18): `acquire()` before the
/// mount-table read, `finish()` after the last `set()`, and `discard()` when
/// nothing could be published — a failed mount-table read, or every
/// `statvfs` failing — so the previous window stands rather than pairing a
/// failed sweep with a fresh one. A host with no local filesystem (a
/// container on an overlay root) publishes nothing and stamps nothing; it is
/// not an error, and the next sweep will see any local mount that appears.
fn sweep(state: &mut SweepState) -> usize {
    let started = Instant::now();
    let guard = FILESYSTEM_SWEEP_ACQ.acquire();

    let SweepState { slots, table } = state;
    table.clear();
    if let Err(e) = std::fs::File::open(MOUNTINFO).and_then(|mut f| f.read_to_string(table)) {
        warn!("{NAME}: could not read {MOUNTINFO}: {e}");
        guard.discard();
        return 0;
    }
    let read_done = Instant::now();

    let mounts = local_mounts(table);
    let parse_done = Instant::now();
    let devices: Vec<String> = mounts.iter().map(|m| m.device.clone()).collect();
    let assignment = slots.assign(&devices);

    for slot in &assignment.freed {
        vacate(*slot);
    }
    if assignment.dropped > 0 {
        warn!(
            "{NAME}: {} local filesystem(s) beyond the {MAX_MOUNTS}-mount cap are not sampled",
            assignment.dropped
        );
    }

    let mut published = 0;
    for mount in &mounts {
        let Some(slot) = slots.slot_of(&mount.device) else {
            continue;
        };
        if slots.relabel(slot, mount) {
            label(slot, mount);
        }
        match read_usage(&mount.mount_point, mount.id) {
            Ok(usage) => {
                let _ = FILESYSTEM_TOTAL.set(slot, gauge(usage.total_bytes));
                let _ = FILESYSTEM_FREE.set(slot, gauge(usage.free_bytes));
                let _ = FILESYSTEM_AVAILABLE.set(slot, gauge(usage.available_bytes));
                let _ = FILESYSTEM_INODES_TOTAL.set(slot, gauge(usage.total_inodes));
                let _ = FILESYSTEM_INODES_FREE.set(slot, gauge(usage.free_inodes));
                published += 1;
            }
            Err(e) => {
                // Unmounted or replaced between the table read and the call,
                // or not ours to read. Absent beats stale: unset rather than
                // keep the previous sweep's numbers under a fresh window.
                debug!("{NAME}: statvfs {} failed: {e}", mount.mount_point);
                for group in GROUPS {
                    let _ = group.set(slot, i64::MIN);
                }
            }
        }
    }

    // The bound follows the population, up and down, so a snapshot walks
    // only the slots that can hold a member. This sweep task is its only
    // writer, and the store must stay above the `finish()` below: a window
    // stamped first is a window a snapshot can read against a stale bound.
    FILESYSTEM_SWEEP_ACQ.set_member_bound(slots.bound());

    if published > 0 {
        guard.finish();
    } else {
        guard.discard();
    }
    // The off-cycle cost principle 16 asks for, as a number, by phase: the
    // mount-table read, its parse and classification, and the statvfs calls
    // plus publication.
    debug!(
        "{NAME}: sweep published {published}/{} local filesystem(s) in {} us (read {} us, parse {} us, statvfs+publish {} us)",
        mounts.len(),
        started.elapsed().as_micros(),
        (read_done - started).as_micros(),
        (parse_done - read_done).as_micros(),
        parse_done.elapsed().as_micros()
    );
    published
}

#[async_trait]
impl Sampler for Filesystem {
    fn name(&self) -> &'static str {
        NAME
    }

    async fn refresh(&self) {
        // Scoped so the lock is dropped before the dispatch below.
        {
            let mut last = self.last_read.lock().unwrap();
            match *last {
                Some(t) if t.elapsed() < self.interval => return,
                _ => *last = Some(Instant::now()),
            }
        }

        // Never overlap sweeps.
        if self.reading.swap(true, Ordering::AcqRel) {
            return;
        }

        // Off the async worker and back immediately: the sweep is syscalls,
        // however cheap. The sweep task is the acquisition group's single
        // writer; `refresh()` never acquires or finishes it.
        let state = self.state.clone();
        let reading = self.reading.clone();
        tokio::task::spawn_blocking(move || {
            sweep(&mut state.lock().unwrap());
            reading.store(false, Ordering::Release);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_are_stable_for_a_mount_that_stays_and_reused_for_one_that_goes() {
        let mut slots = Slots::new(4);
        let first = slots.assign(&["259:2".to_string(), "8:1".to_string()]);
        assert_eq!(slots.slot_of("259:2"), Some(0));
        assert_eq!(slots.slot_of("8:1"), Some(1));
        assert!(first.freed.is_empty());
        assert_eq!(slots.bound(), 2);

        // `/` (259:2) stays, the xfs disk (8:1) is unmounted, a USB stick (8:17) appears.
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
        assert_eq!(outcome.dropped, 1);
        assert_eq!(slots.slot_of("c"), None);
    }

    /// `/` as the real mount table describes it.
    fn root_mount() -> MountEntry {
        let table = std::fs::read_to_string(MOUNTINFO).unwrap();
        local_mounts(&table)
            .into_iter()
            .find(|m| m.mount_point == "/")
            .expect("/ is a local mount")
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
        // The top slot: no sweep on a real host has enough mounts to reach
        // it, so this test cannot race the ones that sweep for real.
        let slot = MAX_MOUNTS - 1;
        let mount = MountEntry {
            id: 900,
            parent: 1,
            mount_point: "/scratch".to_string(),
            fstype: "xfs".to_string(),
            source: "/dev/sdz1".to_string(),
            device: "8:401".to_string(),
        };
        label(slot, &mount);
        for group in GROUPS {
            assert!(group.set(slot, 7));
            let m = group.load_metadata(slot).expect("labels set");
            assert_eq!(m.get("mount").map(String::as_str), Some("/scratch"));
            assert_eq!(m.get("fstype").map(String::as_str), Some("xfs"));
            assert_eq!(m.get("device").map(String::as_str), Some("8:401"));
        }

        vacate(slot);
        for group in GROUPS {
            assert_eq!(group.value(slot), None, "value must read absent, not zero");
            assert!(group.load_metadata(slot).is_none_or(|m| m.is_empty()));
        }
    }

    /// The whole sweep against the real mount table: `/` is a local mount on
    /// any Linux host, so after one sweep it holds a slot, its five gauges are
    /// populated, and the group carries a stamped window.
    #[test]
    fn a_sweep_publishes_the_root_filesystem_and_stamps_the_group() {
        let mut state = SweepState::new();
        let published = sweep(&mut state);
        assert!(published >= 1, "no local filesystem published");
        let slots = &state.slots;

        let table = std::fs::read_to_string(MOUNTINFO).unwrap();
        let root = local_mounts(&table)
            .into_iter()
            .find(|m| m.mount_point == "/")
            .expect("/ is a local mount");
        let slot = slots.slot_of(&root.device).expect("/ holds a slot");
        assert!(slot < slots.bound());

        let total = FILESYSTEM_TOTAL.value(slot).expect("total set");
        let free = FILESYSTEM_FREE.value(slot).expect("free set");
        let available = FILESYSTEM_AVAILABLE.value(slot).expect("available set");
        assert!(total > 0 && free <= total && available <= free);
        assert!(FILESYSTEM_INODES_TOTAL.value(slot).is_some());
        assert!(FILESYSTEM_INODES_FREE.value(slot).is_some());
        let labels = FILESYSTEM_TOTAL.load_metadata(slot).expect("labels set");
        assert_eq!(labels.get("mount").map(String::as_str), Some("/"));

        let w = FILESYSTEM_SWEEP_ACQ
            .window()
            .expect("the sweep stamped its group");
        assert!(w.width_ns() > 0);
    }

    /// Where a sweep spends its time, phase by phase. Ignored: it prints, it
    /// does not assert. Run with
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

    /// Needs a kernel that reports `STATX_MNT_ID` (5.8+); older ones skip the
    /// check by design, and this test would fail there.
    #[test]
    fn a_path_resolving_to_a_different_mount_is_refused() {
        let root = root_mount();
        let err = read_usage("/", root.id.wrapping_add(1 << 40))
            .expect_err("a mount id mismatch must refuse");
        assert!(err.to_string().contains("now resolves to mount"), "{err}");
    }

    fn ext4_at(point: &str) -> MountEntry {
        MountEntry {
            id: 40,
            parent: 22,
            mount_point: point.to_string(),
            fstype: "ext4".to_string(),
            source: "/dev/sda1".to_string(),
            device: "8:1".to_string(),
        }
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
