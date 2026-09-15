//! Parse and classify `/proc/self/mountinfo` without resolving mount paths.
//!
//! See the parent module for the sampler's local-only policy. Visibility
//! filtering retains the full table, including excluded filesystem types:
//! an excluded mount can cover a local one. Device deduplication runs after
//! filtering, so a covered alias does not displace an uncovered one.

use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountEntry {
    /// Mount-table id, comparable to statx.stx_mnt_id.
    pub id: u64,
    /// Parent mount id; the parent can lie outside the process root and table.
    pub parent: u64,
    /// Mount path relative to the process root, with octal escapes decoded.
    pub mount_point: String,
    pub fstype: String,
    /// Filesystem-specific source, such as a device path, export or ZFS dataset.
    pub source: String,
    /// major:minor device id used to deduplicate bind aliases.
    pub device: String,
    /// Read-only as a whole: a leading `ro` superblock option, or ext4's
    /// `emergency_ro`, which current kernels set after an error instead of the
    /// superblock flag. Per-mount options describe one path, not the filesystem.
    pub readonly: bool,
}

/// Network or clustered filesystems whose statvfs can wait on remote state.
const NETWORK_FSTYPES: &[&str] = &[
    "nfs",
    "nfs4",
    "cifs",
    "smb3",
    "ceph",
    "glusterfs",
    "afs",
    "9p",
    "lustre",
    "ocfs2",
    "gfs2",
    "davfs",
    "ncpfs",
    "coda",
];

/// Excluded by scope: pseudo-filesystems, RAM storage, overlays and read-only images.
/// Autofs lookup can trigger a mount; squashfs reports full capacity by construction.
const PSEUDO_FSTYPES: &[&str] = &[
    "autofs",
    "tmpfs",
    "devtmpfs",
    "ramfs",
    "overlay",
    "squashfs",
    "iso9660",
    "udf",
    "proc",
    "sysfs",
    "cgroup",
    "cgroup2",
    "devpts",
    "mqueue",
    "hugetlbfs",
    "debugfs",
    "tracefs",
    "securityfs",
    "pstore",
    "efivarfs",
    "bpf",
    "configfs",
    "fusectl",
    "binfmt_misc",
    "nsfs",
    "rpc_pipefs",
    "selinuxfs",
];

/// ZFS identifies its source by pool or dataset name, not a /dev path.
const LOCAL_FSTYPES_WITHOUT_DEV_SOURCE: &[&str] = &["zfs"];

impl MountEntry {
    /// Local-only policy gate; does not establish path visibility or a read deadline.
    pub fn is_local(&self) -> bool {
        let fstype = self.fstype.as_str();
        if NETWORK_FSTYPES.contains(&fstype) || PSEUDO_FSTYPES.contains(&fstype) {
            return false;
        }
        // FUSE must remain excluded even with a /dev source: its daemon can block.
        if fstype == "fuse" || fstype == "fuseblk" || fstype.starts_with("fuse.") {
            return false;
        }
        self.source.starts_with("/dev/") || LOCAL_FSTYPES_WITHOUT_DEV_SOURCE.contains(&fstype)
    }

    /// A path lookup through this mount can wait on a server, a userspace
    /// daemon or an automount: network types, FUSE and autofs.
    pub fn lookup_can_block(&self) -> bool {
        let fstype = self.fstype.as_str();
        NETWORK_FSTYPES.contains(&fstype)
            || fstype == "autofs"
            || fstype == "fuse"
            || fstype == "fuseblk"
            || fstype.starts_with("fuse.")
    }
}

/// Unparseable lines are skipped.
///
/// proc_pid_mountinfo(5): `id parent major:minor root mount_point options
/// [optional fields...] - fstype source super_options`.
pub fn parse_mountinfo(text: &str) -> Vec<MountEntry> {
    text.lines().filter_map(parse_line).collect()
}

