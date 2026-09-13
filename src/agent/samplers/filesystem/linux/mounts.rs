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
    if sep < 6 || fields.len() < sep + 3 {
        return None;
    }
    Some(MountEntry {
        id: fields[0].parse().ok()?,
        parent: fields[1].parse().ok()?,
        device: fields[2].to_string(),
        mount_point: unescape(fields[4]),
        fstype: fields[sep + 1].to_string(),
        source: unescape(fields[sep + 2]),
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

/// One entry per device, choosing its shortest path among visible mounts;
/// sorted by mount point for deterministic initial slot assignment.
pub fn local_mounts(text: &str) -> Vec<MountEntry> {
    let mut by_device: HashMap<String, MountEntry> = HashMap::new();
    // Classify before resolving: a container host carries thousands of overlay
    // mounts, and only local candidates need a path walk.
    for entry in visible(&parse_mountinfo(text), MountEntry::is_local) {
        match by_device.get(&entry.device) {
            Some(kept) if kept.mount_point.len() <= entry.mount_point.len() => {}
            _ => {
                by_device.insert(entry.device.clone(), entry);
            }
        }
    }
    let mut mounts: Vec<MountEntry> = by_device.into_values().collect();
    mounts.sort_by(|a, b| a.mount_point.cmp(&b.mount_point));
    mounts
}

/// Mounts that path lookup reaches, of every filesystem type.
#[cfg(test)]
pub fn visible_mounts(text: &str) -> Vec<MountEntry> {
    visible(&parse_mountinfo(text), |_| true)
}

/// The mounts in `table` that `keep` accepts and path lookup reaches.
///
/// Resolution starts at the root mount, the one at `/` whose parent is not in
/// the table. At each mount point along a path it enters the mount attached
/// there and climbs any stack on that point, since a mount made over another
/// takes it as parent. A mount is visible when resolving its own path ends on
/// it. With no single root, or two mounts attached to one parent at one point,
/// resolution stops, and nothing at or below the ambiguity is returned.
fn visible(table: &[MountEntry], keep: impl Fn(&MountEntry) -> bool) -> Vec<MountEntry> {
    let ids: HashSet<u64> = table.iter().map(|m| m.id).collect();
    // Must index every mount, whatever `keep` accepts: an excluded type can
    // cover a local mount.
    let mut attached: HashMap<(u64, &str), Vec<u64>> = HashMap::new();
    for m in table.iter().filter(|m| m.parent != m.id) {
        attached
            .entry((m.parent, m.mount_point.as_str()))
            .or_default()
            .push(m.id);
    }
    let roots: Vec<u64> = table
        .iter()
        .filter(|m| m.mount_point == "/" && (m.parent == m.id || !ids.contains(&m.parent)))
        .map(|m| m.id)
        .collect();
    let &[root] = roots.as_slice() else {
        return Vec::new();
    };
    table
        .iter()
        .filter(|m| keep(m) && resolve(&m.mount_point, root, &attached) == Some(m.id))
        .cloned()
        .collect()
}

/// The mount a lookup of `path` ends on, or `None` past an ambiguous step.
fn resolve(path: &str, root: u64, attached: &HashMap<(u64, &str), Vec<u64>>) -> Option<u64> {
    let mut current = climb(root, "/", attached)?;
    let points = path
        .match_indices('/')
        .skip(1)
        .map(|(i, _)| &path[..i])
        .chain(std::iter::once(path))
        .filter(|point| *point != "/");
    for point in points {
        current = climb(current, point, attached)?;
    }
    Some(current)
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

    /// With no root mount at `/` to resolve from, nothing is sampled.
    #[test]
    fn a_table_without_a_root_mount_samples_nothing() {
        let text = "\
40 22 8:1 / /data rw - ext4 /dev/sda1 rw
41 40 0:40 / /data rw - nfs4 nas:/export rw";
        assert!(points(text).is_empty());
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
