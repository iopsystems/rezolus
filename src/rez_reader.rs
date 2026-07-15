//! `RezReader`: reads a `.rez` archive as a unified `metriken_query::MetricsSource`
//! by composing one `ParquetReader` per per-sampler table.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use metriken_query::{BufferPool, MetricsSource, ParquetReader, QueryError, QueryResult};

use crate::recorder::rez::{self, RecordingBytes};

/// One opened per-sampler table.
struct SamplerReader {
    sampler: String,
    reader: ParquetReader,
}

/// A `.rez` archive presented as one `MetricsSource`. Phase B: a single
/// recording; every recording's tables are flattened into `tables`
/// (multi-recording faceting is Phase C).
pub struct RezReader {
    tables: Vec<SamplerReader>,
    /// The (first) recording's file-level metadata, for `source`/`version`/etc.
    metadata: BTreeMap<String, String>,
    filename: Option<String>,
}

impl RezReader {
    /// Open a `.rez` at `path`, opening each per-sampler table against `pool`.
    pub fn open_with_pool(
        path: &Path,
        pool: Arc<BufferPool>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let (_manifest, recordings) = rez::read_archive_bytes(path)?;
        let filename = path.file_name().map(|s| s.to_string_lossy().into_owned());
        Self::from_recordings(recordings, filename, pool)
    }

    /// Open a `.rez` from in-memory bytes (upload mode).
    pub fn open_bytes_with_pool(
        bytes: Vec<u8>,
        filename: Option<String>,
        pool: Arc<BufferPool>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let (_manifest, recordings) = rez::read_archive_reader(std::io::Cursor::new(bytes))?;
        Self::from_recordings(recordings, filename, pool)
    }

    fn from_recordings(
        recordings: Vec<RecordingBytes>,
        filename: Option<String>,
        pool: Arc<BufferPool>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let metadata = recordings
            .first()
            .map(|r| r.metadata.clone())
            .unwrap_or_default();
        let mut tables = Vec::new();
        for rec in recordings {
            for (sampler, bytes) in rec.tables {
                let reader = ParquetReader::open_bytes_with_pool(bytes, Arc::clone(&pool))
                    .map_err(|e| format!("opening table {sampler}: {e}"))?;
                tables.push(SamplerReader { sampler, reader });
            }
        }
        Ok(Self {
            tables,
            metadata,
            filename,
        })
    }
}

impl MetricsSource for RezReader {
    // ── Query methods: implemented in Task 3. Until then, a clear error. ──
    fn query_range(
        &self,
        _expr: &str,
        _start_s: f64,
        _end_s: f64,
        _step_s: f64,
    ) -> Result<QueryResult, QueryError> {
        Err(QueryError::ParseError("rez query routing not yet wired".into()))
    }
    fn query(&self, _expr: &str, _time: Option<f64>) -> Result<QueryResult, QueryError> {
        Err(QueryError::ParseError("rez query routing not yet wired".into()))
    }
    fn columns(&self, _query: &str) -> Result<HashSet<String>, QueryError> {
        Err(QueryError::ParseError("rez query routing not yet wired".into()))
    }

    // ── Union metadata / naming / labels ──
    fn counter_names(&self) -> Vec<String> {
        union_sorted(self.tables.iter().map(|t| t.reader.counter_names()))
    }
    fn gauge_names(&self) -> Vec<String> {
        union_sorted(self.tables.iter().map(|t| t.reader.gauge_names()))
    }
    fn histogram_names(&self) -> Vec<String> {
        union_sorted(self.tables.iter().map(|t| t.reader.histogram_names()))
    }
    fn counter_labels(&self, name: &str) -> Vec<BTreeMap<String, String>> {
        self.tables.iter().flat_map(|t| t.reader.counter_labels(name)).collect()
    }
    fn gauge_labels(&self, name: &str) -> Vec<BTreeMap<String, String>> {
        self.tables.iter().flat_map(|t| t.reader.gauge_labels(name)).collect()
    }
    fn histogram_labels(&self, name: &str) -> Vec<BTreeMap<String, String>> {
        self.tables.iter().flat_map(|t| t.reader.histogram_labels(name)).collect()
    }

