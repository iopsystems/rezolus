//! Discovery of Intel GPU PMUs exposed through `perf_event_open(2)`.
//!
//! The i915 driver registers one perf PMU per GPU under
//! `/sys/bus/event_source/devices/`. The naming depends on whether the GPU is
//! discrete (`i915_pmu.c`):
//!
//! ```c
//! if (IS_DGFX(i915)) {
//!     pmu->name = kasprintf(GFP_KERNEL, "i915_%s", dev_name(i915->drm.dev));
//!     strreplace((char *)pmu->name, ':', '_');   /* perf reserves colons */
//! } else {
//!     pmu->name = "i915";
//! }
//! ```
//!
//! So an integrated GPU is the bare `i915`, while a discrete card is
//! `i915_<pci-address>` with colons replaced by underscores (e.g. an Arc A770 at
//! `0000:04:00.0` becomes `i915_0000_04_00.0`).
//!
//! Only `i915` is matched; the `xe` driver's event vocabulary differs and is
//! out of scope — see [`parse_pmu_name`].
//!
//! Everything about a PMU is read from sysfs rather than hardcoded:
//!
//! - The perf `type` is **assigned dynamically at boot**, so it must be read from
//!   `<pmu>/type`. Hosts observed with the iGPU at type 23 and an A770 at 24.
//! - The available events differ per GPU. Both GPUs on the same host expose the
//!   same *names* for shared engines with identical configs, but only the A770
//!   has `ccs0` (compute) and the second video/video-enhance engines. Reading
//!   `<pmu>/events/` therefore discovers the real engine set instead of guessing
//!   it from the engine-class macros in `i915_drm.h`.
//! - `<pmu>/events/<event>.unit` gives the unit (`ns`, `M`, or absent for plain
//!   counts), which is how the sampler decides what a counter means.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

const EVENT_SOURCE_DIR: &str = "/sys/bus/event_source/devices";

/// A single event exposed by an Intel GPU PMU.
#[derive(Clone, Debug)]
pub struct PmuEvent {
    /// `perf_event_attr.config` for this event.
    pub config: u64,
    /// Unit from the sysfs `.unit` file, if any (`ns`, `M`, ...).
    pub unit: Option<String>,
}

/// An Intel GPU PMU discovered in sysfs.
#[derive(Clone, Debug)]
pub struct GpuPmu {
    /// The sysfs PMU name, e.g. `i915` or `i915_0000_04_00.0`.
    pub name: String,
    /// The dynamically-assigned perf type id.
    pub perf_type: u32,
    /// PCI address (`0000:04:00.0`) for a discrete GPU, `None` when integrated.
    pub pci_address: Option<String>,
    /// Kernel driver backing this PMU. Always `i915`; see [`parse_pmu_name`].
    pub driver: String,
    /// Events keyed by sysfs event name (`ccs0-busy`, `actual-frequency`, ...).
    pub events: HashMap<String, PmuEvent>,
}

impl GpuPmu {
    /// True when this PMU belongs to a discrete GPU (named after its PCI address).
    pub fn is_discrete(&self) -> bool {
        self.pci_address.is_some()
    }

    /// Look up an event by its sysfs name.
    pub fn event(&self, name: &str) -> Option<&PmuEvent> {
        self.events.get(name)
    }

    /// A stable, human-meaningful identifier for this GPU. Discrete GPUs are
    /// identified by PCI address; the integrated GPU has no PCI address in its
    /// PMU name, so it is reported as `integrated`.
    pub fn device_label(&self) -> String {
        self.pci_address
            .clone()
            .unwrap_or_else(|| "integrated".to_string())
    }
}

/// Discover every Intel GPU PMU on this host, sorted for stable `id` assignment.
///
/// Ordering is: integrated GPU first (it has no PCI address), then discrete GPUs
/// by ascending PCI address. Sorting keeps the `id` label stable across restarts
/// on a given host, since readdir order is not guaranteed.
pub fn discover() -> Vec<GpuPmu> {
    discover_in(Path::new(EVENT_SOURCE_DIR))
}

fn discover_in(dir: &Path) -> Vec<GpuPmu> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };

    let mut pmus: Vec<GpuPmu> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            parse_pmu_name(&name)?;
            read_pmu(&entry.path(), &name)
        })
        .collect();

    pmus.sort_by(|a, b| a.pci_address.cmp(&b.pci_address));
    pmus
}

