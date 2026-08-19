use metriken::*;

use crate::agent::timing::AcquisitionGroup;
use linkme::distributed_slice;

/// Maximum number of drives tracked by the drive health metric group. Drives
/// discovered beyond this cap are dropped by `GaugeGroup` (logged once).
pub const MAX_DRIVES: usize = 64;

// Registered here (not in `linux/mod.rs`) because this file is also
// `include!`d directly on non-Linux platforms (see `drivehealth/mod.rs`'s
// `#[cfg(not(target_os = "linux"))] mod stats` fallback) to keep metric
// identity stable across platforms. Same cross-platform-name mechanism as
// the BPF samplers (e.g. `cpu_migrations`'s `MIGRATIONS_ACQ`) — see
// `crate::agent::samplers::bpf_sampler_name`'s doc comment.
//
/// ONE group for the entire drivehealth sweep: `read_all(&drives)` plus the
/// per-drive `set()` loop that follows it, bracketed inside the
/// `spawn_blocking` task in `linux/mod.rs` — that task is this group's
/// single writer (`refresh()` itself never stamps; see the doc comment
/// there). All seven metrics below — drive temperature and the six
/// NVMe-only throttle counters — share this one group rather than one per
/// metric family: each drive's reading comes from a SINGLE read-only
/// pass-through ioctl (`device::read_one`, one command per drive) that
/// decodes every one of these fields from that one response, so unlike
/// `cpu_usage`'s three separate BPF map reads, there is exactly one source
/// per drive here — unambiguously one read section for the whole sweep. See
/// `docs/principles.md` principle 18's "device sweep" read-section shape.
pub static DRIVEHEALTH_SWEEP_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("drivehealth"),
    "drivehealth_sweep",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static DRIVEHEALTH_SWEEP_ACQ_REG: &'static AcquisitionGroup = &DRIVEHEALTH_SWEEP_ACQ;

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
    metadata = { unit = "seconds", acq_group = "drivehealth_sweep" }
)]
pub static DRIVE_TEMPERATURE_WARNING_TIME: CounterGroup = CounterGroup::new(MAX_DRIVES);

#[metric(
    name = "drive_temperature_critical_time",
    description = "Cumulative seconds the NVMe composite temperature was at or above the critical threshold (CCTEMP).",
    metadata = { unit = "seconds", acq_group = "drivehealth_sweep" }
)]
pub static DRIVE_TEMPERATURE_CRITICAL_TIME: CounterGroup = CounterGroup::new(MAX_DRIVES);

#[metric(
    name = "drive_thermal_throttle_time",
    description = "Cumulative seconds spent in NVMe host-controlled thermal-management state TMT1 (only nonzero when HCTM is enabled).",
    metadata = { level = "1", unit = "seconds", acq_group = "drivehealth_sweep" }
)]
pub static DRIVE_THERMAL_THROTTLE_TIME_1: CounterGroup = CounterGroup::new(MAX_DRIVES);

#[metric(
    name = "drive_thermal_throttle_time",
    description = "Cumulative seconds spent in NVMe host-controlled thermal-management state TMT2 (only nonzero when HCTM is enabled).",
    metadata = { level = "2", unit = "seconds", acq_group = "drivehealth_sweep" }
)]
pub static DRIVE_THERMAL_THROTTLE_TIME_2: CounterGroup = CounterGroup::new(MAX_DRIVES);

#[metric(
    name = "drive_thermal_throttle_transitions",
    description = "Number of transitions into NVMe host-controlled thermal-management state TMT1 (only nonzero when HCTM is enabled).",
    metadata = { level = "1", acq_group = "drivehealth_sweep" }
)]
pub static DRIVE_THERMAL_THROTTLE_TRANSITIONS_1: CounterGroup = CounterGroup::new(MAX_DRIVES);

#[metric(
    name = "drive_thermal_throttle_transitions",
    description = "Number of transitions into NVMe host-controlled thermal-management state TMT2 (only nonzero when HCTM is enabled).",
    metadata = { level = "2", acq_group = "drivehealth_sweep" }
)]
pub static DRIVE_THERMAL_THROTTLE_TRANSITIONS_2: CounterGroup = CounterGroup::new(MAX_DRIVES);
