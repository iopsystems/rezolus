//! Occupancy and read-only state of local filesystems on macOS, one series per
//! mounted volume: total, free, available and used bytes, total and free
//! inodes, and the read-only flag.
//!
//! ```text
//! getfsstat(MNT_NOWAIT) -> select local volumes -> assign slots
//!   -> statfs and getattrlist each, verify device -> gauges -> window
//! ```
//!
//! # Scope and blocking
//!
//! A volume is read when its mount is local (`MNT_LOCAL`), it reports a
//! nonzero size, and its type is not `devfs`, `autofs` or macFUSE. That covers
//! APFS and HFS+ volumes, the system's hidden volumes under `/System/Volumes`,
//! and mounted disk images. Network mounts are left out.
//!
//! `getfsstat(MNT_NOWAIT)` lists the mounts without calling into any
//! filesystem, but the counters it returns are the ones the mount's last
//! refresh left (a `statfs`, or a `getfsstat` without `MNT_NOWAIT`, by any
//! process), and can be hours old on a volume nothing else reads. So each
//! selected volume is read again with `statfs` and `getattrlist` on its
//! mount point, and a reading where either call reports a device other than
//! the listed one (the mount was replaced) is dropped. Each call looks the
//! path up, which walks every mount above it: a volume mounted beneath a
//! network or autofs mount is skipped, since the lookup could wait on a
//! server or trigger an automount; a macFUSE mount, and anything beneath one,
//! is skipped for the same reason. The sweep runs on the blocking pool, and
//! one still running after three intervals is logged as a warning.
//!
//! # APFS
//!
//! The volumes of an APFS container share its space. A volume without a
//! quota or reserve reports the container's size as its total and the
//! container's free space as its free and available bytes, so total minus
//! free is the space used in the whole container, not by the volume. The
//! volume's own use is `getattrlist(ATTR_VOL_SPACEUSED)`, which `df` reports
//! as used, published as `filesystem_volume_used`. HFS+ and FAT volumes
//! report it as total minus free. APFS reports an inode count that moves
//! with free space, as XFS does on Linux.
//!
//! # Cadence and publication
//!
//! The sweep runs at most once per configured interval (60s by default).
//! Each series is labeled `mount`, `fstype`, `devnum` (the `major:minor` of
//! the volume's device node, from `f_fsid`) and, for a device-backed volume,
//! `block_device`, the BSD name of that node (`disk3s5`). Volumes are keyed
//! by `devnum`; a volume mounted at two paths is read once, at the path
//! `getfsstat` lists first. Slot management, the member bound and the
//! acquisition window follow the Linux sampler (principle 18's device-sweep
//! shape). `filesystem_volume_used` is macOS-only. `filesystem_errors`,
//! `filesystem_written_bytes` and the `filesystem_tmpfs_*` families are not
//! set.

const NAME: &str = "filesystem";

const DEFAULT_READ_INTERVAL: Duration = Duration::from_secs(60);

use crate::agent::samplers::{count_change, CountChange};
use crate::agent::*;
use metriken::{metric, GaugeGroup};

use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

mod stats {
    include!("../linux/stats.rs");
}

use stats::*;

#[metric(
    name = "filesystem_volume_used",
    description = "Bytes a mounted volume itself uses (getattrlist ATTR_VOL_SPACEUSED), the number `df` reports as used. On APFS this is the volume's share of its container, which filesystem_total minus filesystem_free does not give, since every volume reports the container's size and free space; on other filesystems it equals filesystem_total minus filesystem_free. macOS only.",
    metadata = { unit = "bytes", acq_group = "filesystem_sweep" }
)]
pub static FILESYSTEM_VOLUME_USED: GaugeGroup = GaugeGroup::new(MAX_SLOTS);

