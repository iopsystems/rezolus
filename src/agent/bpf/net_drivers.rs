//! Enumerate which NIC drivers (kernel modules) are bound to present network
//! interfaces, for classifying per-driver BPF probe expectations. This is
//! one-time sysfs discovery (principle 15), not a hot-path read.

use std::collections::HashSet;
use std::path::Path;

/// Set of sysfs module names bound to present interfaces (e.g. `ena`,
/// `mlx5_core`, `virtio_net`). Reads the real sysfs root.
pub fn bound_net_drivers() -> HashSet<String> {
    bound_net_drivers_in(Path::new("/sys/class/net"))
}

/// Inner form taking the sysfs `net` root, for testing against a fixture dir.
/// Each interface's `<iface>/device/driver` is a symlink whose basename is the
/// driver module name.
fn bound_net_drivers_in(net_root: &Path) -> HashSet<String> {
    let mut out = HashSet::new();
    let entries = match std::fs::read_dir(net_root) {
        Ok(e) => e,
        Err(_) => return out,
    };
    for entry in entries.flatten() {
        let driver_link = entry.path().join("device").join("driver");
        if let Ok(target) = std::fs::read_link(&driver_link) {
            if let Some(name) = target.file_name().and_then(|n| n.to_str()) {
                out.insert(name.to_string());
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
    fn reads_driver_basenames_from_fixture() {
        let tmp = std::env::temp_dir().join(format!("rezolus_netdrv_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let net = tmp.join("net");
        // eth0 -> driver ena
        let eth0_dev = net.join("eth0").join("device");
        std::fs::create_dir_all(&eth0_dev).unwrap();
        let ena_mod = tmp.join("modules").join("ena");
        std::fs::create_dir_all(&ena_mod).unwrap();
        symlink(&ena_mod, eth0_dev.join("driver")).unwrap();
        // lo -> no device dir (skipped)
        std::fs::create_dir_all(net.join("lo")).unwrap();

        let drivers = bound_net_drivers_in(&net);
        assert!(drivers.contains("ena"));
        assert_eq!(drivers.len(), 1);

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn missing_root_is_empty() {
        assert!(bound_net_drivers_in(Path::new("/nonexistent/xyz")).is_empty());
    }
}
