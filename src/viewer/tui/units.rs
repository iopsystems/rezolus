//! Value formatting for TUI chart axes. Mirrors the web viewer's
//! `assets/charts/util/units.js` (in metriken-charts) so the terminal shows
//! the same scaled units (e.g. `100.5%`, `503.32ms`, `9.29GB`, `10.34Gbps`,
//! `47.9K/s`).
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

/// Each stored unit metriken-charts knows (`STORED_UNITS` in
/// `assets/charts/util/units.js`): its dimension and the factor that takes a
/// value in it to the dimension's base unit.
static STORED_UNITS: &[(&str, &str, f64)] = &[
    ("nanoseconds", "time", 1.0),
    ("microseconds", "time", 1e3),
    ("milliseconds", "time", 1e6),
    ("seconds", "time", 1e9),
    ("bytes", "bytes", 1.0),
    ("kilobytes", "bytes", 1e3),
    ("kibibytes", "bytes", 1024.0),
    ("megabytes", "bytes", 1e6),
    ("mebibytes", "bytes", 1_048_576.0),
    ("gigabytes", "bytes", 1e9),
    ("gibibytes", "bytes", 1_073_741_824.0),
    ("bits", "bits", 1.0),
    ("percent", "ratio", 0.01),
    ("percentage", "ratio", 0.01),
    ("ratio", "ratio", 1.0),
    ("celsius", "temperature", 1.0),
    ("hertz", "frequency", 1.0),
    ("cores", "cores", 1.0),
];

/// The unit systems whose base is each dimension's base; the first is the
/// one a plot with a stored unit and no system uses.
static DIMENSION_SYSTEMS: &[(&str, &[&str])] = &[
    ("time", &["time"]),
    ("bytes", &["bytes", "datarate"]),
    ("bits", &["bitrate"]),
    ("ratio", &["percentage"]),
    ("temperature", &["temperature"]),
    ("frequency", &["frequency"]),
    ("cores", &["cores"]),
];

/// [`DIMENSION_SYSTEMS`]' systems for `dimension`, none for an unknown one.
fn dimension_systems(dimension: &str) -> &'static [&'static str] {
    DIMENSION_SYSTEMS
        .iter()
        .find(|(d, _)| *d == dimension)
        .map_or(&[], |(_, systems)| systems)
}

/// The unit system a plot is formatted with, and the factor its values are
/// multiplied by to be in that system's base, from its `format.unit_system`
/// and `format.unit` (the unit the series is stored in), as metriken-charts'
/// `resolveUnit` decides them: a unit of the system's dimension (or with no
/// system) applies its factor; a unit of another dimension, or one not in
/// [`STORED_UNITS`], is ignored.
pub fn resolve(unit_system: Option<&str>, unit: Option<&str>) -> (Option<String>, f64) {
    let stored = unit.and_then(|u| STORED_UNITS.iter().find(|(name, ..)| *name == u));
    let Some(&(_, dimension, factor)) = stored else {
        return (unit_system.map(str::to_string), 1.0);
    };
    let systems = dimension_systems(dimension);
    match unit_system {
        Some(sys) if !systems.contains(&sys) => (Some(sys.to_string()), 1.0),
        Some(sys) => (Some(sys.to_string()), factor),
        None => (systems.first().map(|s| s.to_string()), factor),
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
    fn a_stored_unit_of_the_systems_dimension_applies_its_factor() {
        assert_eq!(
            resolve(Some("time"), Some("seconds")),
            (Some("time".into()), 1e9)
        );
        // A p99 of 0.25 s, as a `_seconds` histogram stored as served reads.
        let (sys, factor) = resolve(Some("time"), Some("seconds"));
        assert_eq!(format_value(sys.as_deref(), 0.25 * factor), "250ms");
        assert_eq!(
            resolve(Some("datarate"), Some("kibibytes")),
            (Some("datarate".into()), 1024.0)
        );
        assert_eq!(
            resolve(None, Some("milliseconds")),
            (Some("time".into()), 1e6)
        );
        assert_eq!(
            resolve(Some("percentage"), Some("percent")),
            (Some("percentage".into()), 0.01)
        );
    }

    #[test]
    fn another_dimension_or_an_unknown_unit_is_ignored() {
        assert_eq!(
            resolve(Some("count"), Some("seconds")),
            (Some("count".into()), 1.0)
        );
        assert_eq!(
            resolve(Some("time"), Some("faults")),
            (Some("time".into()), 1.0)
        );
        assert_eq!(resolve(Some("time"), None), (Some("time".into()), 1.0));
        assert_eq!(resolve(None, None), (None, 1.0));
    }

    /// The body of `const <name> = { ... };` in metriken-charts' `units.js`:
    /// its lines up to the closing `};`.
    fn js_table<'a>(js: &'a str, name: &str) -> Vec<&'a str> {
        js.lines()
            .skip_while(|l| !l.starts_with(&format!("const {name} = {{")))
            .skip(1)
            .take_while(|l| !l.starts_with("};"))
            .map(str::trim)
            .collect()
    }

    /// [`STORED_UNITS`] and [`DIMENSION_SYSTEMS`] are copies of the tables
    /// metriken-charts' `resolveUnit` reads, so `view --tui` scales a series
    /// as the browser charts do.
    #[test]
    fn the_unit_tables_match_metriken_charts() {
        let js = metriken_charts::ASSETS
            .get_file("charts/util/units.js")
            .and_then(|f| f.contents_utf8())
            .expect("metriken-charts ships charts/util/units.js");

        // `name: { dimension: 'd', factor: f },`
        let mut stored: Vec<(String, String, f64)> = js_table(js, "STORED_UNITS")
            .into_iter()
            .map(|line| {
                let (name, rest) = line.split_once(':').expect(line);
                let dimension = rest.split('\'').nth(1).expect(line);
                let factor = rest
                    .split("factor:")
                    .nth(1)
                    .and_then(|f| f.trim().trim_end_matches(['}', ',', ' ']).parse().ok())
                    .unwrap_or_else(|| panic!("no factor in {line}"));
                (name.trim().to_string(), dimension.to_string(), factor)
            })
            .collect();
        let mut ours: Vec<(String, String, f64)> = STORED_UNITS
            .iter()
            .map(|(n, d, f)| (n.to_string(), d.to_string(), *f))
            .collect();
        stored.sort_by(|a, b| a.0.cmp(&b.0));
        ours.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(ours, stored);

        // `dimension: ['system', ...],`, each JS dimension's systems as ours
        // lists them, and no dimension of ours that the JS lacks.
        let systems = js_table(js, "DIMENSION_SYSTEMS");
        assert!(!systems.is_empty());
        let mut js_dimensions = Vec::new();
        for line in systems {
            let (dimension, rest) = line.split_once(':').expect(line);
            let listed: Vec<&str> = rest.split('\'').skip(1).step_by(2).collect();
            assert_eq!(dimension_systems(dimension.trim()), listed, "{line}");
            js_dimensions.push(dimension.trim());
        }
        for (dimension, _) in DIMENSION_SYSTEMS {
            assert!(js_dimensions.contains(dimension), "{dimension}");
        }
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
