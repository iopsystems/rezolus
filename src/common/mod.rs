mod counters;
mod gauges;

use chrono::{Timelike, Utc};
pub use counters::*;
pub use gauges::*;
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

pub fn aligned_interval(interval: Duration) -> tokio::time::Interval {
    // get an aligned start time
    let start = tokio::time::Instant::now() - Duration::from_nanos(Utc::now().nanosecond() as u64)
        + interval;

    tokio::time::interval_at(start, interval)
}
