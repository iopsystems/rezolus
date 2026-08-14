use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::oneshot;

use super::buffer::Summary;

/// Shared state between the sampling loop and the HTTP handlers.
///
/// What used to live here was the ring's geometry — slot size, slot count, the
/// write index, and how many writes had happened so the handlers could tell a
/// wrapped buffer from a partly-filled one. None of it has a v3 analogue: the
/// buffer is a `.rez` file, and every question the handlers used to answer
/// from these fields is now answered by reading its catalog
/// ([`super::buffer::summarize`]), which any process can do at any time.
///
/// What remains is what the file cannot tell you: where it is, where a dump
/// should go, and the configuration the loop is running at.
pub struct SharedState {
    /// The buffer `.rez` the loop is writing.
    pub buffer_path: PathBuf,
    /// Where `/dump/file` and SIGHUP write the buffer out.
    pub output_path: PathBuf,
    /// Sampling interval.
    pub interval: Duration,
    /// How far back the buffer is asked to reach — `[general] duration`.
    pub lookback: Duration,
    /// Snapshots pulled from the agent and ingested since startup. Not a ring
    /// position and not a capacity: just how much work the loop has done.
    ticks: AtomicU64,
    /// Whether retention has begun. Published by the loop rather than derived
    /// from the file: the buffer knows the span it has COVERED, and eviction
    /// keeps the span it RETAINS permanently just under the lookback — see
    /// [`super::buffer::HindsightBuffer::at_retention_bound`].
    at_retention_bound: AtomicBool,
}

impl SharedState {
    pub fn new(
        buffer_path: PathBuf,
        output_path: PathBuf,
        interval: Duration,
        lookback: Duration,
    ) -> Self {
        Self {
            buffer_path,
            output_path,
            interval,
            lookback,
            ticks: AtomicU64::new(0),
            at_retention_bound: AtomicBool::new(false),
        }
    }

    pub fn record_tick(&self) {
        self.ticks.fetch_add(1, Ordering::Relaxed);
    }

    pub fn ticks(&self) -> u64 {
        self.ticks.load(Ordering::Relaxed)
    }

    pub fn set_at_retention_bound(&self, at_bound: bool) {
        self.at_retention_bound.store(at_bound, Ordering::Relaxed);
    }

    pub fn at_retention_bound(&self) -> bool {
        self.at_retention_bound.load(Ordering::Relaxed)
    }
}

/// Time range filter for dump operations.
#[derive(Debug, Clone, Default)]
pub struct TimeRange {
    pub start: Option<SystemTime>,
    pub end: Option<SystemTime>,
}

impl TimeRange {
    pub fn new(start: Option<SystemTime>, end: Option<SystemTime>) -> Self {
        Self { start, end }
    }

    /// The range as row stamps. A `.rez` row is stamped `anchor + monotonic
    /// elapsed` where the anchor is a wall-clock reading, so nanoseconds since
    /// the epoch is the comparable form. A time before the epoch clamps to 0,
    /// which selects everything — the same thing it meant as a filter.
    pub fn start_ns(&self) -> Option<u64> {
        self.start.map(to_ns)
    }

    pub fn end_ns(&self) -> Option<u64> {
        self.end.map(to_ns)
    }
}

fn to_ns(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos().min(u64::MAX as u128) as u64)
        .unwrap_or(0)
}

/// Request to dump the buffer to the configured output file.
pub struct DumpToFileRequest {
    pub time_range: TimeRange,
    pub response_tx: oneshot::Sender<DumpToFileResponse>,
}

/// Response from a dump-to-file operation.
#[derive(Debug)]
pub struct DumpToFileResponse {
    pub path: PathBuf,
    /// What the dump actually holds. Note the span may reach further back than
    /// the request asked for: a dump trims at segment granularity, so the
    /// caller is told what it got rather than what it asked for.
    pub summary: Option<Summary>,
    pub error: Option<String>,
}

impl DumpToFileResponse {
    pub fn success(path: PathBuf, summary: Summary) -> Self {
        Self {
            path,
            summary: Some(summary),
            error: None,
        }
    }

    pub fn error(error: String) -> Self {
        Self {
            path: PathBuf::new(),
            summary: None,
            error: Some(error),
        }
    }
}
