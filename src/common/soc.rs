//! SoC identification from the flattened device tree.
//!
//! Some NVIDIA parts are integrated GPUs on a Tegra SoC rather than discrete
//! boards. NVML enumerates them, but most of its discrete-GPU query surface
//! either returns `NOT_SUPPORTED` or — worse — succeeds while reporting a
//! value the hardware cannot actually measure. The GPU samplers use this to
//! decide which NVML readings are meaningful on the running host.
//!
//! The check reads the device tree's root `compatible` property, a
//! NUL-separated list of `vendor,model` strings ordered most-specific first.
//! On a Jetson-class board that looks like:
//!
//! ```text
//! nvidia,p3971-0089\0nvidia,tegra264\0
//! ```
//!
//! where the trailing entry names the SoC. Every Tegra generation uses the
//! `nvidia,tegra<n>` form, so matching that prefix identifies the family
//! without enumerating individual SoCs.

/// The device tree's root `compatible` property.
///
/// Absent on x86 hosts and on ARM systems that boot via ACPI rather than a
/// flattened device tree; callers treat "cannot read" as "not a Tegra SoC".
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub const DEVICE_TREE_COMPATIBLE: &str = "/proc/device-tree/compatible";

/// Whether a device tree `compatible` property identifies an NVIDIA Tegra SoC.
///
/// Takes the raw property bytes so the parse is testable without a device
/// tree present; see [`is_tegra_soc`] for the reading wrapper.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn compatible_is_tegra(compatible: &[u8]) -> bool {
    compatible
        .split(|b| *b == 0)
        .any(|entry| entry.starts_with(b"nvidia,tegra"))
}

/// Whether the running host is an NVIDIA Tegra SoC.
///
/// A host with no device tree — every x86 machine, and ARM systems that boot
/// via ACPI — is not a Tegra part, so an unreadable property reads as `false`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn is_tegra_soc() -> bool {
    is_tegra_soc_at(DEVICE_TREE_COMPATIBLE)
}

fn is_tegra_soc_at(path: impl AsRef<std::path::Path>) -> bool {
    std::fs::read(path)
        .map(|compatible| compatible_is_tegra(&compatible))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_temp(name: &str, contents: &[u8]) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(name);
        std::fs::write(&path, contents).expect("write temp compatible");
        path
    }

    #[test]
    fn reads_tegra_soc_from_device_tree_property() {
        let path = write_temp(
            "rezolus-soc-tegra-compatible",
            b"nvidia,p3971-0089\0nvidia,tegra264\0",
        );
        assert!(is_tegra_soc_at(&path));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn identifies_tegra_soc_from_compatible() {
        assert!(compatible_is_tegra(b"nvidia,p3971-0089\0nvidia,tegra264\0"));
    }

    #[test]
    fn matches_every_tegra_generation() {
        for compatible in [
            &b"nvidia,p3701-0000+p3737-0000\0nvidia,tegra234\0"[..],
            &b"nvidia,tegra194\0"[..],
            &b"nvidia,tegra186\0"[..],
        ] {
            assert!(compatible_is_tegra(compatible), "{compatible:?}");
        }
    }

    #[test]
    fn rejects_non_tegra_boards() {
        assert!(!compatible_is_tegra(
            b"raspberrypi,4-model-b\0brcm,bcm2711\0"
        ));
        assert!(!compatible_is_tegra(b""));
    }

    #[test]
    fn requires_nvidia_tegra_at_an_entry_boundary() {
        // A vendor string that merely *contains* the token is a different
        // part; entries are matched from their start, not searched.
        assert!(!compatible_is_tegra(b"acme,not-nvidia,tegra264\0"));
    }

    #[test]
    fn absent_device_tree_is_not_a_tegra_soc() {
        assert!(!is_tegra_soc_at("/nonexistent/device-tree/compatible"));
    }
}