/// The gauge families this sampler sets; every one is cleared when its
/// volume goes.
static GROUPS: &[&GaugeGroup] = &[
    &FILESYSTEM_TOTAL,
    &FILESYSTEM_FREE,
    &FILESYSTEM_AVAILABLE,
    &FILESYSTEM_INODES_TOTAL,
    &FILESYSTEM_INODES_FREE,
    &FILESYSTEM_READONLY,
    &FILESYSTEM_VOLUME_USED,
];

/// A slot's labels, which move at runtime as volumes are mounted and
/// unmounted.
static MOUNT_IDENTITY: metriken::group::SlotIdentity =
    metriken::group::SlotIdentity::grouped(MOUNT_IDENTITY_GROUPS);

static MOUNT_IDENTITY_GROUPS: &[&[&dyn metriken::group::SlotMetadata]] = &[&[
    &FILESYSTEM_TOTAL,
    &FILESYSTEM_FREE,
    &FILESYSTEM_AVAILABLE,
    &FILESYSTEM_INODES_TOTAL,
    &FILESYSTEM_INODES_FREE,
    &FILESYSTEM_READONLY,
    &FILESYSTEM_VOLUME_USED,
]];

/// Mount types never read: the device filesystem and automounter trigger
/// points, which hold no data, and macFUSE, whose `statfs` calls into a
/// userspace daemon that may be network-backed even when the mount is marked
/// local.
const SKIPPED_TYPES: &[&str] = &["devfs", "autofs", "macfuse", "osxfuse"];

/// A sweep still running after this many intervals is reported as stuck.
const STUCK_INTERVALS: u32 = 3;

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

/// One mounted volume as `getfsstat` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Volume {
    pub mount_point: String,
    pub fstype: String,
    /// `major:minor` of the volume's device node.
    pub devnum: String,
    /// The BSD name of the device node, for a volume mounted from `/dev`.
    pub block_device: Option<String>,
    pub local: bool,
    pub readonly: bool,
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub available_bytes: u64,
    pub total_inodes: u64,
    pub free_inodes: u64,
    /// Bytes the volume itself uses, from `getattrlist`; `None` until read,
    /// or when the filesystem does not report it.
    pub used_bytes: Option<u64>,
}

impl Volume {
    fn from_statfs(s: &libc::statfs) -> Self {
        let text = |field: &[libc::c_char]| {
            // SAFETY: the kernel fills each name with strlcpy, which
            // NUL-terminates it within its array.
            unsafe { CStr::from_ptr(field.as_ptr()) }
                .to_string_lossy()
                .into_owned()
        };
        // SAFETY: fsid_t is `{ int32_t val[2] }`; libc keeps the field private.
        let fsid: [i32; 2] = unsafe { std::mem::transmute(s.f_fsid) };
        let dev = fsid[0];
        let from = text(&s.f_mntfromname);
        let bytes = |blocks: u64| blocks.saturating_mul(u64::from(s.f_bsize));
        Volume {
            mount_point: text(&s.f_mntonname),
            fstype: text(&s.f_fstypename),
            devnum: format!("{}:{}", libc::major(dev), libc::minor(dev)),
            block_device: from.strip_prefix("/dev/").map(str::to_string),
            local: s.f_flags & libc::MNT_LOCAL as u32 != 0,
            readonly: s.f_flags & libc::MNT_RDONLY as u32 != 0,
            total_bytes: bytes(s.f_blocks),
            free_bytes: bytes(s.f_bfree),
            available_bytes: bytes(s.f_bavail),
            total_inodes: s.f_files,
            free_inodes: s.f_ffree,
            used_bytes: None,
        }
    }

    fn sampled(&self) -> bool {
        self.local && self.total_bytes != 0 && !SKIPPED_TYPES.contains(&self.fstype.as_str())
    }

    /// A mount whose path lookup can block: a network mount, an autofs
    /// trigger or a macFUSE mount.
    fn blocks_lookup(&self) -> bool {
        !self.local || ["autofs", "macfuse", "osxfuse"].contains(&self.fstype.as_str())
    }
}

