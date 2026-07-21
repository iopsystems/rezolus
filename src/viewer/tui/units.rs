//! Value formatting for TUI chart axes. Mirrors the web viewer's
//! `assets/lib/charts/util/units.js` so the terminal shows the same scaled
//! units (e.g. `100.5%`, `503.32ms`, `9.29GB`, `10.34Gbps`, `47.9K/s`).
//!
//! Each entry is `(threshold, suffix, divisor, multiplier)`: pick the
//! largest scale whose `threshold` the absolute value reaches, then render
//! `value * multiplier / divisor` with the suffix.

type Scale = (f64, &'static str, f64, f64);

// Time, base nanoseconds.
static TIME: &[Scale] = &[
    (0.0, "ns", 1.0, 1.0),
    (1e3, "μs", 1e3, 1.0),
    (1e6, "ms", 1e6, 1.0),
    (1e9, "s", 1e9, 1.0),
];

// Data size, base bytes (binary scaling, matching the web viewer).
static BYTES: &[Scale] = &[
    (0.0, "B", 1.0, 1.0),
    (1024.0, "KB", 1024.0, 1.0),
    (1_048_576.0, "MB", 1_048_576.0, 1.0),
    (1_073_741_824.0, "GB", 1_073_741_824.0, 1.0),
    (1_099_511_627_776.0, "TB", 1_099_511_627_776.0, 1.0),
];

// Network bit rate, base bits/sec.
static BITRATE: &[Scale] = &[
    (0.0, "bps", 1.0, 1.0),
    (1e3, "Kbps", 1e3, 1.0),
    (1e6, "Mbps", 1e6, 1.0),
    (1e9, "Gbps", 1e9, 1.0),
    (1e12, "Tbps", 1e12, 1.0),
];

// Data rate, base bytes/sec.
static DATARATE: &[Scale] = &[
    (0.0, "B/s", 1.0, 1.0),
    (1e3, "KB/s", 1e3, 1.0),
    (1e6, "MB/s", 1e6, 1.0),
    (1e9, "GB/s", 1e9, 1.0),
    (1e12, "TB/s", 1e12, 1.0),
];

// Percentage: value is a fraction, scaled by 100.
static PERCENTAGE: &[Scale] = &[(0.0, "%", 1.0, 100.0)];

// Frequency, base Hz.
static FREQUENCY: &[Scale] = &[
    (0.0, "Hz", 1.0, 1.0),
    (1e3, "KHz", 1e3, 1.0),
    (1e6, "MHz", 1e6, 1.0),
    (1e9, "GHz", 1e9, 1.0),
];

// Bare count with K/M/B suffixes.
static COUNT: &[Scale] = &[
    (0.0, "", 1.0, 1.0),
    (1e3, "K", 1e3, 1.0),
    (1e6, "M", 1e6, 1.0),
    (1e9, "B", 1e9, 1.0),
];

// Rate (count per second) with /s, K/s, M/s, B/s suffixes.
static RATE: &[Scale] = &[
    (0.0, "/s", 1.0, 1.0),
    (1e3, "K/s", 1e3, 1.0),
    (1e6, "M/s", 1e6, 1.0),
    (1e9, "B/s", 1e9, 1.0),
];

fn scales(unit_system: &str) -> Option<&'static [Scale]> {
    match unit_system {
        "time" => Some(TIME),
        "bytes" => Some(BYTES),
        "bitrate" => Some(BITRATE),
        "datarate" => Some(DATARATE),
        "percentage" => Some(PERCENTAGE),
        "frequency" => Some(FREQUENCY),
        "count" => Some(COUNT),
        "rate" => Some(RATE),
        _ => None,
    }
}

/// Format `value` (in the unit system's base unit) with auto-scaling and a
/// suffix. An unknown or absent unit system falls back to a plain, trimmed
/// number. Two decimals, trailing zeros removed (`100.50%` -> `100.5%`,
/// `9.00` -> `9`).
pub fn format_value(unit_system: Option<&str>, value: f64) -> String {
    if !value.is_finite() {
        return "—".to_string();
    }
    let Some(sys) = unit_system.and_then(scales) else {
        return trim(&format!("{value:.2}")).to_string();
    };
    let av = value.abs();
    let &(_, suffix, divisor, multiplier) = sys
        .iter()
        .rev()
        .find(|&&(th, ..)| av >= th)
        .unwrap_or(&sys[0]);
    let scaled = value * multiplier / divisor;
    format!("{}{}", trim(&format!("{scaled:.2}")), suffix)
}

/// Trim trailing fractional zeros (and a bare trailing dot) from a decimal
/// string. Leaves integer strings untouched.
fn trim(s: &str) -> &str {
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.')
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentage_scales_by_100() {
        assert_eq!(format_value(Some("percentage"), 1.005), "100.5%");
        assert_eq!(format_value(Some("percentage"), 0.152), "15.2%");
        assert_eq!(format_value(Some("percentage"), 0.0), "0%");
    }

    #[test]
    fn time_scales_from_nanoseconds() {
        // 1.535 is 1.5349…9 in f64, so `{:.2}` yields 1.53 — matching the
        // web viewer's toFixed(2) on the same value.
        assert_eq!(format_value(Some("time"), 1535.0), "1.53μs");
        assert_eq!(format_value(Some("time"), 503_316_479.0), "503.32ms");
        assert_eq!(format_value(Some("time"), 2_000_000_000.0), "2s");
        assert_eq!(format_value(Some("time"), 42.0), "42ns");
    }

    #[test]
    fn bytes_use_binary_scaling() {
        assert_eq!(format_value(Some("bytes"), 9_980_456_960.0), "9.3GB");
        assert_eq!(format_value(Some("bytes"), 1024.0), "1KB");
    }

    #[test]
    fn bitrate_uses_si_scaling() {
        assert_eq!(format_value(Some("bitrate"), 10_338_674_776.0), "10.34Gbps");
    }

    #[test]
    fn rate_and_count_suffixes() {
        assert_eq!(format_value(Some("rate"), 47902.0), "47.9K/s");
        assert_eq!(format_value(Some("rate"), 500.0), "500/s");
        assert_eq!(format_value(Some("count"), 1_500_000.0), "1.5M");
    }

    #[test]
    fn unknown_or_missing_unit_is_plain_trimmed() {
        assert_eq!(format_value(None, 42.5), "42.5");
        assert_eq!(format_value(None, 9.0), "9");
        assert_eq!(format_value(Some("bogus"), 123.456), "123.46");
    }
}
