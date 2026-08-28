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

    /// A `compatible` file in a private temp dir.
    ///
    /// Deliberately not a fixed path under `temp_dir()`: two concurrent test
    /// runs on one machine (a CI matrix on a shared runner, or a second user)
    /// would race on the same name, and a failing assert would leak the file
    /// into later runs.
    fn write_temp(contents: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("compatible");
        std::fs::write(&path, contents).expect("write temp compatible");
        (dir, path)
    }

    #[test]
    fn reads_tegra_soc_from_device_tree_property() {
        let (_dir, path) = write_temp(b"nvidia,p3971-0089\0nvidia,tegra264\0");
        assert!(is_tegra_soc_at(&path));
    }

    #[test]
    fn reads_a_non_tegra_device_tree_property_as_false() {
        // Pins that the wrapper actually *parses* what it reads. Without this
        // case, replacing the whole body with `path.exists()` passes.
        let (_dir, path) = write_temp(b"raspberrypi,4-model-b\0brcm,bcm2711\0");
        assert!(!is_tegra_soc_at(&path));
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
    fn matches_a_single_entry_with_no_trailing_nul() {
        // What a truncated read, or a property written without its terminator,
        // looks like. `split` still yields the entry intact.
        assert!(compatible_is_tegra(b"nvidia,tegra234"));
    }

    #[test]
    fn rejects_non_tegra_boards() {
        assert!(!compatible_is_tegra(
            b"raspberrypi,4-model-b\0brcm,bcm2711\0"
        ));
        assert!(!compatible_is_tegra(b""));
    }

    #[test]
    fn rejects_other_nvidia_parts() {
        // The `tegra` half of the token is load-bearing: an NVIDIA-vendored
        // entry that does not name the SoC family is not a Tegra SoC. Without
        // this case, matching on `b"nvidia,"` alone passes.
        assert!(!compatible_is_tegra(b"nvidia,p3971-0089\0"));
        assert!(!compatible_is_tegra(b"nvidia,holoscan\0"));
    }

    #[test]
    fn requires_nvidia_tegra_at_an_entry_boundary() {
        // A vendor string that merely *contains* the token is a different
        // part; entries are matched from their start, not searched.
        assert!(!compatible_is_tegra(b"acme,not-nvidia,tegra264\0"));
    }

    #[test]
    fn matches_case_sensitively() {
        // Device tree vendor prefixes are lowercase by schema; an uppercase
        // value is not a device tree we recognise. Recorded as a decision
        // rather than left to chance.
        assert!(!compatible_is_tegra(b"NVIDIA,TEGRA234\0"));
    }

    #[test]
    fn absent_device_tree_is_not_a_tegra_soc() {
        assert!(!is_tegra_soc_at("/nonexistent/device-tree/compatible"));
    }
}