/// Every mounted filesystem, with the counters each mount's last refresh
/// left.
fn mounted() -> std::io::Result<Vec<Volume>> {
    let mut room = unsafe { libc::getfsstat(std::ptr::null_mut(), 0, libc::MNT_NOWAIT) };
    loop {
        if room < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // Room for a few mounts added between the calls.
        let mut buf: Vec<libc::statfs> = vec![unsafe { std::mem::zeroed() }; room as usize + 8];
        let size = (buf.len() * std::mem::size_of::<libc::statfs>()) as libc::c_int;
        let n = unsafe { libc::getfsstat(buf.as_mut_ptr(), size, libc::MNT_NOWAIT) };
        if n < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // A full buffer may have left mounts out; ask again with more room.
        if n as usize == buf.len() {
            room = n * 2;
            continue;
        }
        buf.truncate(n as usize);
        return Ok(buf.iter().map(Volume::from_statfs).collect());
    }
}

/// Whether `path` lies beneath `mount`, a different mount point.
fn beneath(path: &str, mount: &str) -> bool {
    path != mount
        && (mount == "/"
            || path
                .strip_prefix(mount)
                .is_some_and(|rest| rest.starts_with('/')))
}

/// The volumes to sample, one per device, in mount-point order. A volume
/// beneath a mount whose lookup can block is left out.
pub fn select(volumes: Vec<Volume>) -> Vec<Volume> {
    let blocking: Vec<String> = volumes
        .iter()
        .filter(|v| v.blocks_lookup())
        .map(|v| v.mount_point.clone())
        .collect();
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<Volume> = volumes
        .into_iter()
        .filter(Volume::sampled)
        .filter(|v| !blocking.iter().any(|m| beneath(&v.mount_point, m)))
        .filter(|v| seen.insert(v.devnum.clone()))
        .collect();
    out.sort_by(|a, b| a.mount_point.cmp(&b.mount_point));
    out
}

/// `volume` read again with `statfs` on its mount point. An error when the
/// path now leads to another device, because the mount was replaced.
fn read_fresh(volume: &Volume) -> std::io::Result<Volume> {
    let path = CString::new(volume.mount_point.as_bytes())?;
    let mut s: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(path.as_ptr(), &mut s) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut fresh = Volume::from_statfs(&s);
    if fresh.devnum != volume.devnum {
        return Err(replaced(volume, &fresh.devnum));
    }
    let (dev, used) = space_used(&path)?;
    let devnum = format!("{}:{}", libc::major(dev), libc::minor(dev));
    if devnum != volume.devnum {
        return Err(replaced(volume, &devnum));
    }
    fresh.used_bytes = used;
    Ok(fresh)
}

fn replaced(volume: &Volume, devnum: &str) -> std::io::Error {
    std::io::Error::other(format!(
        "{} is now device {devnum}, not {}",
        volume.mount_point, volume.devnum
    ))
}