fn parse_line(line: &str) -> Option<MountEntry> {
    let fields: Vec<&str> = line.split(' ').collect();
    let sep = fields.iter().position(|f| *f == "-")?;
    if sep < 6 || fields.len() < sep + 4 {
        return None;
    }
    Some(MountEntry {
        id: fields[0].parse().ok()?,
        parent: fields[1].parse().ok()?,
        device: fields[2].to_string(),
        mount_point: unescape(fields[4]),
        fstype: fields[sep + 1].to_string(),
        source: unescape(fields[sep + 2]),
        readonly: {
            let mut options = fields[sep + 3].split(',');
            options.next() == Some("ro") || options.any(|option| option == "emergency_ro")
        },
    })
}

/// Kernel mount paths escape space, tab, newline and backslash as \NNN octal.
fn unescape(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 4 <= bytes.len() {
            if let Some(v) = octal(&bytes[i + 1..i + 4]) {
                out.push(v);
                i += 4;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn octal(digits: &[u8]) -> Option<u8> {
    let mut v: u32 = 0;
    for d in digits {
        if !(b'0'..=b'7').contains(d) {
            return None;
        }
        v = v * 8 + u32::from(d - b'0');
    }
    u8::try_from(v).ok()
}

/// Why a local filesystem candidate is not sampled, when the reason is not
/// that another mount covers it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// Two mounts are attached to one parent at one point on its path.
    Ambiguous,
    /// Its path starts in the mount holding the process root, which the table
    /// omits (a chroot of a plain directory). That mount's type is unknown and
    /// its lookup might block.
    UnknownRoot,
    /// Its path passes through a mount whose lookup can block.
    Blocking { through: String, fstype: String },
}

/// A local filesystem candidate that resolution skipped, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    pub mount_point: String,
    pub device: String,
    pub reason: SkipReason,
}

impl Skipped {
    fn new(mount: &MountEntry, reason: SkipReason) -> Self {
        Self {
            mount_point: mount.mount_point.clone(),
            device: mount.device.clone(),
            reason,
        }
    }
}

impl std::fmt::Display for Skipped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({}): ", self.mount_point, self.device)?;
        match &self.reason {
            SkipReason::Ambiguous => {
                write!(f, "two mounts are attached at one point on its path")
            }
            SkipReason::UnknownRoot => write!(
                f,
                "its path starts in the mount holding the process root, which the mount \
                 table omits, so its lookup might block"
            ),
            SkipReason::Blocking { through, fstype } => write!(
                f,
                "its path passes through {fstype} at {through}, whose lookup can block"
            ),
        }
    }
}

/// Local filesystems selected from a mount table, and the candidates skipped
/// for a reason an operator needs to hear about.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Selection {
    /// One entry per device, sorted by mount point.
    pub mounts: Vec<MountEntry>,
    /// Sorted by mount point. Covered candidates are not listed, nor aliases of
    /// a device that another alias samples.
    pub skipped: Vec<Skipped>,
    /// How many mounts could hold the process root, when that is not exactly
    /// one and there were local candidates, all of which are then skipped.
    pub unresolvable_starts: Option<usize>,
}

/// One entry per device, choosing its shortest path among visible mounts;
/// sorted by mount point for deterministic initial slot assignment.
pub fn select_local(text: &str) -> Selection {
    // Classify before resolving: a container host carries thousands of overlay
    // mounts, and only local candidates need a path walk.
    let resolved = visible(&parse_mountinfo(text), MountEntry::is_local, |m| {
        !m.lookup_can_block()
    });
    let mut by_device: HashMap<String, MountEntry> = HashMap::new();
    for entry in resolved.mounts {
        match by_device.get(&entry.device) {
            Some(kept) if kept.mount_point.len() <= entry.mount_point.len() => {}
            _ => {
                by_device.insert(entry.device.clone(), entry);
            }
        }
    }
    let mut mounts: Vec<MountEntry> = by_device.into_values().collect();
    mounts.sort_by(|a, b| a.mount_point.cmp(&b.mount_point));
    let mut skipped = resolved.skipped;
    // A filesystem sampled through another alias has lost nothing.
    skipped.retain(|s| !mounts.iter().any(|m| m.device == s.device));
    skipped.sort_by(|a, b| a.mount_point.cmp(&b.mount_point));
    Selection {
        mounts,
        skipped,
        unresolvable_starts: resolved.unresolvable_starts,
    }
}

