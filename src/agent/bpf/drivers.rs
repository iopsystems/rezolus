//! Enumerate which drivers are bound to present devices, for classifying
//! per-driver BPF probe expectations. This is one-time sysfs discovery
//! (principle 15), not a hot-path read.
//!
//! A driver probe (e.g. a NIC `*_tx_timeout` kprobe) should attach iff the
//! driver it targets is bound to a present device. We read that from the
//! `driver` symlink each bound device exposes under `/sys/bus/<bus>/devices/`,
//! which covers every subsystem (net, block, pci, virtio, …). Unlike
//! `/proc/modules`, this reflects actual device binding and also catches
//! built-in (compiled-in) drivers, not merely whether a module is loaded.

use std::collections::HashSet;
use std::path::Path;

/// Set of driver names bound to present devices (e.g. `ena`, `mlx5_core`,
/// `virtio_net`, `nvme`). Reads the real sysfs bus tree.
pub fn bound_drivers() -> HashSet<String> {
    bound_drivers_in(Path::new("/sys/bus"))
}

/// Inner form taking the sysfs `bus` root, for testing against a fixture dir.
/// Walks `<bus>/devices/<dev>/driver` symlinks; the basename of each is the
/// bound driver's name. A device with no `driver` symlink (unbound) is skipped.
fn bound_drivers_in(bus_root: &Path) -> HashSet<String> {
    let mut out = HashSet::new();
    let buses = match std::fs::read_dir(bus_root) {
        Ok(e) => e,
        Err(_) => return out,
    };
    for bus in buses.flatten() {
        let devices = bus.path().join("devices");
        let devs = match std::fs::read_dir(&devices) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for dev in devs.flatten() {
            let driver_link = dev.path().join("driver");
            if let Ok(target) = std::fs::read_link(&driver_link) {
                if let Some(name) = target.file_name().and_then(|n| n.to_str()) {
                    out.insert(name.to_string());
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn reads_bound_driver_basenames_across_buses() {
        let tmp = std::env::temp_dir().join(format!("rezolus_drv_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let bus = tmp.join("bus");

        // pci bus: a device bound to the `ena` driver
        let pci_dev = bus.join("pci").join("devices").join("0000:00:05.0");
        std::fs::create_dir_all(&pci_dev).unwrap();
        let ena = tmp.join("drivers").join("ena");
        std::fs::create_dir_all(&ena).unwrap();
        symlink(&ena, pci_dev.join("driver")).unwrap();

        // virtio bus: a device bound to the `virtio_net` driver
        let virtio_dev = bus.join("virtio").join("devices").join("virtio0");
        std::fs::create_dir_all(&virtio_dev).unwrap();
        let vnet = tmp.join("drivers").join("virtio_net");
        std::fs::create_dir_all(&vnet).unwrap();
        symlink(&vnet, virtio_dev.join("driver")).unwrap();

        // an unbound device (no driver symlink) is skipped
        std::fs::create_dir_all(bus.join("pci").join("devices").join("0000:00:06.0")).unwrap();

        let drivers = bound_drivers_in(&bus);
        assert!(drivers.contains("ena"));
        assert!(drivers.contains("virtio_net"));
        assert_eq!(drivers.len(), 2);

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn missing_root_is_empty() {
        assert!(bound_drivers_in(Path::new("/nonexistent/xyz")).is_empty());
    }
}