/// Match an Intel GPU PMU directory name, returning `(driver, pci_address)`.
///
/// Accepts the bare driver name (integrated) and `<driver>_<pci>` (discrete).
/// The PCI portion has its colons replaced with underscores by the kernel; we
/// restore them so the label matches what `lspci`/sysfs report elsewhere.
///
/// **i915 only, by design.** The `xe` driver names its PMU the same way but
/// exposes a different event vocabulary — this sampler reads `<engine>-busy`
/// and `actual-frequency`/`requested-frequency`, which are i915's names.
///
/// Matching `xe` here would let discovery succeed and every `Gpu::new` then
/// fail with `NotFound`, so the sampler would report "no PMU counters could be
/// opened (needs CAP_PERFMON ...)" — blaming permissions for what is really a
/// driver this sampler does not read. Matching only i915 makes an xe host
/// report `Unsupported` with an accurate reason.
fn parse_pmu_name(name: &str) -> Option<(&'static str, Option<String>)> {
    const DRIVER: &str = "i915";

    if name == DRIVER {
        return Some((DRIVER, None));
    }

    let rest = name
        .strip_prefix(DRIVER)
        .and_then(|r| r.strip_prefix('_'))?;
    let pci = restore_pci_address(rest)?;
    Some((DRIVER, Some(pci)))
}

/// Turn the kernel's colon-free PCI address back into canonical form.
///
/// `0000_04_00.0` -> `0000:04:00.0`. Only the two separators the kernel rewrote
/// are restored; the function-number dot is left alone. Returns `None` for
/// anything that does not look like a PCI address, so unrelated PMUs that happen
/// to share the prefix are not misread as GPUs.
fn restore_pci_address(raw: &str) -> Option<String> {
    let (domain, rest) = raw.split_once('_')?;
    let (bus, device_function) = rest.split_once('_')?;

    let hex = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_hexdigit());

    if !hex(domain) || !hex(bus) {
        return None;
    }

    let (device, function) = device_function.split_once('.')?;
    if !hex(device) || !hex(function) {
        return None;
    }

    Some(format!("{domain}:{bus}:{device}.{function}"))
}

fn read_pmu(path: &Path, name: &str) -> Option<GpuPmu> {
    let (driver, pci_address) = parse_pmu_name(name)?;

    // The perf type is assigned at boot; without it we cannot open any event.
    let perf_type: u32 = read_trimmed(&path.join("type"))?.parse().ok()?;

    let events = read_events(&path.join("events"));
    if events.is_empty() {
        return None;
    }

    Some(GpuPmu {
        name: name.to_string(),
        perf_type,
        pci_address,
        driver: driver.to_string(),
        events,
    })
}

/// Read `<pmu>/events/`, pairing each event with its optional `.unit` sidecar.
fn read_events(dir: &Path) -> HashMap<String, PmuEvent> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return HashMap::new(),
    };

    let mut units: HashMap<String, String> = HashMap::new();
    let mut configs: HashMap<String, u64> = HashMap::new();

    for entry in entries.flatten() {
        let file_name = entry.file_name().to_string_lossy().into_owned();

        // `.scale` files are not used by the i915 PMU (none are present on
        // observed hosts) and would only rescale a value we already interpret
        // via its unit, so they are ignored.
        if let Some(stem) = file_name.strip_suffix(".unit") {
            if let Some(unit) = read_trimmed(&entry.path()) {
                units.insert(stem.to_string(), unit);
            }
            continue;
        }

        if file_name.contains('.') {
            continue;
        }

        if let Some(config) = read_trimmed(&entry.path()).and_then(|c| parse_config(&c)) {
            configs.insert(file_name, config);
        }
    }

    configs
        .into_iter()
        .map(|(name, config)| {
            let unit = units.get(&name).cloned();
            (name, PmuEvent { config, unit })
        })
        .collect()
}

/// Parse an event's sysfs contents, which look like `config=0x4000`.
///
/// The i915 PMU only ever emits a single `config=` term (its one format field is
/// `i915_eventid`, `config:0-20`), so any other term is unexpected and treated as
/// unparseable rather than silently ignored.
fn parse_config(raw: &str) -> Option<u64> {
    let value = raw.trim().strip_prefix("config=")?;

    // Reject trailing terms (e.g. `config=0x1,umask=0x2`) we do not understand.
    if value.contains(',') {
        return None;
    }

    match value.strip_prefix("0x") {
        Some(hex) => u64::from_str_radix(hex, 16).ok(),
        None => value.parse().ok(),
    }
}

