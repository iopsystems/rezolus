//! Filesystem slots for per-filesystem BPF counters.
//!
//! A BPF hook inside a filesystem has the device in hand (`dev_t`, from the
//! tracepoint argument or `inode->i_sb->s_dev`) and nothing else that names
//! the filesystem. A `dev_t` is a sparse 32-bit key, so it cannot index a
//! counter bank directly; a slot can. This module owns the `dev_t → slot`
//! assignment, one table for every sampler that attributes by filesystem, so
//! two samplers on one host agree on which slot is which mount and the mount
//! table is parsed once, not once per sampler.
//!
//! # Who assigns
//!
//! Userspace, from `/proc/self/mountinfo`, which is where the labels are. The
//! alternative — BPF assigning a slot on first sight of a device and
//! announcing it over a ringbuf, as cgroups do — needs a compare-and-swap
//! against concurrent first sights and still ends up reading the mount table
//! for the labels. Assigning here keeps the BPF side to one lookup and puts
//! the writes on a path that runs once per rescan.
//!
//! The lookup map is a `BPF_MAP_TYPE_HASH` keyed by `dev_t`. Principle 5
//! (`docs/principles.md`) prefers arrays for bounded integer keys; a `dev_t`
//! is not one (major in the high 12 bits, minor in the low 20), and the
//! principle's objection to hash maps — update contention on a hot path —
//! does not arise here because BPF never writes it: userspace inserts and
//! deletes on rescan, BPF reads lock-free.
//!
//! # Slot 0 is "other"
//!
//! Every event lands in some slot, so the per-filesystem sums are complete:
//! slot 0 takes a device the table does not know (a mount younger than the
//! last rescan, one beyond the cap, a jbd2 client that is not ext4) and is
//! labeled `mount="other"`. Movement in slot 0 is how a new mount is noticed
//! between rescans: the reader asks for a rescan on the next refresh, so a
//! mount is attributed within one refresh of its first event rather than at
//! the next [`RESCAN_INTERVAL`].
//!
//! # Stability
//!
//! A slot belongs to a device for as long as it stays mounted; the same
//! device is never moved. A device that goes away frees its slot and the
//! slot's counters are zeroed before another device takes it, so the next
//! occupant starts from zero (its identity is a new uid to subscribers either
//! way, via [`SlotIdentity`](crate::agent::identity::SlotIdentity)).
//!
//! # Which mounts
//!
//! Every mount whose type ext4 serves or jbd2 journals (`ext4`, `ext3`,
//! `ext2`, `ocfs2`), deduplicated by device: a bind mount is the same
//! filesystem, and the shortest mount point names it. No path lookup and no
//! `statvfs`, so nothing here can block on a device or a server; the
//! `filesystem` sampler's local-only policy is about lookups, and does not
//! apply to a text parse.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::{debug, info, warn};

use crate::agent::samplers::filesystem::mounts::{parse_mountinfo, MountEntry};
use crate::agent::MAX_FILESYSTEMS;

const MOUNTINFO: &str = "/proc/self/mountinfo";
const SYS_DEV_BLOCK: &str = "/sys/dev/block";

/// How often the mount table is re-read when nothing asks sooner. Slot 0
/// movement asks sooner; this bounds how long an unmount takes to free a slot.
const RESCAN_INTERVAL: Duration = Duration::from_secs(10);

/// Filesystem types ext4 serves (the ext4 driver mounts ext2 and ext3) or
/// jbd2 journals (ocfs2). A jbd2 event from an ocfs2 mount is attributed to
/// that mount, labeled with its own `fstype`, rather than folded into "other".
const FSTYPES: &[&str] = &["ext4", "ext3", "ext2", "ocfs2"];

/// The label slot 0 carries.
pub const OTHER: &str = "other";

/// One filesystem holding a slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Filesystem {
    /// The kernel's `dev_t` for the block device: what the BPF hooks see.
    pub dev: u32,
    /// `major:minor`, as the mount table and the `filesystem` sampler spell it.
    pub devnum: String,
    /// The shortest mount point of the filesystem.
    pub mount: String,
    pub fstype: String,
    /// The kernel's name for the block device (`nvme0n1p5`, `dm-0`), when
    /// `/sys/dev/block` has it.
    pub block_device: Option<String>,
}

