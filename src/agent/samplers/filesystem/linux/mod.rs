//! Occupancy of local filesystems, sampled from `/proc/self/mountinfo` and
//! `fstatvfs`: total, free and available bytes, plus total and free inodes.
//!
//! ```text
//! mountinfo -> classify / filter covered mounts -> assign slots
//!           -> open / verify mount id / fstatvfs -> gauges -> window
//! ```
//!
//! # Scope and blocking
//!
//! [`mounts::local_mounts`] selects `/dev`-backed filesystems and ZFS,
//! excluding known network types, FUSE, autofs and pseudo-filesystems.
//! Network `statvfs` calls can block on an unavailable server; autofs path
//! lookup can trigger a mount. Classification must precede path lookup.
//! Mount-id validation detects replacement after discovery when the kernel
//! supplies `STATX_MNT_ID`, but cannot prevent a lookup from blocking if a
//! network mount is stacked over the path between discovery and `open`.
//! Such a lookup blocks startup during the initial sweep, or leaves later
//! sweeps skipped while the blocking task remains in flight.
//!
//! Network support is deferred in `docs/backlog.md`; any opt-in must bound
//! both blocking workers and per-mount wait time.
//!
//! # Cadence and publication
//!
//! Superblock counters have no BPF or perf hook, so the sweep reads procfs
//! and `fstatvfs`: a principle 15 exception (`docs/principles.md`).
//! Rescanning the table each sweep discovers mounts added after startup. The initial sweep runs
//! inline; consumer-driven `refresh()` dispatches subsequent sweeps to the
//! blocking pool, at most once per configured interval (60s by default).
//!
//! Sweeps never overlap. Each sweep brackets discovery and all five gauge
//! groups with one acquisition window, stamped after successful publication;
//! an empty or failed sweep leaves the previous window unchanged. This is
//! principle 18's device-sweep shape. [`Slots`] owns membership and labels;
//! the member bound limits snapshot traversal to slots below the highest
//! occupied one, where vacant slots read as absent.
//! Window publication does not make values, labels and membership atomic.
//!
//! Phase measurements and scale limits live in
//! `docs/journal/2026-09-12-filesystem-sampler.md`.

const NAME: &str = "filesystem";

// 60s matches drivehealth; measured sweep cost is recorded in the journal.
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

// Every published gauge family must participate in slot clearing and relabeling.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Usage {
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub available_bytes: u64,
    pub total_inodes: u64,
    pub free_inodes: u64,
}

/// Read a directory mount selected by [`mounts::local_mounts`].
///
/// Callers must classify and filter covered mounts before this path lookup.
/// Rejects a mount-id mismatch when `STATX_MNT_ID` is available; kernels
/// before 5.8 lack that check. Lookup itself has no deadline.
pub fn read_usage(path: &str, mount_id: u64) -> std::io::Result<Usage> {
    let c_path = CString::new(path)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "NUL in path"))?;
    let raw = unsafe {
        libc::open(
            c_path.as_ptr(),
            libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
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

    pub fn assign(&mut self, devices: &[String]) -> Assignment {
        let mut outcome = Assignment::default();

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
    interval: Duration,
    last_read: Mutex<Option<Instant>>,
    reading: Arc<AtomicBool>,
    state: Arc<Mutex<SweepState>>,
}

impl Filesystem {
    fn new(interval: Duration) -> Self {
        let state = Arc::new(Mutex::new(SweepState::new()));

        // Initialize membership and readings before the first snapshot walk.
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

fn vacate(slot: usize) {
    for group in GROUPS {
        // Must use the absent sentinel, not zero, when a filesystem leaves.
        let _ = group.set(slot, i64::MIN);
        group.clear_metadata(slot);
    }
}

fn label(slot: usize, mount: &MountEntry) {
    for group in GROUPS {
        group.insert_metadata(slot, "mount".to_string(), mount.mount_point.clone());
        group.insert_metadata(slot, "fstype".to_string(), mount.fstype.clone());
        group.insert_metadata(slot, "device".to_string(), mount.device.clone());
    }
}

fn gauge(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// Returns the number of mounts whose readings were published.
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

    // Reused slots must be cleared before any new labels or readings are published.
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
                // Failed reads must not retain old values under this sweep's window.
                debug!("{NAME}: statvfs {} failed: {e}", mount.mount_point);
                for group in GROUPS {
                    let _ = group.set(slot, i64::MIN);
                }
            }
        }
    }

    // The sweep is the sole writer; the bound must be stored before finish().
    FILESYSTEM_SWEEP_ACQ.set_member_bound(slots.bound());

    // A sweep with no published readings must not advance the acquisition window.
    if published > 0 {
        guard.finish();
    } else {
        guard.discard();
    }
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

        // Only the blocking task may stamp the group; refresh must not stamp it.
        let state = self.state.clone();
        let reading = self.reading.clone();
        tokio::task::spawn_blocking(move || {
            sweep(&mut state.lock().unwrap());
            // The in-flight latch must remain set until the blocking sweep returns.
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

    // Requires a root filesystem accepted by the local-mount classifier.
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
        // Avoids real-sweep slots only on hosts with fewer than MAX_MOUNTS filesystems.
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

    /// Requires a local root mount; verifies its gauges, labels and acquisition window.
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

    /// Requires STATX_MNT_ID support (Linux 5.8+) and a local root mount.
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