/// The device of the volume whose root is `path`, and the bytes the volume
/// uses (`ATTR_VOL_SPACEUSED`), or `None` when its filesystem does not
/// report them.
fn space_used(path: &CStr) -> std::io::Result<(libc::dev_t, Option<u64>)> {
    let mut request = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: libc::ATTR_CMN_RETURNED_ATTRS | libc::ATTR_CMN_DEVID,
        volattr: libc::ATTR_VOL_INFO | libc::ATTR_VOL_SPACEUSED,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    };
    let mut buf = [0u8; 64];
    // SAFETY: `request` and `buf` are valid for the call, and `buf.len()` is
    // the size passed.
    let rc = unsafe {
        libc::getattrlist(
            path.as_ptr(),
            (&raw mut request).cast(),
            buf.as_mut_ptr().cast(),
            buf.len(),
            libc::FSOPT_PACK_INVAL_ATTRS,
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    parse_space_used(&buf).ok_or_else(|| std::io::Error::other("short getattrlist reply"))
}

/// A `getattrlist` reply to `ATTR_CMN_RETURNED_ATTRS | ATTR_CMN_DEVID` and
/// `ATTR_VOL_SPACEUSED`: the length, the returned-attribute set (five
/// `u32`), the device (`i32`) and the space used (`i64`), each native-endian
/// and 4-byte aligned.
pub fn parse_space_used(buf: &[u8]) -> Option<(libc::dev_t, Option<u64>)> {
    let u32_at = |at: usize| Some(u32::from_ne_bytes(buf.get(at..at + 4)?.try_into().ok()?));
    if (u32_at(0)? as usize) < 36 {
        return None;
    }
    let commonattr = u32_at(4)?;
    let volattr = u32_at(8)?;
    if commonattr & libc::ATTR_CMN_DEVID == 0 {
        return None;
    }
    let dev = i32::from_ne_bytes(buf.get(24..28)?.try_into().ok()?);
    let used = i64::from_ne_bytes(buf.get(28..36)?.try_into().ok()?);
    let used = (volattr & libc::ATTR_VOL_SPACEUSED != 0)
        .then(|| u64::try_from(used).ok())
        .flatten();
    Some((dev, used))
}

/// Slot assignment by device: a volume keeps its slot while it stays mounted,
/// and a slot freed by an unmount is reused.
pub struct Slots {
    by_device: HashMap<String, usize>,
    occupied: Vec<Option<String>>,
    labeled: Vec<Option<(String, String, Option<String>)>>,
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

    /// Free the slots of devices not in `devices`, then place the new ones
    /// in order while a slot is free.
    pub fn assign(&mut self, devices: &[&str]) -> Assignment {
        let mut outcome = Assignment::default();
        for (slot, held) in self.occupied.iter_mut().enumerate() {
            if let Some(device) = held {
                if !devices.contains(&device.as_str()) {
                    self.by_device.remove(device);
                    *held = None;
                    self.labeled[slot] = None;
                    outcome.freed.push(slot);
                }
            }
        }
        for device in devices {
            if self.by_device.contains_key(*device) {
                continue;
            }
            match self.occupied.iter().position(Option::is_none) {
                Some(slot) => {
                    self.occupied[slot] = Some(device.to_string());
                    self.by_device.insert(device.to_string(), slot);
                }
                None => outcome.dropped += 1,
            }
        }
        outcome
    }

    pub fn slot_of(&self, device: &str) -> Option<usize> {
        self.by_device.get(device).copied()
    }

    /// Records `volume`'s labels for `slot`; true when they changed and the
    /// caller must apply them.
    pub fn relabel(&mut self, slot: usize, volume: &Volume) -> bool {
        let current = (
            volume.mount_point.clone(),
            volume.fstype.clone(),
            volume.block_device.clone(),
        );
        if self.labeled[slot].as_ref() == Some(&current) {
            return false;
        }
        self.labeled[slot] = Some(current);
        true
    }

    pub fn bound(&self) -> usize {
        self.occupied
            .iter()
            .rposition(Option::is_some)
            .map_or(0, |slot| slot + 1)
    }
}

struct SweepState {
    slots: Slots,
    dropped: usize,
}

struct Filesystem {
    interval: Duration,
    last_read: Mutex<Option<Instant>>,
    state: Arc<Mutex<SweepState>>,
    /// Set while a sweep runs on the blocking pool.
    reading: Arc<AtomicBool>,
    stuck_warned: AtomicBool,
}

impl Filesystem {
    fn new(interval: Duration) -> Self {
        // No member until the first sweep places one.
        FILESYSTEM_SWEEP_ACQ.set_member_bound(0);
        debug!("{NAME}: sweeping every {interval:?}");
        Self {
            interval,
            last_read: Mutex::new(None),
            state: Arc::new(Mutex::new(SweepState {
                slots: Slots::new(MAX_MOUNTS),
                dropped: 0,
            })),
            reading: Arc::new(AtomicBool::new(false)),
            stuck_warned: AtomicBool::new(false),
        }
    }

    /// A sweep running for more than [`STUCK_INTERVALS`] intervals is most
    /// likely waiting in a path lookup; warn once.
    fn warn_if_stuck(&self) {
        let Some(started) = *self.last_read.lock().unwrap() else {
            return;
        };
        let running = started.elapsed();
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

/// Clears the in-flight flag when the sweep returns or unwinds.
struct InFlight(Arc<AtomicBool>);

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

fn label(slot: usize, volume: &Volume) {
    let mut labels = std::collections::BTreeMap::new();
    labels.insert("mount".to_string(), volume.mount_point.clone());
    labels.insert("fstype".to_string(), volume.fstype.clone());
    labels.insert("devnum".to_string(), volume.devnum.clone());
    if let Some(name) = &volume.block_device {
        labels.insert("block_device".to_string(), name.clone());
    }
    MOUNT_IDENTITY.assign(slot, labels);
}

/// Every family reads absent for `slot`, and its labels are released.
fn vacate(slot: usize) {
    unset(slot);
    MOUNT_IDENTITY.release(slot);
}

/// Every family reads absent for `slot`; its labels stay.
fn unset(slot: usize) {
    for group in GROUPS {
        let _ = group.set(slot, i64::MIN);
    }
}

fn gauge(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// An inode gauge for a filesystem reporting `total` inodes: absent when it
/// reports none, so no consumer divides by zero.
fn inode_gauge(total: u64, value: u64) -> i64 {
    if total == 0 {
        i64::MIN
    } else {
        gauge(value)
    }
}

fn publish(slot: usize, volume: &Volume) {
    let _ = FILESYSTEM_TOTAL.set(slot, gauge(volume.total_bytes));
    let _ = FILESYSTEM_FREE.set(slot, gauge(volume.free_bytes));
    let _ = FILESYSTEM_AVAILABLE.set(slot, gauge(volume.available_bytes));
    let total = volume.total_inodes;
    let _ = FILESYSTEM_INODES_TOTAL.set(slot, inode_gauge(total, total));
    let _ = FILESYSTEM_INODES_FREE.set(slot, inode_gauge(total, volume.free_inodes));
    let _ = FILESYSTEM_READONLY.set(slot, i64::from(volume.readonly));
    let _ = FILESYSTEM_VOLUME_USED.set(slot, volume.used_bytes.map_or(i64::MIN, gauge));
}

fn report_dropped(last: usize, now: usize) {
    match count_change(last, now) {
        Some(CountChange::Began(n)) => {
            warn!("{NAME}: {n} volume(s) beyond the {MAX_MOUNTS}-mount cap are not sampled")
        }
        Some(CountChange::Moved(n)) => {
            debug!("{NAME}: {n} volume(s) beyond the {MAX_MOUNTS}-mount cap are not sampled")
        }
        Some(CountChange::Ended) => {
            info!("{NAME}: every volume fits within the {MAX_MOUNTS}-mount cap again")
        }
        None => {}
    }
}

/// One sweep over the mount table `volumes`, reading each selected volume
/// with `read`; returns how many were published.
fn sweep(
    state: &mut SweepState,
    volumes: std::io::Result<Vec<Volume>>,
    read: impl Fn(&Volume) -> std::io::Result<Volume>,
) -> usize {
    let guard = FILESYSTEM_SWEEP_ACQ.acquire();
    let SweepState { slots, dropped } = state;

    let volumes = match volumes {
        Ok(volumes) => select(volumes),
        Err(e) => {
            warn!("{NAME}: getfsstat failed: {e}");
            FILESYSTEM_SWEEP_ACQ.set_member_bound(slots.bound());
            guard.discard();
            return 0;
        }
    };

    let devices: Vec<&str> = volumes.iter().map(|v| v.devnum.as_str()).collect();
    let assignment = slots.assign(&devices);
    for slot in &assignment.freed {
        vacate(*slot);
    }
    report_dropped(*dropped, assignment.dropped);
    *dropped = assignment.dropped;

    let mut reads = Vec::with_capacity(volumes.len());
    for volume in &volumes {
        let Some(slot) = slots.slot_of(&volume.devnum) else {
            continue;
        };
        if slots.relabel(slot, volume) {
            // A new identity starts with no reading, so a failed read cannot
            // leave the previous identity's values under the new labels.
            unset(slot);
            label(slot, volume);
        }
        let read = read(volume);
        if let Err(e) = &read {
            debug!("{NAME}: statfs {} failed: {e}", volume.mount_point);
        }
        reads.push((slot, read));
    }
    // When nothing succeeded the window stays where it was, and so must the
    // values it describes; otherwise a failed slot reads absent.
    let published = reads.iter().filter(|(_, r)| r.is_ok()).count();
    for (slot, read) in &reads {
        match read {
            Ok(volume) => publish(*slot, volume),
            Err(_) if published > 0 => unset(*slot),
            Err(_) => {}
        }
    }

    // The sweep is the sole writer; the bound must be stored before finish().
    FILESYSTEM_SWEEP_ACQ.set_member_bound(slots.bound());
    if published > 0 {
        guard.finish();
    } else {
        guard.discard();
    }
    published
}

#[async_trait]
impl Sampler for Filesystem {
    fn name(&self) -> &'static str {
        NAME
    }

    async fn refresh(&self) {
        // Checked before the throttle, so a running sweep does not advance
        // `last_read`, which times it.
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

        let state = self.state.clone();
        let reading = self.reading.clone();
        tokio::task::spawn_blocking(move || {
            let _in_flight = InFlight(reading);
            let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
            let published = sweep(&mut state, mounted(), read_fresh);
            debug!("{NAME}: sweep published {published} volume(s)");
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn volume(mount_point: &str, fstype: &str, devnum: &str) -> Volume {
        Volume {
            mount_point: mount_point.to_string(),
            fstype: fstype.to_string(),
            devnum: devnum.to_string(),
            block_device: Some(format!("disk{devnum}")),
            local: true,
            readonly: false,
            total_bytes: 1 << 30,
            free_bytes: 1 << 29,
            available_bytes: 1 << 29,
            total_inodes: 100,
            free_inodes: 50,
            used_bytes: Some(1 << 28),
        }
    }

    #[test]
    fn select_keeps_local_volumes_with_space_once_each_in_mount_order() {
        let mut network = volume("/Volumes/share", "nfs", "26:6");
        network.local = false;
        let mut empty = volume("/empty", "apfs", "1:9");
        empty.total_bytes = 0;
        let picked = select(vec![
            volume("/System/Volumes/Data", "apfs", "1:18"),
            volume("/dev", "devfs", "83:206"),
            volume("/System/Volumes/Data/home", "autofs", "53:3"),
            network,
            empty,
            volume("/", "apfs", "1:19"),
            // The same device mounted a second time.
            volume("/Volumes/again", "apfs", "1:18"),
        ]);
        let mounts: Vec<&str> = picked.iter().map(|v| v.mount_point.as_str()).collect();
        assert_eq!(mounts, ["/", "/System/Volumes/Data"]);
    }

    /// A volume whose mount point lies beneath a network or autofs mount is
    /// left out, since looking its path up walks that mount; one beside it is
    /// not.
    #[test]
    fn select_skips_a_volume_beneath_a_mount_whose_lookup_can_block() {
        let mut share = volume("/Volumes/share", "nfs", "26:6");
        share.local = false;
        let home = volume("/System/Volumes/Data/home", "autofs", "53:3");
        let picked = select(vec![
            volume("/", "apfs", "1:19"),
            share,
            home,
            volume("/Volumes/share/image", "apfs", "1:40"),
            volume("/System/Volumes/Data/home/user/disk", "hfs", "1:41"),
            volume("/Volumes/shared", "apfs", "1:42"),
        ]);
        let mounts: Vec<&str> = picked.iter().map(|v| v.mount_point.as_str()).collect();
        assert_eq!(mounts, ["/", "/Volumes/shared"]);
    }

    #[test]
    fn slots_are_stable_for_a_volume_that_stays_and_reused_for_one_that_goes() {
        let mut slots = Slots::new(2);
        assert_eq!(slots.assign(&["1:18", "1:19"]), Assignment::default());
        assert_eq!(slots.slot_of("1:18"), Some(0));
        assert_eq!(slots.slot_of("1:19"), Some(1));

        let a = slots.assign(&["1:19", "1:30"]);
        assert_eq!(a.freed, [0]);
        assert_eq!(slots.slot_of("1:19"), Some(1));
        assert_eq!(slots.slot_of("1:30"), Some(0));

        let a = slots.assign(&["1:19", "1:30", "1:31"]);
        assert_eq!(a.dropped, 1);
        assert_eq!(slots.slot_of("1:31"), None);
        assert_eq!(slots.bound(), 2);
    }

    #[test]
    fn relabel_reports_a_change_once() {
        let mut slots = Slots::new(1);
        slots.assign(&["1:18"]);
        let v = volume("/System/Volumes/Data", "apfs", "1:18");
        assert!(slots.relabel(0, &v));
        assert!(!slots.relabel(0, &v));
        let moved = volume("/Volumes/Data", "apfs", "1:18");
        assert!(slots.relabel(0, &moved));
    }

    /// The live mount table: the root volume is read, with its labels, and
    /// its bytes add up.
    #[test]
    fn the_root_volume_is_read() {
        let volumes = select(mounted().expect("getfsstat"));
        let root = volumes
            .iter()
            .find(|v| v.mount_point == "/")
            .expect("the root volume is sampled");
        assert!(root.total_bytes > 0);
        assert!(root.free_bytes <= root.total_bytes);
        assert!(root.available_bytes <= root.free_bytes);
        assert!(volumes.iter().all(|v| v.fstype != "devfs" && v.local));
        assert!(volumes
            .iter()
            .any(|v| v.mount_point == "/System/Volumes/Data"));

        // devnum is the device node's own number.
        use std::os::unix::fs::MetadataExt;
        let node = format!(
            "/dev/{}",
            root.block_device.as_deref().expect("block device")
        );
        let rdev = std::fs::metadata(&node).unwrap().rdev() as libc::dev_t;
        assert_eq!(
            root.devnum,
            format!("{}:{}", libc::major(rdev), libc::minor(rdev))
        );

        let fresh = read_fresh(root).expect("statfs /");
        assert_eq!(fresh.devnum, root.devnum);
        assert!(fresh.readonly, "the sealed system volume is read-only");

        // The system volume's own use is part of its container's, which also
        // holds the Data volume.
        let used = fresh.used_bytes.expect("space used on /");
        assert!(used > 0);
        assert!(
            used < fresh.total_bytes - fresh.free_bytes,
            "{used} {fresh:?}"
        );
        let data = volumes
            .iter()
            .find(|v| v.mount_point == "/System/Volumes/Data")
            .unwrap();
        assert!(read_fresh(data).unwrap().used_bytes.is_some());
    }

    fn reply(commonattr: u32, volattr: u32, dev: i32, used: i64) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend(36u32.to_ne_bytes());
        for attr in [commonattr, volattr, 0, 0, 0] {
            buf.extend(attr.to_ne_bytes());
        }
        buf.extend(dev.to_ne_bytes());
        buf.extend(used.to_ne_bytes());
        buf
    }

    #[test]
    fn a_getattrlist_reply_parses_to_device_and_space_used() {
        let common = libc::ATTR_CMN_RETURNED_ATTRS | libc::ATTR_CMN_DEVID;
        let vol = libc::ATTR_VOL_INFO | libc::ATTR_VOL_SPACEUSED;
        assert_eq!(
            parse_space_used(&reply(common, vol, 0x0100_0012, 4096)),
            Some((0x0100_0012, Some(4096)))
        );
        // A filesystem that does not return the space used.
        assert_eq!(
            parse_space_used(&reply(common, libc::ATTR_VOL_INFO, 7, 0)),
            Some((7, None))
        );
        // No device, a short reply or a negative size is not trusted.
        assert_eq!(parse_space_used(&reply(0, vol, 7, 1)), None);
        assert_eq!(parse_space_used(&reply(common, vol, 7, 1)[..30]), None);
        assert_eq!(
            parse_space_used(&reply(common, vol, 7, -1)),
            Some((7, None))
        );
    }

    /// A mount point that now leads to another device is not read.
    #[test]
    fn read_fresh_refuses_a_replaced_mount() {
        assert!(read_fresh(&volume("/", "apfs", "0:0")).is_err());
    }

    /// A sweep publishes each volume with its labels, stamps the window and
    /// bounds the group; a later sweep without one volume clears its slot.
    #[test]
    fn a_sweep_publishes_and_a_later_sweep_clears_a_volume_that_went() {
        let mut state = SweepState {
            slots: Slots::new(MAX_MOUNTS),
            dropped: 0,
        };
        let data = volume("/System/Volumes/Data", "apfs", "1:18");
        let root = volume("/", "apfs", "1:19");
        let fresh = |v: &Volume| Ok(v.clone());
        assert_eq!(
            sweep(&mut state, Ok(vec![data.clone(), root.clone()]), fresh),
            2
        );
        assert_eq!(FILESYSTEM_SWEEP_ACQ.member_bound(), Some(2));
        assert!(FILESYSTEM_SWEEP_ACQ.window().is_some());
        let slot = state.slots.slot_of("1:18").unwrap();
        assert_eq!(FILESYSTEM_TOTAL.value(slot), Some(1 << 30));
        assert_eq!(FILESYSTEM_INODES_FREE.value(slot), Some(50));
        assert_eq!(FILESYSTEM_VOLUME_USED.value(slot), Some(1 << 28));

        assert_eq!(sweep(&mut state, Ok(vec![root.clone()]), fresh), 1);
        assert_eq!(FILESYSTEM_TOTAL.value(slot), None);
        assert_eq!(FILESYSTEM_VOLUME_USED.value(slot), None);

        // The sweep publishes what `read` returns, not the listed counters.
        let root_slot = state.slots.slot_of("1:19").unwrap();
        let newer = |v: &Volume| {
            let mut v = v.clone();
            v.available_bytes = 7;
            v.used_bytes = None;
            Ok(v)
        };
        assert_eq!(sweep(&mut state, Ok(vec![root.clone()]), newer), 1);
        assert_eq!(FILESYSTEM_AVAILABLE.value(root_slot), Some(7));
        // A volume that does not report its use reads absent.
        assert_eq!(FILESYSTEM_VOLUME_USED.value(root_slot), None);

        // A sweep whose every read failed keeps the values and the window.
        let window = FILESYSTEM_SWEEP_ACQ.window();
        let failed = |_: &Volume| Err(std::io::Error::other("gone"));
        assert_eq!(sweep(&mut state, Ok(vec![root.clone()]), failed), 0);
        assert_eq!(FILESYSTEM_AVAILABLE.value(root_slot), Some(7));
        assert_eq!(FILESYSTEM_SWEEP_ACQ.window(), window);

        // A failed read beside a successful one reads absent.
        let root_fails = |v: &Volume| {
            if v.devnum == "1:19" {
                Err(std::io::Error::other("gone"))
            } else {
                Ok(v.clone())
            }
        };
        assert_eq!(sweep(&mut state, Ok(vec![root, data]), root_fails), 1);
        assert_eq!(FILESYSTEM_AVAILABLE.value(root_slot), None);
        assert_eq!(FILESYSTEM_VOLUME_USED.value(root_slot), None);
    }
}