/// The selected mounts alone.
#[cfg(test)]
pub fn local_mounts(text: &str) -> Vec<MountEntry> {
    select_local(text).mounts
}

/// Mounts that path lookup reaches, of every filesystem type.
#[cfg(test)]
pub fn visible_mounts(text: &str) -> Vec<MountEntry> {
    visible(&parse_mountinfo(text), |_| true, |_| true).mounts
}

/// The mounts in `table` that `keep` accepts and path lookup reaches, and the
/// accepted ones skipped for a reason other than being covered.
///
/// Resolution starts at the one parent id the table references but does not
/// list: the mount holding the process root, which `mountinfo` omits because
/// it is not reachable from that root. On a host it is the parent of the `/`
/// row; a chroot of a plain directory has no `/` row at all. A mount that is
/// its own parent starts resolution the same way. At each mount point along a
/// path, resolution enters the mount attached there and climbs any stack on
/// that point, since a mount made over another takes it as parent. A mount is
/// visible when resolving its own path ends on it.
///
/// Every mount the lookup passes through on the way must satisfy `traverse`: a
/// local mount at `/home/scratch` under an NFS `/home` is reached by looking
/// `scratch` up inside the share, which waits on its server. With no single
/// start, two mounts attached to one parent at one point, or a path starting in
/// the omitted root, the candidate is skipped and the reason recorded.
fn visible(
    table: &[MountEntry],
    keep: impl Fn(&MountEntry) -> bool,
    traverse: impl Fn(&MountEntry) -> bool,
) -> Selection {
    let by_id: HashMap<u64, &MountEntry> = table.iter().map(|m| (m.id, m)).collect();
    // Must index every mount, whatever `keep` accepts: an excluded type can
    // cover a local mount.
    let mut attached: HashMap<(u64, &str), Vec<u64>> = HashMap::new();
    for m in table.iter().filter(|m| m.parent != m.id) {
        attached
            .entry((m.parent, m.mount_point.as_str()))
            .or_default()
            .push(m.id);
    }
    let starts: Vec<u64> = table
        .iter()
        .filter_map(|m| {
            if m.parent == m.id {
                Some(m.id)
            } else if !by_id.contains_key(&m.parent) {
                Some(m.parent)
            } else {
                None
            }
        })
        .collect::<HashSet<u64>>()
        .into_iter()
        .collect();
    let &[start] = starts.as_slice() else {
        return Selection {
            unresolvable_starts: table.iter().any(&keep).then_some(starts.len()),
            ..Selection::default()
        };
    };

    let mut selection = Selection::default();
    for m in table.iter().filter(|m| keep(m)) {
        let Some(walk) = resolve(&m.mount_point, start, &attached) else {
            selection
                .skipped
                .push(Skipped::new(m, SkipReason::Ambiguous));
            continue;
        };
        let Some((last, through)) = walk.split_last() else {
            continue;
        };
        if *last != m.id {
            // Covered: the path reaches another mount, which is not a loss.
            continue;
        }
        let blocked = through.iter().find_map(|id| match by_id.get(id) {
            // Must fail closed: the omitted root of a chroot may be NFS.
            None => Some(SkipReason::UnknownRoot),
            Some(p) if !traverse(p) => Some(SkipReason::Blocking {
                through: p.mount_point.clone(),
                fstype: p.fstype.clone(),
            }),
            Some(_) => None,
        });
        match blocked {
            Some(reason) => selection.skipped.push(Skipped::new(m, reason)),
            None => selection.mounts.push(m.clone()),
        }
    }
    selection
}

