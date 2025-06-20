pub static HISTOGRAM_GROUPING_POWER: u8 = 3;

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