fn read_trimmed(path: &PathBuf) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_integrated_pmu_name() {
        assert_eq!(parse_pmu_name("i915"), Some(("i915", None)));
        // `xe` is deliberately NOT matched: it is out of scope, and accepting
        // it here would surface a permissions error instead.
        assert_eq!(parse_pmu_name("xe"), None);
    }

    #[test]
    fn parses_discrete_pmu_name() {
        // An Arc A770 at 0000:04:00.0, as observed on a real host.
        assert_eq!(
            parse_pmu_name("i915_0000_04_00.0"),
            Some(("i915", Some("0000:04:00.0".to_string())))
        );
        assert_eq!(parse_pmu_name("xe_0000_03_00.0"), None);
    }

    #[test]
    fn ignores_unrelated_pmus() {
        for name in ["cpu", "uncore_imc", "intel_pt", "msr", "i915foo", "xelite"] {
            assert_eq!(parse_pmu_name(name), None, "should not match {name}");
        }
    }

    #[test]
    fn parses_event_configs() {
        assert_eq!(parse_config("config=0x4000"), Some(0x4000));
        assert_eq!(parse_config("config=0x100000\n"), Some(0x100000));
        assert_eq!(parse_config("config=0"), Some(0));
        // Multi-term configs are not part of the i915 PMU ABI.
        assert_eq!(parse_config("config=0x1,umask=0x2"), None);
        assert_eq!(parse_config("event=0x3"), None);
    }

    #[test]
    fn discovers_pmus_from_a_sysfs_tree() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        // An integrated GPU (no ccs) and a discrete GPU (with ccs), mirroring
        // the layout of a host that has both.
        let igpu = root.join("i915");
        std::fs::create_dir_all(igpu.join("events")).unwrap();
        std::fs::write(igpu.join("type"), "23\n").unwrap();
        std::fs::write(igpu.join("events/rcs0-busy"), "config=0x0\n").unwrap();
        std::fs::write(igpu.join("events/rcs0-busy.unit"), "ns\n").unwrap();

        let dgpu = root.join("i915_0000_04_00.0");
        std::fs::create_dir_all(dgpu.join("events")).unwrap();
        std::fs::write(dgpu.join("type"), "24\n").unwrap();
        std::fs::write(dgpu.join("events/ccs0-busy"), "config=0x4000\n").unwrap();
        std::fs::write(dgpu.join("events/ccs0-busy.unit"), "ns\n").unwrap();
        std::fs::write(dgpu.join("events/actual-frequency"), "config=0x100000\n").unwrap();
        std::fs::write(dgpu.join("events/actual-frequency.unit"), "M\n").unwrap();

        // An unrelated PMU that must be ignored.
        let other = root.join("uncore_imc");
        std::fs::create_dir_all(other.join("events")).unwrap();
        std::fs::write(other.join("type"), "12\n").unwrap();
        std::fs::write(other.join("events/data_reads"), "config=0x1\n").unwrap();

        let pmus = discover_in(root);
        assert_eq!(pmus.len(), 2);

        // Integrated sorts first (no PCI address).
        assert_eq!(pmus[0].name, "i915");
        assert_eq!(pmus[0].perf_type, 23);
        assert!(!pmus[0].is_discrete());
        assert_eq!(pmus[0].device_label(), "integrated");

        assert_eq!(pmus[1].name, "i915_0000_04_00.0");
        assert_eq!(pmus[1].perf_type, 24);
        assert!(pmus[1].is_discrete());
        assert_eq!(pmus[1].device_label(), "0000:04:00.0");
        assert_eq!(pmus[1].event("ccs0-busy").unwrap().config, 0x4000);
        assert_eq!(
            pmus[1].event("actual-frequency").unwrap().unit.as_deref(),
            Some("M")
        );
        assert!(pmus[1].event("rcs0-busy").is_none());
    }

    #[test]
    fn skips_pmus_with_no_events() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        let igpu = root.join("i915");
        std::fs::create_dir_all(igpu.join("events")).unwrap();
        std::fs::write(igpu.join("type"), "23\n").unwrap();

        assert!(discover_in(root).is_empty());
    }
}
