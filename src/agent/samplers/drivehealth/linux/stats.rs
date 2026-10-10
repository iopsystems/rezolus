use metriken::*;

use crate::agent::timing::AcquisitionGroup;
use linkme::distributed_slice;

/// Maximum number of drives tracked by the drive health metric group. Drives
/// discovered beyond this cap are dropped by `GaugeGroup` (logged once).
pub const MAX_DRIVES: usize = 64;

// Registered here (not in `linux/mod.rs`) because this file is compiled three
// ways: as the Linux sampler's `stats` module, inside the macOS sampler
// (`drivehealth/macos/mod.rs`), and on every other platform under
// `drivehealth/mod.rs`'s fallback `mod stats`, which keeps metric identity
// stable across platforms.

/// The sampler these groups belong to: `drivehealth` where that sampler exists
/// (Linux, macOS), and elsewhere the `unattributed` bucket, to which the
/// metrics below are attributed there (see
/// `crate::agent::samplers::bpf_sampler_name`).
#[cfg(any(target_os = "linux", target_os = "macos"))]
const GROUP_SAMPLER: &str = "drivehealth";
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
const GROUP_SAMPLER: &str = crate::agent::samplers::bpf_sampler_name("drivehealth");

/// The drivehealth sweep's group, holding `drive_temperature` for every
/// drive. One sweep reads every drive once and sets the gauge, and it is the
/// group's single writer: the `spawn_blocking` task in `linux/mod.rs` (one
/// read-only pass-through command per drive, `device::read_one`) or `sweep`
/// in `macos/mod.rs` (one NVMe SMART log page per drive). `refresh()` itself
/// never stamps. See `docs/principles.md` principle 18's "device sweep"
/// read-section shape.
/// A sweep that finishes while the V3 builder's walk is between this
/// group's first touch and its own emit point yields the honest union of
/// both windows (`resolve_walk_window` in `snapshot.rs`), not just one. At
/// this sampler's 60s read cadence that shows up as an occasional ~60s-wide
/// uncertainty band on this gauge, which is expected, not a bug.
pub static DRIVEHEALTH_SWEEP_ACQ: AcquisitionGroup =
    AcquisitionGroup::new(GROUP_SAMPLER, "drivehealth_sweep");

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static DRIVEHEALTH_SWEEP_ACQ_REG: &'static AcquisitionGroup = &DRIVEHEALTH_SWEEP_ACQ;

/// The NVMe-only thermal counters, which are a different population from the
/// temperature gauge beside them.
///
/// They were in `drivehealth_sweep` and labeled only for NVMe drives, so a SATA
/// drive's slot carried labels on the temperature gauge and none on these —
/// one slot meaning two things inside one group. That is exactly what a slot
/// index cannot express, and what
/// `a_groups_metrics_agree_on_slot_order_and_on_what_each_slot_means` pins for
/// every other group. Splitting them says what was already true: these
/// describe NVMe drives, and the gauge describes all of them.
pub static DRIVEHEALTH_NVME_ACQ: AcquisitionGroup =
    AcquisitionGroup::new(GROUP_SAMPLER, "drivehealth_nvme");

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static DRIVEHEALTH_NVME_ACQ_REG: &'static AcquisitionGroup = &DRIVEHEALTH_NVME_ACQ;

#[metric(
    name = "drive_temperature",
    description = "The current drive temperature in degrees Celsius (C). Labeled with the drive's `serial` when available, which is potentially sensitive but included for stable cross-reboot fleet identity.",
    metadata = { unit = "Celsius", acq_group = "drivehealth_sweep" }
)]
pub static DRIVE_TEMPERATURE: GaugeGroup = GaugeGroup::new(MAX_DRIVES);

// NVMe thermal-throttling counters, decoded from SMART/Health log page 0x02.
// Monotonic, so a coarse read cadence captures every event. NVMe-only.

#[metric(
    name = "drive_temperature_warning_time",
    description = "Cumulative seconds the NVMe composite temperature was at or above the warning threshold (WCTEMP).",
    metadata = { unit = "seconds", acq_group = "drivehealth_nvme" }
)]
pub static DRIVE_TEMPERATURE_WARNING_TIME: CounterGroup = CounterGroup::new(MAX_DRIVES);

#[metric(
    name = "drive_temperature_critical_time",
    description = "Cumulative seconds the NVMe composite temperature was at or above the critical threshold (CCTEMP).",
    metadata = { unit = "seconds", acq_group = "drivehealth_nvme" }
)]
pub static DRIVE_TEMPERATURE_CRITICAL_TIME: CounterGroup = CounterGroup::new(MAX_DRIVES);

#[metric(
    name = "drive_thermal_throttle_time",
    description = "Cumulative seconds spent in NVMe host-controlled thermal-management state TMT1 (only nonzero when HCTM is enabled).",
    metadata = { level = "1", unit = "seconds", acq_group = "drivehealth_nvme" }
)]
pub static DRIVE_THERMAL_THROTTLE_TIME_1: CounterGroup = CounterGroup::new(MAX_DRIVES);

#[metric(
    name = "drive_thermal_throttle_time",
    description = "Cumulative seconds spent in NVMe host-controlled thermal-management state TMT2 (only nonzero when HCTM is enabled).",
    metadata = { level = "2", unit = "seconds", acq_group = "drivehealth_nvme" }
)]
pub static DRIVE_THERMAL_THROTTLE_TIME_2: CounterGroup = CounterGroup::new(MAX_DRIVES);

#[metric(
    name = "drive_thermal_throttle_transitions",
    description = "Number of transitions into NVMe host-controlled thermal-management state TMT1 (only nonzero when HCTM is enabled).",
    metadata = { level = "1", acq_group = "drivehealth_nvme" }
)]
pub static DRIVE_THERMAL_THROTTLE_TRANSITIONS_1: CounterGroup = CounterGroup::new(MAX_DRIVES);

#[metric(
    name = "drive_thermal_throttle_transitions",
    description = "Number of transitions into NVMe host-controlled thermal-management state TMT2 (only nonzero when HCTM is enabled).",
    metadata = { level = "2", acq_group = "drivehealth_nvme" }
)]
pub static DRIVE_THERMAL_THROTTLE_TRANSITIONS_2: CounterGroup = CounterGroup::new(MAX_DRIVES);
