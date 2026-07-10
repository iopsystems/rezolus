//! One-time enumeration of physical drives from sysfs (blessed one-time
//! discovery, principle 15) and per-drive temperature dispatch. No kernel module
//! is loaded: NVMe controllers come from `/sys/class/nvme`, SATA/SCSI disks from
//! `/sys/block/sd*`. Each drive carries the `/dev` node its temperature is read
//! from via a read-only pass-through ioctl ([`super::ata`] / [`super::nvme`]).

use metriken::Window;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriveType {
    Nvme,
    Sata,
}

impl DriveType {
    pub fn as_str(&self) -> &'static str {
        match self {
            DriveType::Nvme => "nvme",
            DriveType::Sata => "sata",
        }
    }
}

/// A physical drive found at startup. `node` is the device the pass-through
/// ioctl is issued against; the label fields are read once here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Drive {
    /// Kernel device name (`nvme0`, `sda`).
    pub device: String,
    pub drive_type: DriveType,
    /// Model string, trimmed. Empty when unavailable.
    pub model: String,
    /// Serial, trimmed. Empty when unavailable (SATA serial via ATA IDENTIFY is
    /// deferred; NVMe serial comes from sysfs).
    pub serial: String,
    /// Device node for the pass-through ioctl (`/dev/nvme0`, `/dev/sda`).
    pub node: PathBuf,
}

fn read_trim(path: PathBuf) -> Option<String> {
    Some(std::fs::read_to_string(path).ok()?.trim().to_string())
}

/// Enumerate physical drives on the host.
pub fn enumerate() -> Vec<Drive> {
    enumerate_in(
        Path::new("/sys/block"),
        Path::new("/sys/class/nvme"),
        Path::new("/dev"),
    )
}

/// Inner form taking the sysfs/dev roots, for testing against fixtures.
/// SATA/SCSI whole disks are `/sys/block/sd<letters>` (partitions like `sda1`
/// and virtual devices like `loop0`/`dm-0` are skipped). NVMe is enumerated by
/// *controller* (`/sys/class/nvme/nvme0`), not namespace, so multi-namespace
/// drives are not double-counted.
fn enumerate_in(sys_block: &Path, sys_nvme: &Path, dev: &Path) -> Vec<Drive> {
    let mut out = Vec::new();

    // SATA/SCSI disks.
    if let Ok(entries) = std::fs::read_dir(sys_block) {
        let mut names: Vec<String> = entries
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        names.sort();
        for name in names {
            let is_whole_disk = name.strip_prefix("sd").is_some_and(|rest| {
                !rest.is_empty() && rest.chars().all(|c| c.is_ascii_alphabetic())
            });
            if !is_whole_disk {
                continue;
            }
            let devdir = sys_block.join(&name).join("device");
            out.push(Drive {
                model: read_trim(devdir.join("model")).unwrap_or_default(),
                serial: String::new(),
                node: dev.join(&name),
                device: name,
                drive_type: DriveType::Sata,
            });
        }
    }

    // NVMe controllers.
    if let Ok(entries) = std::fs::read_dir(sys_nvme) {
        let mut names: Vec<String> = entries
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        names.sort();
        for name in names {
            let ctrl = sys_nvme.join(&name);
            out.push(Drive {
                model: read_trim(ctrl.join("model")).unwrap_or_default(),
                serial: read_trim(ctrl.join("serial")).unwrap_or_default(),
                node: dev.join(&name),
                device: name,
                drive_type: DriveType::Nvme,
            });
        }
    }

    out
}

/// One drive's decoded reading. Every drive reports `temperature_c`; NVMe drives
/// additionally carry thermal-throttle counters from the same log-page read.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DriveReading {
    pub temperature_c: Option<i64>,
    pub nvme: Option<super::nvme::NvmeHealth>,
    /// Acquisition window of this device's read (wall begin, wall begin+elapsed).
    pub window: Option<Window>,
}

/// Run `read` while capturing a wall-clock acquisition window: begin is wall
/// time before the call; end is begin + monotonic elapsed (immune to an NTP
/// step during the read).
fn timed<T>(read: impl FnOnce() -> T) -> (T, Window) {
    let begin_wall = SystemTime::now();
    let begin_mono = Instant::now();
    let out = read();
    let elapsed_ns = begin_mono.elapsed().as_nanos() as u64;
    let begin_ns = begin_wall
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    (out, Window::new(begin_ns, begin_ns + elapsed_ns))
}

