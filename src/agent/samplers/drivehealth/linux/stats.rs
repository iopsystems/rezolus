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