impl Filesystem {
    /// The labels the `filesystem` sampler publishes for the same mount, so
    /// the two join on `devnum` and `mount`.
    pub fn labels(&self) -> BTreeMap<String, String> {
        let mut labels = BTreeMap::new();
        labels.insert("mount".to_string(), self.mount.clone());
        labels.insert("fstype".to_string(), self.fstype.clone());
        labels.insert("devnum".to_string(), self.devnum.clone());
        if let Some(name) = &self.block_device {
            labels.insert("block_device".to_string(), name.clone());
        }
        labels
    }
}

/// Which filesystem holds which slot, as of one scan. Index is the slot;
/// slot 0 is never assigned (it is "other").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Assignment {
    /// Bumped on every change. A reader that applied generation N has
    /// nothing to do while it stays N.
    pub generation: u64,
    pub slots: Vec<Option<Filesystem>>,
}

impl Default for Assignment {
    fn default() -> Self {
        Self {
            generation: 0,
            slots: vec![None; MAX_FILESYSTEMS],
        }
    }
}

impl Assignment {
    /// The slots a reader publishes: 0 and every occupied one.
    pub fn members(&self) -> Vec<usize> {
        std::iter::once(0)
            .chain(
                self.slots
                    .iter()
                    .enumerate()
                    .filter_map(|(slot, held)| held.as_ref().map(|_| slot)),
            )
            .collect()
    }
}

struct Registry {
    assignment: Arc<Assignment>,
    by_dev: HashMap<u32, usize>,
    last_scan: Option<Instant>,
    rescan_requested: bool,
    /// Filesystems over the cap at the last scan, so the warning fires on change.
    dropped: usize,
}

impl Registry {
    const fn new() -> Self {
        Self {
            assignment: Arc::new(Assignment {
                generation: 0,
                slots: Vec::new(),
            }),
            by_dev: HashMap::new(),
            last_scan: None,
            rescan_requested: false,
            dropped: 0,
        }
    }
}

static REGISTRY: Mutex<Registry> = Mutex::new(Registry::new());

/// The current assignment, rescanning the mount table first when one is due:
/// [`RESCAN_INTERVAL`] has passed, [`request_rescan`] was called, or this is
/// the first call.
pub fn current() -> Arc<Assignment> {
    let mut registry = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    if registry.assignment.slots.is_empty() {
        registry.assignment = Arc::new(Assignment::default());
    }
    let due = registry.rescan_requested
        || registry
            .last_scan
            .is_none_or(|t| t.elapsed() >= RESCAN_INTERVAL);
    if due {
        registry.rescan_requested = false;
        registry.last_scan = Some(Instant::now());
        match std::fs::read_to_string(MOUNTINFO) {
            Ok(table) => {
                assign(&mut registry, &table, |devnum| {
                    block_device_name(Path::new(SYS_DEV_BLOCK), devnum)
                });
            }
            Err(e) => warn!("filesystem slots: could not read {MOUNTINFO}: {e}"),
        }
    }
    registry.assignment.clone()
}

/// Ask for the mount table to be re-read on the next [`current`] call. The
/// slotted-counter reader calls this when slot 0 moved: some device the table
/// does not know is producing events.
pub fn request_rescan() {
    REGISTRY
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .rescan_requested = true;
}

/// The kernel's `dev_t` for a `major:minor` string: `MKDEV` packs the major
/// above 20 bits of minor. This is the value a tracepoint's `dev_t` argument
/// and `super_block::s_dev` carry, and the one the BPF lookup is keyed by.
pub fn kernel_dev(devnum: &str) -> Option<u32> {
    let (major, minor) = devnum.split_once(':')?;
    let major: u32 = major.parse().ok()?;
    let minor: u32 = minor.parse().ok()?;
    if major >= 1 << 12 || minor >= 1 << 20 {
        return None;
    }
    Some((major << 20) | minor)
}

/// The kernel's name for block device `devnum` from its `/sys/dev/block` link;
/// `None` for an anonymous device.
fn block_device_name(sys_dev_block: &Path, devnum: &str) -> Option<String> {
    let target = std::fs::read_link(sys_dev_block.join(devnum)).ok()?;
    target.file_name()?.to_str().map(str::to_string)
}

