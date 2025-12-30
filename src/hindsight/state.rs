use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};
use tokio::sync::oneshot;

/// Shared state between sampling loop and HTTP handlers
#[allow(dead_code)]
pub struct SharedState {
    /// Path to the temporary ring buffer file
    pub temp_path: PathBuf,
    /// Size of each snapshot slot in bytes (aligned to 4KB)
    pub snapshot_len: u64,
    /// Total number of snapshot slots in the ring buffer
    pub snapshot_count: u64,
    /// Sampling interval
    pub interval: Duration,
    /// Total buffer duration
    pub duration: Duration,
    /// Path for output file (used by dump-to-file)
    pub output_path: PathBuf,

    /// Current write index (next position to write to)
    /// Updated atomically by the sampling loop after each write
    idx: AtomicU64,
    /// Total number of snapshots written since startup
    /// Used to determine if buffer has wrapped
    snapshots_written: AtomicU64,
}

impl SharedState {
    pub fn new(
        temp_path: PathBuf,
        snapshot_len: u64,
        snapshot_count: u64,
        interval: Duration,
        duration: Duration,
        output_path: PathBuf,
    ) -> Self {
        Self {
            temp_path,
            snapshot_len,
            snapshot_count,
            interval,
            duration,
            output_path,
            idx: AtomicU64::new(0),
            snapshots_written: AtomicU64::new(0),
        }
    }

    /// Get the current write index
    pub fn idx(&self) -> u64 {
        self.idx.load(Ordering::SeqCst)
    }

    /// Update the write index after a successful write
    pub fn advance_idx(&self) {
        let mut idx = self.idx.load(Ordering::SeqCst) + 1;
        if idx >= self.snapshot_count {
            idx = 0;
        }
        self.idx.store(idx, Ordering::SeqCst);
        self.snapshots_written.fetch_add(1, Ordering::SeqCst);
    }

    /// Get the total number of snapshots written
    pub fn snapshots_written(&self) -> u64 {
        self.snapshots_written.load(Ordering::SeqCst)
    }

    /// Check if the buffer has been filled at least once
    #[allow(dead_code)]
    pub fn buffer_filled(&self) -> bool {
        self.snapshots_written() >= self.snapshot_count
    }

    /// Get the number of valid snapshots in the buffer
    pub fn valid_snapshot_count(&self) -> u64 {
        self.snapshots_written().min(self.snapshot_count)
    }
}

/// Time range filter for dump operations
#[derive(Debug, Clone, Default)]
pub struct TimeRange {
    pub start: Option<SystemTime>,
    pub end: Option<SystemTime>,
}

impl TimeRange {
    pub fn new(start: Option<SystemTime>, end: Option<SystemTime>) -> Self {
        Self { start, end }
    }

    /// Check if a timestamp falls within this range
    pub fn contains(&self, timestamp: SystemTime) -> bool {
        if let Some(start) = self.start {
            if timestamp < start {
                return false;
            }
        }
        if let Some(end) = self.end {
            if timestamp > end {
                return false;
            }
        }
        true
    }
}

/// Request to dump the ring buffer to the configured output file
pub struct DumpToFileRequest {
    pub time_range: TimeRange,
    pub response_tx: oneshot::Sender<DumpToFileResponse>,
}

/// Response from a dump-to-file operation
#[derive(Debug)]
pub struct DumpToFileResponse {
    pub path: PathBuf,
    pub snapshots: u64,
    pub start_time: Option<u64>,
    pub end_time: Option<u64>,
    pub error: Option<String>,
}

impl DumpToFileResponse {
    pub fn success(path: PathBuf, snapshots: u64, start_time: Option<u64>, end_time: Option<u64>) -> Self {
        Self {
            path,
            snapshots,
            start_time,
            end_time,
            error: None,
        }
    }

    pub fn error(error: String) -> Self {
        Self {
            path: PathBuf::new(),
            snapshots: 0,
            start_time: None,
            end_time: None,
            error: Some(error),
        }
    }
}
