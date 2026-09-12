//! The mount table, read from `/proc/self/mountinfo`, reduced to the local
//! filesystems this sampler is allowed to `statvfs`.
//!
//! Classification happens here, before any `statvfs` call, because the call
//! itself is where a network filesystem can block (see the module doc in
//! `mod.rs`). Everything in this file is a pure function of the mount table's
//! text, so it is testable without a real mount.

use std::collections::HashMap;

/// One line of `/proc/self/mountinfo`, reduced to what the sweep needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountEntry {
    /// Where the filesystem is mounted, with `\NNN` octal escapes decoded.
    pub mount_point: String,
    /// The filesystem type as the kernel names it (`ext4`, `nfs4`, `fuse.sshfs`).
    pub fstype: String,
    /// The mount source: a `/dev` path for block-backed filesystems, a
    /// `host:/export` or `//server/share` for network ones, a bare word for
    /// pseudo-filesystems.
    pub source: String,
    /// The `major:minor` device id. Two mounts sharing it are the same
    /// filesystem seen twice (a bind mount, a subvolume mount).
    pub device: String,
}

/// Filesystem types whose reads go over a network, so `statvfs` can block for
/// as long as the mount options let it. Never sampled.
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

/// Filesystem types with no occupancy worth reporting: kernel pseudo
/// filesystems, RAM-backed scratch space, container overlays, read-only
/// images (a squashfs is always 100% full by construction), and autofs
/// triggers, which a `statvfs` would turn into a mount attempt.
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

/// Local filesystems whose source is not a `/dev` path: a pool or dataset
/// name stands in for the device.
const LOCAL_FSTYPES_WITHOUT_DEV_SOURCE: &[&str] = &["zfs"];

impl MountEntry {
    /// True when a `statvfs` on this mount is answered from the kernel's own
    /// superblock counters and never crosses a network or a userspace daemon.
    ///
    /// The rule is fail-closed: a filesystem qualifies by a positive signal
    /// (a `/dev` source, or a type known to be local without one), never by
    /// merely failing to match the deny lists. FUSE is excluded as a family
    /// because the process behind it can block for any reason, `fuseblk`
    /// included even though its source is a `/dev` path.
    pub fn is_local(&self) -> bool {
        let fstype = self.fstype.as_str();
        if NETWORK_FSTYPES.contains(&fstype) || PSEUDO_FSTYPES.contains(&fstype) {
            return false;
        }
        if fstype == "fuse" || fstype == "fuseblk" || fstype.starts_with("fuse.") {
            return false;
        }
        self.source.starts_with("/dev/") || LOCAL_FSTYPES_WITHOUT_DEV_SOURCE.contains(&fstype)
    }
}

/// Parse the text of `/proc/self/mountinfo`. Lines that do not have the
/// documented shape are skipped rather than failing the whole table.
///
/// The format (`proc(5)`): `id parent major:minor root mount_point options
/// [optional fields...] - fstype source super_options`. The optional fields
/// are variable in number, so the separator `-` is located first and the
/// fields after it are read from there.
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
        device: fields[2].to_string(),
        mount_point: unescape(fields[4]),
        fstype: fields[sep + 1].to_string(),
        source: unescape(fields[sep + 2]),
    })
}

/// Decode the `\NNN` octal escapes the kernel uses for space, tab, newline
/// and backslash in mount paths and sources.
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

/// The local filesystems in a mount table, one entry per filesystem.
///
/// A filesystem mounted more than once (bind mounts, btrfs subvolumes on one
/// device) shares a `major:minor` id and reports the same occupancy at every
/// mount, so it is kept once, under its shortest mount point. The result is
/// sorted by mount point so slot assignment upstream is deterministic.
pub fn local_mounts(text: &str) -> Vec<MountEntry> {
    let mut by_device: HashMap<String, MountEntry> = HashMap::new();
    for entry in parse_mountinfo(text)
        .into_iter()
        .filter(MountEntry::is_local)
    {
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
}