    // ── Time / interval: union extent, finest interval ──
    fn time_range(&self) -> Option<(f64, f64)> {
        self.tables
            .iter()
            .filter_map(|t| t.reader.time_range())
            .reduce(|(a0, a1), (b0, b1)| (a0.min(b0), a1.max(b1)))
    }
    fn time_range_ns(&self) -> Option<(u64, u64)> {
        self.tables
            .iter()
            .filter_map(|t| t.reader.time_range_ns())
            .reduce(|(a0, a1), (b0, b1)| (a0.min(b0), a1.max(b1)))
    }
    fn interval(&self) -> f64 {
        let finest = self
            .tables
            .iter()
            .map(|t| t.reader.interval())
            .filter(|i| *i > 0.0)
            .fold(f64::INFINITY, f64::min);
        if finest.is_finite() {
            finest
        } else {
            1.0
        }
    }

    // ── File-level metadata from the recording manifest ──
    fn source(&self) -> String {
        self.metadata.get("source").cloned().unwrap_or_default()
    }
    fn version(&self) -> String {
        self.metadata.get("version").cloned().unwrap_or_default()
    }
    fn filename(&self) -> Option<String> {
        self.filename.clone()
    }
    fn metadata_get(&self, key: &str) -> Option<String> {
        self.metadata.get(key).cloned()
    }
    fn file_metadata(&self) -> HashMap<String, String> {
        self.metadata.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    }
}

fn union_sorted(iters: impl Iterator<Item = Vec<String>>) -> Vec<String> {
    let mut set: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for v in iters {
        set.extend(v);
    }
    set.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recorder::rez::RezRecorder;
    use metriken::Window;
    use metriken_exposition::{Counter, Snapshot, SnapshotV2};
    use std::time::SystemTime;

    fn counter(name: &str, sampler: &str, v: u64, w: Option<Window>) -> Counter {
        Counter {
            name: name.to_string(),
            value: v,
            metadata: [
                ("metric".to_string(), name.to_string()),
                ("sampler".to_string(), sampler.to_string()),
            ]
            .into_iter()
            .collect(),
            window: w,
        }
    }

    fn snap(ts: u64, counters: Vec<Counter>) -> Snapshot {
        Snapshot::V2(SnapshotV2 {
            systemtime: SystemTime::UNIX_EPOCH + std::time::Duration::from_nanos(ts),
            duration: std::time::Duration::ZERO,
            metadata: HashMap::new(),
            counters,
            gauges: Vec::new(),
            histograms: Vec::new(),
        })
    }

    /// Build a 2-sampler .rez fixture on disk; return (tempdir, path).
    pub(super) fn two_sampler_rez() -> (tempfile::TempDir, std::path::PathBuf) {
        let mut r = RezRecorder::new(
            [("source".to_string(), "rezolus".to_string())].into_iter().collect(),
            [("source".to_string(), "rezolus".to_string())].into_iter().collect(),
            "rezolus".to_string(),
        );
        for i in 0..3u64 {
            let end = 1_000 + i * 100;
            r.ingest(
                &snap(
                    1_000 + i,
                    vec![
                        counter("cpu_cycles", "cpu_usage", i, Some(Window::new(end - 50, end))),
                        counter("reads", "blockio_requests", i, Some(Window::new(end - 50, end))),
                    ],
                ),
                1_000 + i,
            );
        }
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("two.rez");
        r.finalize(&out).unwrap();
        (dir, out)
    }

    #[test]
    fn union_names_across_samplers() {
        let (_d, path) = two_sampler_rez();
        let pool = BufferPool::new(64 * 1024 * 1024);
        let reader = RezReader::open_with_pool(&path, pool).unwrap();
        let mut names = reader.counter_names();
        names.sort();
        assert_eq!(names, vec!["cpu_cycles".to_string(), "reads".to_string()]);
        assert!(!names.iter().any(|n| n.contains(":window")));
    }

    #[test]
    fn source_from_manifest_metadata() {
        let (_d, path) = two_sampler_rez();
        let pool = BufferPool::new(64 * 1024 * 1024);
        let reader = RezReader::open_with_pool(&path, pool).unwrap();
        assert_eq!(reader.source(), "rezolus");
    }
}
