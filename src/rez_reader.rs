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

    /// Open a `.rez` as one `RezReader` **per recording**, paired with that
    /// recording's labels. Used by the viewer to map a 2-recording `.rez` onto
    /// baseline/experiment without cross-recording sampler-name collisions.
    pub fn open_recordings(
        path: &Path,
        pool: Arc<BufferPool>,
    ) -> Result<Vec<(BTreeMap<String, String>, RezReader)>, Box<dyn std::error::Error>> {
        let (_manifest, recordings) = rez::read_archive_bytes(path)?;
        let mut out = Vec::with_capacity(recordings.len());
        for rec in recordings {
            let labels = rec.labels.clone();
            let filename = Some(rec.dir.clone());
            let reader = Self::from_recordings(vec![rec], filename, Arc::clone(&pool))?;
            out.push((labels, reader));
        }
        Ok(out)
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

    /// Sub-readers whose `columns(query)` is non-empty (own ≥1 referenced metric).
    fn owners(&self, query: &str) -> Result<Vec<&SamplerReader>, QueryError> {
        let mut out = Vec::new();
        for t in &self.tables {
            if !t.reader.columns(query)?.is_empty() {
                out.push(t);
            }
        }
        Ok(out)
    }

    /// Resolve the single sub-reader that owns every metric a query references.
    /// Errors clearly when a query spans two samplers (cross-timeline alignment
    /// is a later phase) or references no known metric.
    fn route(&self, query: &str) -> Result<&SamplerReader, QueryError> {
        let owners = self.owners(query)?;
        match owners.as_slice() {
            [one] => Ok(one),
            [] => Err(QueryError::ParseError(format!(
                "query references no metric present in this .rez: {query}"
            ))),
            many => {
                let mut samplers: Vec<&str> = many.iter().map(|t| t.sampler.as_str()).collect();
                samplers.sort();
                Err(QueryError::ParseError(format!(
                    "cross-timeline query spans samplers {} — per-sampler alignment \
                     (interpolate/decimate) is not yet supported; query one sampler at a time",
                    samplers.join(", ")
                )))
            }
        }
    }
}