/// The mounts that get slots: one per device among [`FSTYPES`], under its
/// shortest mount point.
fn candidates(table: &str) -> Vec<MountEntry> {
    let mut by_device: BTreeMap<String, MountEntry> = BTreeMap::new();
    for mount in parse_mountinfo(table) {
        if !FSTYPES.contains(&mount.fstype.as_str()) {
            continue;
        }
        match by_device.get(&mount.device) {
            Some(held)
                if (held.mount_point.len(), &held.mount_point)
                    <= (mount.mount_point.len(), &mount.mount_point) => {}
            _ => {
                by_device.insert(mount.device.clone(), mount);
            }
        }
    }
    by_device.into_values().collect()
}

/// Re-derive the assignment from `table`. Devices already holding a slot keep
/// it; devices that left free theirs; new devices take the lowest free slot
/// above 0, in mount-point order. Returns whether anything changed.
fn assign(
    registry: &mut Registry,
    table: &str,
    block_device_name: impl Fn(&str) -> Option<String>,
) -> bool {
    let mut candidates = candidates(table);
    candidates.sort_by(|a, b| a.mount_point.cmp(&b.mount_point));

    let mut slots = registry.assignment.slots.clone();
    let mut by_dev = registry.by_dev.clone();
    let mut changed = false;

    // Free the slots of devices no longer mounted.
    let present: Vec<u32> = candidates
        .iter()
        .filter_map(|m| kernel_dev(&m.device))
        .collect();
    for (dev, slot) in registry.by_dev.iter() {
        if !present.contains(dev) {
            slots[*slot] = None;
            by_dev.remove(dev);
            changed = true;
        }
    }

    let mut dropped = 0;
    for mount in &candidates {
        let Some(dev) = kernel_dev(&mount.device) else {
            continue;
        };
        let filesystem = Filesystem {
            dev,
            devnum: mount.device.clone(),
            mount: mount.mount_point.clone(),
            fstype: mount.fstype.clone(),
            block_device: block_device_name(&mount.device),
        };
        match by_dev.get(&dev) {
            Some(&slot) => {
                // Same device: keep the slot, refresh the labels if the
                // selected mount point moved.
                if slots[slot].as_ref() != Some(&filesystem) {
                    slots[slot] = Some(filesystem);
                    changed = true;
                }
            }
            None => match slots.iter().skip(1).position(Option::is_none) {
                Some(free) => {
                    let slot = free + 1;
                    slots[slot] = Some(filesystem);
                    by_dev.insert(dev, slot);
                    changed = true;
                }
                None => dropped += 1,
            },
        }
    }

    if dropped != registry.dropped {
        if dropped > 0 {
            warn!(
                "filesystem slots: {dropped} filesystem(s) beyond the {MAX_FILESYSTEMS}-slot cap \
                 are counted as \"{OTHER}\""
            );
        } else {
            info!("filesystem slots: every filesystem fits within the {MAX_FILESYSTEMS}-slot cap again");
        }
        registry.dropped = dropped;
    }

    if changed {
        let generation = registry.assignment.generation + 1;
        registry.assignment = Arc::new(Assignment { generation, slots });
        registry.by_dev = by_dev;
        debug!(
            "filesystem slots: generation {generation}, {} filesystem(s) assigned",
            registry.by_dev.len()
        );
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE: &str = "\
22 1 259:3 / / rw,relatime shared:1 - ext4 /dev/nvme0n1p5 rw,errors=remount-ro
23 22 0:5 / /proc rw,nosuid shared:2 - proc proc rw
24 22 259:3 /home /mnt/home-bind rw,relatime shared:1 - ext4 /dev/nvme0n1p5 rw
25 22 8:17 / /data rw,relatime shared:3 - ext4 /dev/sdb1 rw
26 22 8:33 / /scratch rw,relatime shared:4 - xfs /dev/sdc1 rw
27 22 253:0 / /var/lib/cluster rw shared:5 - ocfs2 /dev/dm-0 rw
";

    fn fresh() -> Registry {
        let mut r = Registry::new();
        r.assignment = Arc::new(Assignment::default());
        r
    }

    fn names(devnum: &str) -> Option<String> {
        match devnum {
            "259:3" => Some("nvme0n1p5".to_string()),
            "8:17" => Some("sdb1".to_string()),
            _ => None,
        }
    }

    #[test]
    fn kernel_dev_packs_major_above_twenty_bits_of_minor() {
        assert_eq!(kernel_dev("259:3"), Some((259 << 20) | 3));
        assert_eq!(kernel_dev("8:17"), Some((8 << 20) | 17));
        assert_eq!(kernel_dev("0:5"), Some(5));
        assert_eq!(kernel_dev("garbage"), None);
        assert_eq!(kernel_dev("4096:0"), None, "major has 12 bits");
    }

    #[test]
    fn ext4_family_and_ocfs2_get_slots_once_per_device_in_mount_order() {
        let mut r = fresh();
        assert!(assign(&mut r, TABLE, names));
        let a = r.assignment.clone();
        assert_eq!(a.generation, 1);
        let occupied: Vec<(usize, &str, &str)> = a
            .slots
            .iter()
            .enumerate()
            .filter_map(|(s, f)| f.as_ref().map(|f| (s, f.mount.as_str(), f.fstype.as_str())))
            .collect();
        // Mount-point order; the bind alias of the root device is folded in
        // (shortest mount point wins); xfs is not a candidate.
        assert_eq!(
            occupied,
            vec![
                (1, "/", "ext4"),
                (2, "/data", "ext4"),
                (3, "/var/lib/cluster", "ocfs2"),
            ]
        );
        assert_eq!(a.members(), vec![0, 1, 2, 3]);
        let root = a.slots[1].as_ref().unwrap();
        assert_eq!(root.dev, (259 << 20) | 3);
        assert_eq!(root.devnum, "259:3");
        assert_eq!(root.block_device.as_deref(), Some("nvme0n1p5"));
        assert_eq!(a.slots[3].as_ref().unwrap().block_device, None);
        let labels = root.labels();
        assert_eq!(labels.get("mount").map(String::as_str), Some("/"));
        assert_eq!(labels.get("fstype").map(String::as_str), Some("ext4"));
        assert_eq!(labels.get("devnum").map(String::as_str), Some("259:3"));
        assert_eq!(
            labels.get("block_device").map(String::as_str),
            Some("nvme0n1p5")
        );

        // The same table again changes nothing.
        assert!(!assign(&mut r, TABLE, names));
        assert_eq!(r.assignment.generation, 1);
    }

    #[test]
    fn a_device_keeps_its_slot_while_others_come_and_go() {
        let mut r = fresh();
        assign(&mut r, TABLE, names);
        // /data unmounts; a new device appears at a path that sorts first.
        let table = TABLE.replace(
            "25 22 8:17 / /data rw,relatime shared:3 - ext4 /dev/sdb1 rw\n",
            "",
        ) + "28 22 8:49 / /a rw,relatime shared:6 - ext4 /dev/sdd1 rw\n";
        assert!(assign(&mut r, &table, names));
        let a = r.assignment.clone();
        assert_eq!(a.generation, 2);
        assert_eq!(a.slots[1].as_ref().unwrap().mount, "/", "root kept slot 1");
        assert_eq!(
            a.slots[2].as_ref().unwrap().mount,
            "/a",
            "the freed slot is reused by the newcomer"
        );
        assert_eq!(
            a.slots[3].as_ref().unwrap().mount,
            "/var/lib/cluster",
            "ocfs2 kept slot 3"
        );
    }

    #[test]
    fn a_moved_mount_point_relabels_without_moving_the_slot() {
        let mut r = fresh();
        assign(&mut r, TABLE, names);
        let table = TABLE.replace("/data ", "/srv/data ");
        assert!(assign(&mut r, &table, names));
        assert_eq!(r.assignment.slots[2].as_ref().unwrap().mount, "/srv/data");
        assert_eq!(r.assignment.generation, 2);
    }

    #[test]
    fn filesystems_past_the_cap_are_dropped_and_counted() {
        let mut r = fresh();
        let mut table = String::new();
        for i in 0..(MAX_FILESYSTEMS + 5) {
            table.push_str(&format!(
                "{id} 1 8:{minor} / /m{i:03} rw shared:1 - ext4 /dev/sd{i} rw\n",
                id = 100 + i,
                minor = i
            ));
        }
        assert!(assign(&mut r, &table, |_| None));
        assert_eq!(
            r.assignment.members().len(),
            MAX_FILESYSTEMS,
            "slot 0 plus 63"
        );
        assert_eq!(r.dropped, 6);
    }

    #[test]
    fn a_default_assignment_publishes_only_other() {
        assert_eq!(Assignment::default().members(), vec![0]);
    }
}
