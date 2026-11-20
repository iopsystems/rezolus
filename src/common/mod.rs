pub mod description_merge;
mod logging;
mod soc;
pub use logging::{configure_logging, verbosity_to_level, Level, LogConfig, LogDrain};
pub use soc::is_tegra_soc;

pub static HISTOGRAM_GROUPING_POWER: u8 = 3;

/// Default percentile quantiles used for histogram summaries across the
/// exporter, parquet annotation, and viewer.
pub const DEFAULT_PERCENTILES: &[f64] = &[0.5, 0.9, 0.99, 0.999, 0.9999];

// Static metric descriptions extracted from source by build.rs.
// Platform-independent — includes all metrics regardless of target OS.
include!(concat!(env!("OUT_DIR"), "/metric_descriptions.rs"));

/// Returns a map of metric names to their descriptions.
/// Uses the build-time extracted descriptions (platform-independent) as the
/// base, then overlays any additional descriptions from the runtime registry.
pub fn metric_descriptions() -> std::collections::HashMap<String, String> {
    let mut descriptions: std::collections::HashMap<String, String> = static_metric_descriptions()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    // Overlay runtime descriptions (may add metrics not in stats.rs files)
    for (name, description) in registry_descriptions() {
        descriptions.entry(name).or_insert(description);
    }
    descriptions
}

/// Each registered metric name's description, merged across the series that
/// share the name ([`description_merge::merge`]).
pub fn registry_descriptions() -> std::collections::HashMap<String, String> {
    let mut by_name: std::collections::BTreeMap<String, Vec<description_merge::Series>> =
        std::collections::BTreeMap::new();
    for metric in metriken::metrics().iter() {
        if let Some(description) = metric.description() {
            let labels = metric
                .metadata()
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            by_name
                .entry(metric.name().to_string())
                .or_default()
                .push((labels, description.to_string()));
        }
    }
    by_name
        .into_iter()
        .filter_map(|(name, series)| description_merge::merge(&series).map(|d| (name, d)))
        .collect()
}

// Time units with base unit as nanoseconds
pub const SECONDS: u64 = 1_000 * MILLISECONDS;
pub const MILLISECONDS: u64 = 1_000 * MICROSECONDS;
pub const MICROSECONDS: u64 = 1_000 * NANOSECONDS;
pub const NANOSECONDS: u64 = 1;

// Data (IEC) with base unit as bytes - typically used for memory
pub const KIBIBYTES: u64 = 1024 * BYTES;
pub const BYTES: u64 = 1;

// Max attempts to get an 'aligned' UTC and monotonic clock time
const ALIGN_RETRIES: usize = 5;
const MAX_ALIGN_ERROR: Duration = Duration::from_millis(1);

use chrono::{DateTime, Timelike, Utc};
use std::io::Error;
use std::time::{Duration, Instant};

/// Returns a vector of logical CPU IDs for CPUs which are present.
pub fn cpus() -> Result<Vec<usize>, Error> {
    let raw =
        std::fs::read_to_string("/sys/devices/system/cpu/present").map(|v| v.trim().to_string())?;

    let mut ids = Vec::new();

    for range in raw.split(',') {
        let mut parts = range.split('-');

        let first: Option<usize> = parts
            .next()
            .map(|text| text.parse())
            .transpose()
            .map_err(|_| Error::other("could not parse"))?;
        let second: Option<usize> = parts
            .next()
            .map(|text| text.parse())
            .transpose()
            .map_err(|_| Error::other("could not parse"))?;

        if parts.next().is_some() {
            // The line is invalid.
            return Err(Error::other("could not parse"));
        }

        match (first, second) {
            (Some(value), None) => ids.push(value),
            (Some(start), Some(stop)) => ids.extend(start..=stop),
            _ => continue,
        }
    }

    Ok(ids)
}

pub fn aligned_interval(interval: Duration) -> tokio::time::Interval {
    let (utc, now) = utc_instant();

    // get an aligned start time
    let start = now - Duration::from_nanos(utc.nanosecond() as u64) + interval;

    let mut interval = tokio::time::interval_at(start.into(), interval);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    interval
}

pub fn utc_instant() -> (DateTime<Utc>, Instant) {
    for _ in 0..ALIGN_RETRIES {
        let t0 = Instant::now();
        let utc = Utc::now();
        let t1 = Instant::now();

        if t1.duration_since(t0) <= MAX_ALIGN_ERROR {
            return (utc, t0);
        }
    }

    eprintln!("could not get a UTC time and Instant pair");
    std::process::exit(1);
}

/// This function is best-effort detection of if the code is running inside of a
/// virtual machine.
pub fn is_virt() -> bool {
    let sys_vendor = std::fs::read_to_string("/sys/class/dmi/id/sys_vendor")
        .unwrap_or_default()
        .trim()
        .to_string();

    matches!(
        sys_vendor.as_str(),
        "Amazon EC2"
            | "Google"
            | "innotek GmbH"
            | "Microsoft Corporation"
            | "QEMU"
            | "Red Hat"
            | "VMware, Inc."
            | "Xen"
    )
}

#[cfg(test)]
mod tests {
    /// #1454: a name whose series describe different things carries every
    /// series' description, from the registry (what `/metrics/descriptions`
    /// serves) and from the build-time table alike. `ext4_journal_commit_latency`
    /// is registered on every platform (its `stats.rs` is included off Linux).
    #[test]
    fn a_name_with_several_descriptions_keeps_each_one() {
        let registry = super::registry_descriptions();
        let from_registry = registry
            .get("ext4_journal_commit_latency")
            .expect("ext4_journal_commit_latency is registered");
        for phase in ["flushing", "locked", "logging", "running"] {
            assert!(
                from_registry.contains(&format!("phase={phase}: ")),
                "{phase} missing from {from_registry}"
            );
        }
        let from_table = super::static_metric_descriptions()
            .iter()
            .find(|(name, _)| *name == "ext4_journal_commit_latency")
            .map(|(_, d)| *d)
            .expect("in the build-time table");
        assert_eq!(from_table, from_registry, "build.rs and the registry agree");
    }

    /// Every description a registered series carries appears whole in the
    /// build-time table's entry for its name. The table also holds other
    /// platforms' series, so it can hold more than the registry, never less.
    #[test]
    fn the_build_time_table_carries_every_registered_description() {
        let table: std::collections::HashMap<&str, &str> = super::static_metric_descriptions()
            .iter()
            .copied()
            .collect();
        let mut checked = 0;
        for metric in metriken::metrics().iter() {
            let (Some(description), Some(entry)) = (metric.description(), table.get(metric.name()))
            else {
                continue;
            };
            let description = description.trim_end().trim_end_matches('.');
            assert!(
                entry.contains(description),
                "{}: the table's entry {entry:?} lacks {description:?}",
                metric.name()
            );
            checked += 1;
        }
        assert!(checked > 100, "checked {checked} series");
    }
}