/// The mounts a lookup of `path` passes through, ending on the one it lands on,
/// or `None` past an ambiguous step.
fn resolve(path: &str, start: u64, attached: &HashMap<(u64, &str), Vec<u64>>) -> Option<Vec<u64>> {
    let mut current = climb(start, "/", attached)?;
    let mut walk = vec![current];
    let points = path
        .match_indices('/')
        .skip(1)
        .map(|(i, _)| &path[..i])
        .chain(std::iter::once(path))
        .filter(|point| *point != "/");
    for point in points {
        let next = climb(current, point, attached)?;
        if next != current {
            walk.push(next);
            current = next;
        }
    }
    Some(walk)
}

/// The top of the stack of mounts attached to `base` at `point`.
fn climb(base: u64, point: &str, attached: &HashMap<(u64, &str), Vec<u64>>) -> Option<u64> {
    let mut top = base;
    // Bounded by the table size, so a cyclic stack cannot loop.
    for _ in 0..=attached.len() {
        match attached.get(&(top, point)).map(Vec::as_slice) {
            None => return Some(top),
            Some(&[next]) => top = next,
            Some(_) => return None,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: &str =
        "22 1 259:2 / / rw,relatime shared:1 - ext4 /dev/nvme0n1p2 rw,errors=remount-ro";

    #[test]
    fn parses_the_fields_a_sweep_needs() {
        let entries = parse_mountinfo(ROOT);
        assert_eq!(entries.len(), 1);
        let m = &entries[0];
        assert_eq!((m.id, m.parent), (22, 1));
        assert_eq!(m.mount_point, "/");
        assert_eq!(m.fstype, "ext4");
        assert_eq!(m.source, "/dev/nvme0n1p2");
        assert_eq!(m.device, "259:2");
        assert!(!m.readonly);
    }

    /// A superblock `ro` (an older ext4 or a btrfs error) and ext4's
    /// `emergency_ro` (a current ext4 error) both read as read-only while the
    /// mount stays `rw`; a read-only bind is the reverse, and stays writable.
    #[test]
    fn read_only_comes_from_the_superblock_not_the_mount() {
        let text = "\
40 22 8:1 / /errored rw,relatime - ext4 /dev/sda1 ro,errors=remount-ro
41 22 8:2 / /view ro,relatime - ext4 /dev/sdb1 rw
42 22 8:3 / /emergency rw,relatime - ext4 /dev/sdc1 rw,errors=remount-ro,emergency_ro";
        let flags: Vec<bool> = parse_mountinfo(text).iter().map(|m| m.readonly).collect();
        assert_eq!(flags, vec![true, false, true]);
    }

    #[test]
    fn unescapes_octal_sequences_in_the_mount_point() {
        let line = "40 22 8:17 / /media/yao/USB\\040DRIVE rw - vfat /dev/sdb1 rw";
        let entries = parse_mountinfo(line);
        assert_eq!(entries[0].mount_point, "/media/yao/USB DRIVE");
    }

    #[test]
    fn tolerates_any_number_of_optional_fields_before_the_separator() {
        let none = "22 1 259:2 / / rw - ext4 /dev/a rw";
        let two = "22 1 259:2 / / rw shared:1 master:2 - ext4 /dev/a rw";
        assert_eq!(parse_mountinfo(none)[0].fstype, "ext4");
        assert_eq!(parse_mountinfo(two)[0].fstype, "ext4");
        assert_eq!(parse_mountinfo(two)[0].source, "/dev/a");
    }

    #[test]
    fn skips_lines_it_cannot_parse() {
        let text = "garbage\n\n22 1 259:2 / / rw - ext4 /dev/a rw\n1 2 3";
        assert_eq!(parse_mountinfo(text).len(), 1);
    }

    fn entry(fstype: &str, source: &str) -> MountEntry {
        MountEntry {
            id: 2,
            parent: 1,
            mount_point: "/x".to_string(),
            fstype: fstype.to_string(),
            source: source.to_string(),
            device: "0:1".to_string(),
            readonly: false,
        }
    }

    #[test]
    fn block_backed_filesystems_are_local() {
        for (fstype, source) in [
            ("ext4", "/dev/nvme0n1p2"),
            ("xfs", "/dev/sda1"),
            ("btrfs", "/dev/mapper/root"),
            ("vfat", "/dev/mmcblk0p1"),
            ("exfat", "/dev/sdb1"),
            ("f2fs", "/dev/mmcblk0p2"),
            ("ntfs3", "/dev/sdc1"),
        ] {
            assert!(entry(fstype, source).is_local(), "{fstype} on {source}");
        }
    }

    #[test]
    fn zfs_datasets_are_local_without_a_dev_source() {
        assert!(entry("zfs", "tank/home").is_local());
    }

    #[test]
    fn network_filesystems_are_not_local() {
        for (fstype, source) in [
            ("nfs", "nas:/export/runs"),
            ("nfs4", "nas:/export/runs"),
            ("cifs", "//nas/share"),
            ("smb3", "//nas/share"),
            ("ceph", "10.0.0.1:6789:/"),
            ("glusterfs", "gfs1:/vol"),
            ("9p", "hostshare"),
            ("afs", "afs"),
            ("lustre", "mgs@tcp:/fs"),
        ] {
            assert!(!entry(fstype, source).is_local(), "{fstype} on {source}");
        }
    }

    #[test]
    fn fuse_filesystems_are_not_local_whatever_they_mount() {
        assert!(!entry("fuse.sshfs", "yao@host:/").is_local());
        assert!(!entry("fuse.rclone", "remote:bucket").is_local());
        assert!(!entry("fuse", "/dev/fuse").is_local());
        assert!(!entry("fuseblk", "/dev/sdb1").is_local());
    }

    #[test]
    fn autofs_triggers_and_pseudo_filesystems_are_not_local() {
        for (fstype, source) in [
            ("autofs", "systemd-1"),
            ("tmpfs", "tmpfs"),
            ("devtmpfs", "udev"),
            ("overlay", "overlay"),
            ("proc", "proc"),
            ("sysfs", "sysfs"),
            ("cgroup2", "cgroup2"),
            ("squashfs", "/dev/loop3"),
            ("iso9660", "/dev/sr0"),
        ] {
            assert!(!entry(fstype, source).is_local(), "{fstype} on {source}");
        }
    }

    #[test]
    fn a_dev_source_with_an_unknown_type_is_local() {
        assert!(entry("bcachefs", "/dev/nvme1n1").is_local());
    }

    #[test]
    fn local_mounts_collapse_bind_mounts_onto_the_shortest_path() {
        let text = "\
22 1 259:2 / / rw - ext4 /dev/nvme0n1p2 rw
50 22 259:2 /var/lib/docker /mnt/docker-bind rw - ext4 /dev/nvme0n1p2 rw
60 22 8:1 / /data rw - xfs /dev/sda1 rw
70 22 0:40 / /mnt/nas rw - nfs4 nas:/export rw
80 22 0:24 / /run rw - tmpfs tmpfs rw";
        let mounts = local_mounts(text);
        let points: Vec<&str> = mounts.iter().map(|m| m.mount_point.as_str()).collect();
        assert_eq!(points, vec!["/", "/data"]);
    }

    fn points(text: &str) -> Vec<String> {
        local_mounts(text)
            .into_iter()
            .map(|m| m.mount_point)
            .collect()
    }

    #[test]
    fn a_local_mount_with_a_network_mount_stacked_on_it_is_not_sampled() {
        let text = "\
22 1 259:2 / / rw - ext4 /dev/nvme0n1p2 rw
40 22 8:1 / /data rw - ext4 /dev/sda1 rw
41 40 0:40 / /data rw - nfs4 nas:/export rw";
        assert_eq!(points(text), vec!["/"]);
    }

    /// An old `/data` tree covered by a replacement tree: the hidden old
    /// `/data/sub` must not suppress the visible new one at the same path.
    #[test]
    fn a_hidden_mount_does_not_hide_the_visible_mount_at_its_path() {
        let text = "\
22 1 259:2 / / rw - ext4 /dev/root rw
40 22 8:1 / /data rw - ext4 /dev/old rw
41 40 8:2 / /data/sub rw - ext4 /dev/oldchild rw
42 40 8:3 / /data rw - ext4 /dev/new rw
43 42 8:4 / /data/sub rw - ext4 /dev/newchild rw";
        let found: Vec<(u64, String)> = local_mounts(text)
            .into_iter()
            .map(|m| (m.id, m.mount_point))
            .collect();
        assert_eq!(
            found,
            vec![
                (22, "/".to_string()),
                (42, "/data".to_string()),
                (43, "/data/sub".to_string())
            ]
        );
    }

    /// A chroot of a plain directory omits the mount holding the process root.
    /// Its type is unknown and it may be NFS, so nothing below it is sampled,
    /// and each candidate says why.
    #[test]
    fn a_chroot_table_without_a_root_row_samples_nothing_and_says_why() {
        let text = "\
40 22 8:1 / /data rw - ext4 /dev/sda1 rw
41 22 0:3 / /proc rw - proc proc rw";
        let selection = select_local(text);
        assert!(selection.mounts.is_empty());
        assert_eq!(
            selection.skipped,
            vec![Skipped {
                mount_point: "/data".to_string(),
                device: "8:1".to_string(),
                reason: SkipReason::UnknownRoot,
            }]
        );
    }

    #[test]
    fn a_chroot_table_still_excludes_a_covered_mount() {
        let text = "\
40 22 8:1 / /data rw - ext4 /dev/sda1 rw
41 40 0:40 / /data rw - nfs4 nas:/export rw";
        assert!(points(text).is_empty());
    }

    /// Two parents referenced but not listed leave the process root unknown.
    #[test]
    fn two_omitted_parents_sample_nothing() {
        let text = "\
40 22 8:1 / /data rw - ext4 /dev/sda1 rw
50 23 8:2 / /srv rw - ext4 /dev/sdb1 rw";
        assert!(points(text).is_empty());
        assert_eq!(select_local(text).unresolvable_starts, Some(2));
    }

    /// Two mounts attached to one parent at one point leave the top unknown.
    #[test]
    fn an_ambiguous_attachment_samples_nothing_at_or_below_it() {
        let text = "\
22 1 259:2 / / rw - ext4 /dev/nvme0n1p2 rw
40 22 8:1 / /data rw - ext4 /dev/sda1 rw
41 22 8:2 / /data rw - ext4 /dev/sdb1 rw
42 41 8:3 / /data/sub rw - ext4 /dev/sdc1 rw
50 22 8:5 / /srv rw - ext4 /dev/sde1 rw";
        assert_eq!(points(text), vec!["/", "/srv"]);
        let skipped = select_local(text).skipped;
        assert_eq!(skipped.len(), 3, "{skipped:?}");
        assert!(skipped.iter().all(|s| s.reason == SkipReason::Ambiguous));
    }

    #[test]
    fn a_local_mount_under_a_later_mount_on_a_parent_directory_is_not_sampled() {
        let text = "\
22 1 259:2 / / rw - ext4 /dev/nvme0n1p2 rw
40 22 8:1 / /data/sub rw - ext4 /dev/sda1 rw
41 22 0:40 / /data rw - nfs4 nas:/export rw";
        assert_eq!(points(text), vec!["/"]);
    }

    #[test]
    fn a_local_mount_stacked_on_top_is_sampled_with_the_mounts_inside_it() {
        let text = "\
22 1 259:2 / / rw - ext4 /dev/nvme0n1p2 rw
40 22 0:40 / /data rw - nfs4 nas:/export rw
41 40 8:1 / /data rw - ext4 /dev/sda1 rw
42 41 8:2 / /data/sub rw - xfs /dev/sdb1 rw";
        assert_eq!(points(text), vec!["/", "/data", "/data/sub"]);
    }

    /// Opening `/home/scratch` looks `scratch` up inside the NFS share, so a
    /// dead server blocks the walk even though the target is local. tmpfs is
    /// in-kernel and does not block.
    #[test]
    fn a_local_mount_below_a_blocking_mount_is_not_sampled() {
        let text = "\
22 1 259:2 / / rw - ext4 /dev/nvme0n1p2 rw
40 22 0:40 / /home rw - nfs4 nas:/home rw
41 40 8:1 / /home/scratch rw - xfs /dev/sda1 rw
50 22 0:50 / /mnt/fuse rw - fuse.sshfs host:/ rw
51 50 8:2 / /mnt/fuse/disk rw - ext4 /dev/sdb1 rw
60 22 0:60 / /auto rw - autofs systemd-1 rw
61 60 8:3 / /auto/disk rw - ext4 /dev/sdc1 rw
70 22 0:24 / /run rw - tmpfs tmpfs rw
71 70 8:4 / /run/media rw - ext4 /dev/sdd1 rw";
        assert_eq!(points(text), vec!["/", "/run/media"]);
        let through: Vec<(String, String)> = select_local(text)
            .skipped
            .into_iter()
            .map(|s| match s.reason {
                SkipReason::Blocking { through, fstype } => (through, fstype),
                other => panic!("unexpected reason {other:?}"),
            })
            .collect();
        assert_eq!(
            through,
            vec![
                ("/auto".to_string(), "autofs".to_string()),
                ("/home".to_string(), "nfs4".to_string()),
                ("/mnt/fuse".to_string(), "fuse.sshfs".to_string()),
            ]
        );
    }

    /// A filesystem sampled through another alias has lost nothing, so its alias
    /// below a blocking mount is not reported.
    #[test]
    fn an_alias_below_a_blocking_mount_is_not_reported_when_another_is_sampled() {
        let text = "\
22 1 259:2 / / rw - ext4 /dev/nvme0n1p2 rw
40 22 0:40 / /home rw - nfs4 nas:/home rw
41 40 8:1 / /home/scratch rw - xfs /dev/sda1 rw
42 22 8:1 / /scratch rw - xfs /dev/sda1 rw";
        let selection = select_local(text);
        let points: Vec<&str> = selection
            .mounts
            .iter()
            .map(|m| m.mount_point.as_str())
            .collect();
        assert_eq!(points, vec!["/", "/scratch"]);
        assert!(selection.skipped.is_empty(), "{:?}", selection.skipped);
    }

    /// `/data` is a string prefix of `/database`, not a directory above it.
    #[test]
    fn cover_is_by_directory_not_by_string_prefix() {
        let text = "\
22 1 259:2 / / rw - ext4 /dev/nvme0n1p2 rw
40 22 8:1 / /database rw - ext4 /dev/sda1 rw
41 22 0:40 / /data rw - nfs4 nas:/export rw";
        assert_eq!(points(text), vec!["/", "/database"]);
    }

    #[test]
    fn a_covered_bind_alias_yields_to_a_longer_uncovered_one() {
        let text = "\
22 1 259:2 / / rw - ext4 /dev/nvme0n1p2 rw
50 22 8:1 / /a rw - ext4 /dev/sda1 rw
51 22 8:1 / /mnt/longer rw - ext4 /dev/sda1 rw
52 50 0:40 / /a rw - nfs4 nas:/export rw";
        assert_eq!(points(text), vec!["/", "/mnt/longer"]);
    }
}
