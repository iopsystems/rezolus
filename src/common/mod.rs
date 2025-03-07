mod counters;
mod gauges;

pub use counters::*;
pub use gauges::*;

use chrono::{DateTime, Timelike, Utc};
use tokio::time::Instant;

use std::time::Duration;

#[cfg(target_os = "linux")]
pub mod bpf;

#[cfg(target_os = "linux")]
pub use bpf::*;

#[cfg(target_os = "linux")]
pub mod linux;

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

pub fn aligned_interval(interval: Duration) -> tokio::time::Interval {
    let (utc, now) = utc_instant();

    // get an aligned start time
    let start = now - Duration::from_nanos(utc.nanosecond() as u64) + interval;

    tokio::time::interval_at(start, interval)
}

pub fn utc_instant() -> (DateTime<Utc>, Instant) {
    for _ in 0..ALIGN_RETRIES {
        let t0 = Instant::now();
        let utc = Utc::now();
        let t1 = Instant::now();

        if t1.duration_since(t0).as_millis() <= 1 {
            return (utc, t0);
        }
    }

    eprintln!("could not get a UTC time and Instant pair");
    std::process::exit(1);
}
