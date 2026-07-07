use metriken::*;

/// Maximum number of drives tracked by the drive health metric group. Drives
/// discovered beyond this cap are dropped by `GaugeGroup` (logged once).
pub const MAX_DRIVES: usize = 64;

#[metric(
    name = "drive_temperature",
    description = "The current drive temperature in degrees Celsius (C). Labeled with the drive's `serial` when available, which is potentially sensitive but included for stable cross-reboot fleet identity.",
    metadata = { unit = "Celsius" }
)]
pub static DRIVE_TEMPERATURE: GaugeGroup = GaugeGroup::new(MAX_DRIVES);

// NVMe thermal-throttling counters, decoded from SMART/Health log page 0x02.
// Monotonic, so a coarse read cadence captures every event. NVMe-only.

#[metric(
    name = "drive_temperature_warning_time",
    description = "Cumulative seconds the NVMe composite temperature was at or above the warning threshold (WCTEMP).",
    metadata = { unit = "seconds" }
)]
pub static DRIVE_TEMPERATURE_WARNING_TIME: CounterGroup = CounterGroup::new(MAX_DRIVES);

#[metric(
    name = "drive_temperature_critical_time",
    description = "Cumulative seconds the NVMe composite temperature was at or above the critical threshold (CCTEMP).",
    metadata = { unit = "seconds" }
)]
pub static DRIVE_TEMPERATURE_CRITICAL_TIME: CounterGroup = CounterGroup::new(MAX_DRIVES);

#[metric(
    name = "drive_thermal_throttle_time",
    description = "Cumulative seconds spent in NVMe host-controlled thermal-management state TMT1 (only nonzero when HCTM is enabled).",
    metadata = { level = "1", unit = "seconds" }
)]
pub static DRIVE_THERMAL_THROTTLE_TIME_1: CounterGroup = CounterGroup::new(MAX_DRIVES);

#[metric(
    name = "drive_thermal_throttle_time",
    description = "Cumulative seconds spent in NVMe host-controlled thermal-management state TMT2 (only nonzero when HCTM is enabled).",
    metadata = { level = "2", unit = "seconds" }
)]
pub static DRIVE_THERMAL_THROTTLE_TIME_2: CounterGroup = CounterGroup::new(MAX_DRIVES);

#[metric(
    name = "drive_thermal_throttle_transitions",
    description = "Number of transitions into NVMe host-controlled thermal-management state TMT1 (only nonzero when HCTM is enabled).",
    metadata = { level = "1" }
)]
pub static DRIVE_THERMAL_THROTTLE_TRANSITIONS_1: CounterGroup = CounterGroup::new(MAX_DRIVES);

#[metric(
    name = "drive_thermal_throttle_transitions",
    description = "Number of transitions into NVMe host-controlled thermal-management state TMT2 (only nonzero when HCTM is enabled).",
    metadata = { level = "2" }
)]
pub static DRIVE_THERMAL_THROTTLE_TRANSITIONS_2: CounterGroup = CounterGroup::new(MAX_DRIVES);