/// Read one drive via the appropriate read-only pass-through ioctl.
fn read_one(drive: &Drive) -> DriveReading {
    match drive.drive_type {
        DriveType::Nvme => {
            let (nvme, window) = timed(|| super::nvme::read_health(&drive.node));
            DriveReading {
                temperature_c: nvme.as_ref().and_then(|h| h.temperature_c),
                nvme,
                window: Some(window),
            }
        }
        DriveType::Sata => {
            let (temperature_c, window) = timed(|| super::ata::read_temperature(&drive.node));
            DriveReading {
                temperature_c,
                nvme: None,
                window: Some(window),
            }
        }
    }
}

/// Read every drive concurrently, returning results in the same order as
/// `drives`. Each read is a blocking device command, so reads run one thread per
/// drive to bound the burst to a single drive's latency. A drive whose read
/// fails yields an empty `DriveReading`.
pub fn read_all(drives: &[Drive]) -> Vec<DriveReading> {
    std::thread::scope(|scope| {
        let handles: Vec<_> = drives
            .iter()
            .map(|drive| scope.spawn(move || read_one(drive)))
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_default())
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn enumerates_sata_and_nvme_skipping_partitions_and_virtual() {
        let tmp = std::env::temp_dir().join(format!("rezolus_drivedev_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        let sys_block = tmp.join("sys/block");
        let sys_nvme = tmp.join("sys/class/nvme");
        let dev = tmp.join("dev");

        // A whole SATA disk, a partition (skip), and virtual devices (skip).
        write(&sys_block.join("sda/device/model"), "Samsung SSD 870 \n");
        fs::create_dir_all(sys_block.join("sda1")).unwrap(); // partition
        fs::create_dir_all(sys_block.join("loop0")).unwrap();
        fs::create_dir_all(sys_block.join("dm-0")).unwrap();
        fs::create_dir_all(sys_block.join("nvme0n1")).unwrap(); // namespace, not counted here

        // An NVMe controller.
        write(&sys_nvme.join("nvme0/model"), "Samsung SSD 990 PRO\n");
        write(&sys_nvme.join("nvme0/serial"), "S1A2B3\n");

        let drives = enumerate_in(&sys_block, &sys_nvme, &dev);

        assert_eq!(drives.len(), 2, "expected exactly sda + nvme0: {drives:?}");

        let sata = drives.iter().find(|d| d.device == "sda").unwrap();
        assert_eq!(sata.drive_type, DriveType::Sata);
        assert_eq!(sata.model, "Samsung SSD 870");
        assert_eq!(sata.node, dev.join("sda"));

        let nvme = drives.iter().find(|d| d.device == "nvme0").unwrap();
        assert_eq!(nvme.drive_type, DriveType::Nvme);
        assert_eq!(nvme.model, "Samsung SSD 990 PRO");
        assert_eq!(nvme.serial, "S1A2B3");
        assert_eq!(nvme.node, dev.join("nvme0"));

        fs::remove_dir_all(&tmp).ok();
    }

    /// Hardware smoke + overhead measurement — requires root. Ignored by
    /// default. Run: build test bin, then
    ///   sudo ./target/debug/deps/rezolus-* device::tests::hardware -- --ignored --nocapture
    #[test]
    #[ignore]
    fn hardware_enumerate_and_read_all() {
        let drives = enumerate();
        println!("enumerated {} drive(s)", drives.len());
        let start = std::time::Instant::now();
        let readings = read_all(&drives);
        let elapsed = start.elapsed();
        for (d, r) in drives.iter().zip(&readings) {
            println!(
                "  {:>6} {:<4} -> {:?} C   node={:?} model={:?}  nvme={:?}",
                d.device,
                d.drive_type.as_str(),
                r.temperature_c,
                d.node,
                d.model,
                r.nvme
            );
        }
        let ok = readings
            .iter()
            .filter(|r| r.temperature_c.is_some())
            .count();
        println!(
            "read {}/{} drives in {:?} ({:?}/drive avg)",
            ok,
            drives.len(),
            elapsed,
            elapsed.checked_div(drives.len() as u32).unwrap_or_default()
        );
    }

    #[test]
    fn timed_captures_a_nonzero_window_covering_the_read() {
        let (val, window) = timed(|| {
            std::thread::sleep(std::time::Duration::from_millis(5));
            7
        });
        assert_eq!(val, 7);
        assert!(window.end_ns >= window.begin_ns);
        assert!(window.width_ns() >= 4_000_000, "≥4ms: {}", window.width_ns());
    }

    #[test]
    fn missing_roots_are_empty() {
        let drives = enumerate_in(
            Path::new("/nonexistent/block"),
            Path::new("/nonexistent/nvme"),
            Path::new("/dev"),
        );
        assert!(drives.is_empty());
    }
}