impl MetricsSource for RezReader {
    // ── Query methods: route to the sub-reader owning the referenced metrics. ──
    fn query_range(
        &self,
        expr: &str,
        start_s: f64,
        end_s: f64,
        step_s: f64,
    ) -> Result<QueryResult, QueryError> {
        self.route(expr)?
            .reader
            .query_range(expr, start_s, end_s, step_s)
    }
    fn query(&self, expr: &str, time: Option<f64>) -> Result<QueryResult, QueryError> {
        self.route(expr)?.reader.query(expr, time)
    }
    fn columns(&self, query: &str) -> Result<HashSet<String>, QueryError> {
        // columns() is answerable as the union — it never crosses timelines.
        let mut out = HashSet::new();
        for t in &self.tables {
            out.extend(t.reader.columns(query)?);
        }
        Ok(out)
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
        self.tables
            .iter()
            .flat_map(|t| t.reader.counter_labels(name))
            .collect()
    }
    fn gauge_labels(&self, name: &str) -> Vec<BTreeMap<String, String>> {
        self.tables
            .iter()
            .flat_map(|t| t.reader.gauge_labels(name))
            .collect()
    }
    fn histogram_labels(&self, name: &str) -> Vec<BTreeMap<String, String>> {
        self.tables
            .iter()
            .flat_map(|t| t.reader.histogram_labels(name))
            .collect()
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
        self.metadata
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
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
    use metriken_exposition::{Counter, Gauge, Snapshot, SnapshotV2};
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

    fn gauge(name: &str, sampler: &str, v: i64, w: Option<Window>) -> Gauge {
        Gauge {
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

    fn snap(ts: u64, counters: Vec<Counter>, gauges: Vec<Gauge>) -> Snapshot {
        Snapshot::V2(SnapshotV2 {
            systemtime: SystemTime::UNIX_EPOCH + std::time::Duration::from_nanos(ts),
            duration: std::time::Duration::ZERO,
            metadata: HashMap::new(),
            counters,
            gauges,
            histograms: Vec::new(),
        })
    }

    /// Build a 2-sampler .rez fixture on disk; return (tempdir, path).
    pub(super) fn two_sampler_rez() -> (tempfile::TempDir, std::path::PathBuf) {
        let mut r = RezRecorder::new(
            [("source".to_string(), "rezolus".to_string())]
                .into_iter()
                .collect(),
            [("source".to_string(), "rezolus".to_string())]
                .into_iter()
                .collect(),
            "rezolus".to_string(),
        );
        for i in 0..3u64 {
            // Seconds-scale timestamps (1s, 2s, 3s) so query-engine time handling
            // is well-behaved; windows advance each poll → 3 rows per sampler.
            let ts = 1_000_000_000 * (i + 1);
            let w = Some(Window::new(ts - 50_000_000, ts));
            r.ingest(
                &snap(
                    ts,
                    vec![
                        counter("cpu_cycles", "cpu_usage", i, w),
                        counter("reads", "blockio_requests", i, w),
                    ],
                    // A gauge in cpu_usage: bare gauge selectors are valid instant
                    // vectors, so the delegation test can actually evaluate.
                    vec![gauge("frequency", "cpu_usage", 2_000 + i as i64, w)],
                ),
                ts,
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

    #[test]
    fn single_sampler_query_delegates() {
        let (_d, path) = two_sampler_rez();
        let pool = BufferPool::new(64 * 1024 * 1024);
        let reader = RezReader::open_with_pool(&path, pool).unwrap();
        let (start, end) = reader.time_range().unwrap();
        // "frequency" is a gauge in the cpu_usage table only → routes there and
        // resolves (bare gauge selectors are valid instant vectors; a bare
        // counter would need rate()). columns() also finds it via that reader.
        let cols = reader.columns("frequency").unwrap();
        assert!(cols.iter().any(|c| c.contains("frequency")));
        let r = reader.query_range("frequency", start, end + 1.0, 1.0);
        assert!(
            r.is_ok(),
            "single-sampler gauge query should succeed: {r:?}"
        );
    }

    #[test]
    fn open_recordings_returns_one_reader_per_recording() {
        // Build a 2-recording .rez by reading a 1-recording fixture and writing
        // it twice under distinct dirs/arms via write_archive_bytes.
        let (_d, p) = two_sampler_rez();
        let (m, rb) = crate::recorder::rez::read_archive_bytes(&p).unwrap();
        let rec0 = m.recordings.into_iter().next().unwrap();
        let bytes0: Vec<Vec<u8>> = rb
            .into_iter()
            .next()
            .unwrap()
            .tables
            .into_iter()
            .map(|(_, b)| b)
            .collect();

        let mut a = rec0.clone();
        a.dir = "arm0".to_string();
        a.labels.insert("arm".to_string(), "arm0".to_string());
        let mut b = rec0.clone();
        b.dir = "arm1".to_string();
        b.labels.insert("arm".to_string(), "arm1".to_string());

        let d = tempfile::tempdir().unwrap();
        let out = d.path().join("two_rec.rez");
        crate::recorder::rez::write_archive_bytes(&out, &[(a, bytes0.clone()), (b, bytes0)])
            .unwrap();

        let pool = BufferPool::new(64 * 1024 * 1024);
        let readers = RezReader::open_recordings(&out, pool).unwrap();
        assert_eq!(readers.len(), 2);
        assert_eq!(readers[0].0.get("arm").map(String::as_str), Some("arm0"));
        assert_eq!(readers[1].0.get("arm").map(String::as_str), Some("arm1"));
        assert!(!readers[0].1.counter_names().is_empty());
    }

    #[test]
    fn cross_sampler_query_errors_naming_both() {
        let (_d, path) = two_sampler_rez();
        let pool = BufferPool::new(64 * 1024 * 1024);
        let reader = RezReader::open_with_pool(&path, pool).unwrap();
        // cpu_cycles (cpu_usage) and reads (blockio_requests) live in different
        // tables; a query spanning both must error, naming both samplers.
        let err = reader
            .query_range("cpu_cycles + reads", 0.0, 10.0, 1.0)
            .unwrap_err();
        let msg = format!("{err:?}");
        assert!(
            msg.contains("cpu_usage") && msg.contains("blockio_requests"),
            "got: {msg}"
        );
    }
}
