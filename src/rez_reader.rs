//! `RezReader`: reads a `.rez` archive as a unified `metriken_query::MetricsSource`
//! by composing one sub-source per per-sampler table — a `ParquetReader` for a
//! single-segment table, a `SegmentedParquetReader` for one the streaming
//! writer sealed more than once.
//!
//! Both containers land here. A v2 tar archive resolves to its per-sampler
//! parquet blobs; a v3 SQLite archive resolves to its sealed segment BLOBs
//! **plus a newest segment materialized from the live WAL** — see
//! [`materialize_wal_tail`]. From that point on the two are the same
//! `Vec<Vec<u8>>` per table and nothing below this file knows which container
//! it came from.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use metriken_query::{
    BufferPool, MetricsSource, ParquetReader, QueryError, QueryOptions, QueryResult,
    SegmentedParquetReader,
};

use crate::recorder::rez::{self, RecordingBytes};
use crate::recorder::rez_sqlite::RezDb;
use crate::recorder::rez_v3_writer::materialize_wal_tail;

/// One opened per-sampler table. A table is one or more parquet segments, so
/// the backing source is either a plain `ParquetReader` (single segment) or a
/// `SegmentedParquetReader` (many) — both are `MetricsSource`, and everything
/// below this point treats them identically.
struct SamplerReader {
    sampler: String,
    reader: Box<dyn MetricsSource>,
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

/// One `RezReader` per recording, paired with that recording's label set.
type LabeledRecordings = Vec<(BTreeMap<String, String>, RezReader)>;

impl RezReader {
    /// Open a `.rez` at `path`, opening each per-sampler table against `pool`.
    pub fn open_with_pool(
        path: &Path,
        pool: Arc<BufferPool>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let recordings = read_recordings(path)?;
        let filename = path.file_name().map(|s| s.to_string_lossy().into_owned());
        Self::from_recordings(recordings, filename, pool)
    }

    /// Open a `.rez` as one `RezReader` **per recording**, paired with that
    /// recording's labels. Used by the viewer to map a 2-recording `.rez` onto
    /// baseline/experiment without cross-recording sampler-name collisions.
    pub fn open_recordings(
        path: &Path,
        pool: Arc<BufferPool>,
    ) -> Result<LabeledRecordings, Box<dyn std::error::Error>> {
        let recordings = read_recordings(path)?;
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
            if !rec.complete {
                tracing::warn!(
                    "recording {} was not cleanly finalized; it was recovered up to its \
                     last checkpoint and data after that may be missing",
                    rec.dir
                );
            }
            for (sampler, segments) in rec.tables {
                // A single-segment table keeps the plain reader: the streaming
                // writer's slow samplers and every atomically written archive
                // land here, and there is nothing for the splice to do.
                // Multi-segment tables go to the segment-aware source, which
                // splices raw samples below PromQL evaluation so a `rate()`
                // window straddling a seal boundary still computes on complete
                // data. Both open footer-only against the shared pool.
                let reader: Box<dyn MetricsSource> = match <[Vec<u8>; 1]>::try_from(segments) {
                    Ok([bytes]) => Box::new(
                        ParquetReader::open_bytes_with_pool(bytes, Arc::clone(&pool))
                            .map_err(|e| format!("opening table {sampler}: {e}"))?,
                    ),
                    Err(segments) => Box::new(
                        SegmentedParquetReader::open_bytes_with_pool(segments, Arc::clone(&pool))
                            .map_err(|e| format!("opening table {sampler}: {e}"))?,
                    ),
                };
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
    fn query_range_opts(
        &self,
        expr: &str,
        start_s: f64,
        end_s: f64,
        step_s: f64,
        opts: &QueryOptions,
    ) -> Result<QueryResult, QueryError> {
        self.route(expr)?
            .reader
            .query_range_opts(expr, start_s, end_s, step_s, opts)
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

/// Read either container into the one shape the reader consumes: per
/// recording, `(sampler, segments-newest-last)`.
///
/// Dispatch is by CONTENT (`detect_rez_format`), not by extension, and the
/// non-v3 arm deliberately falls through to `read_archive_bytes` unchanged —
/// including for `NotRez`, so a caller handed something that is not a `.rez`
/// at all keeps getting the tar reader's own error rather than a new one.
fn read_recordings(path: &Path) -> Result<Vec<RecordingBytes>, Box<dyn std::error::Error>> {
    match rez::detect_rez_format(path)? {
        rez::RezFormat::V3Sqlite => read_v3_recordings(path),
        rez::RezFormat::V2Tar | rez::RezFormat::NotRez => Ok(rez::read_archive_bytes(path)?.1),
    }
}

/// Resolve a v3 (SQLite) `.rez` into the same `RecordingBytes` the tar reader
/// produces, so everything downstream is container-agnostic.
///
/// Two things differ from a mechanical transcription of the catalog:
///
/// * Tables are enumerated with `all_samplers`, NOT `samplers`. The latter
///   sees only `segments`, so a table still inside its first seal period —
///   16 of 26 in the fleet measurement that motivated this container — would
///   be invisible, which is precisely the data v3 exists to keep.
/// * Each table's live WAL tail is materialized into an in-memory parquet
///   segment and appended as the NEWEST segment. `live_wal`'s watermark
///   (`ts > MAX(last_ts)` of that sampler's own segments) is what guarantees
///   the seam has no duplicate row, so nothing here has to de-duplicate.
fn read_v3_recordings(path: &Path) -> Result<Vec<RecordingBytes>, Box<dyn std::error::Error>> {
    let db = RezDb::open(path)?;
    let mut out = Vec::new();
    for rec in db.read_recordings()? {
        let mut tables = Vec::new();
        for sampler in db.all_samplers(rec.id)? {
            let segments = table_segments(&db, rec.id, &sampler)?;
            // Only reachable if a sampler's every WAL row was pruned without
            // its segment landing — which the seal ordering rules out. A table
            // with no bytes has nothing to open, so skip rather than hand the
            // reader an empty segment list.
            if segments.is_empty() {
                continue;
            }
            tables.push((sampler, segments));
        }
        out.push(RecordingBytes {
            // v3 has no tar directory. `dir` survives only as a display name,
            // and this is the function that produced it in the first place.
            dir: rez::recording_dir_slug(&rec.meta.labels),
            labels: rec.meta.labels,
            metadata: rec.meta.metadata,
            complete: rec.complete,
            tables,
        });
    }
    Ok(out)
}

/// One sampler's parquet segments, oldest first: its sealed segments in `seq`
/// order, then its live WAL tail materialized as the newest segment.
///
/// `live_wal`, NOT `read_wal`: the watermark (`ts > MAX(last_ts)` over that
/// sampler's own segments) is the only thing keeping the seam free of
/// duplicates. The prune runs outside the seal transaction, so `wal` routinely
/// still holds rows a sealed segment already covers; replaying the raw table
/// would splice those rows in a second time.
fn table_segments(
    db: &RezDb,
    recording_id: i64,
    sampler: &str,
) -> Result<Vec<Vec<u8>>, Box<dyn std::error::Error>> {
    let mut segments: Vec<Vec<u8>> = db
        .read_segments(recording_id, sampler)?
        .into_iter()
        .map(|s| s.bytes)
        .collect();
    if let Some(tail) = materialize_wal_tail(sampler, &db.live_wal(recording_id, sampler)?)? {
        segments.push(tail);
    }
    Ok(segments)
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
        Counter::new(
            name.to_string(),
            v,
            [
                ("metric".to_string(), name.to_string()),
                ("sampler".to_string(), sampler.to_string()),
            ]
            .into_iter()
            .collect(),
        )
        .with_window(w)
    }

    fn gauge(name: &str, sampler: &str, v: i64, w: Option<Window>) -> Gauge {
        Gauge::new(
            name.to_string(),
            v,
            [
                ("metric".to_string(), name.to_string()),
                ("sampler".to_string(), sampler.to_string()),
            ]
            .into_iter()
            .collect(),
        )
        .with_window(w)
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

    /// The fixture's rows as `(snapshot, timestamp)`: two samplers
    /// (`cpu_usage` = the `cpu_cycles` counter plus the `frequency` gauge,
    /// `blockio_requests` = the `reads` counter), one row per second.
    ///
    /// Shared by the atomic and streaming builders below so a segmented archive
    /// can be compared against a single-segment one holding the *same* rows.
    fn fixture_rows(n: u64) -> Vec<(Snapshot, u64)> {
        (0..n)
            .map(|i| {
                // Seconds-scale timestamps (1s, 2s, ...) so query-engine time
                // handling is well-behaved; windows advance each poll → one row
                // per sampler per poll.
                let ts = 1_000_000_000 * (i + 1);
                let w = Some(Window::new(ts - 50_000_000, ts));
                (
                    snap(
                        ts,
                        vec![
                            counter("cpu_cycles", "cpu_usage", i * 1_000, w),
                            counter("reads", "blockio_requests", i, w),
                        ],
                        // A gauge in cpu_usage: bare gauge selectors are valid
                        // instant vectors, so the delegation test can actually
                        // evaluate.
                        vec![gauge("frequency", "cpu_usage", 2_000 + i as i64, w)],
                    ),
                    ts,
                )
            })
            .collect()
    }

    fn rez_labels() -> BTreeMap<String, String> {
        [("source".to_string(), "rezolus".to_string())]
            .into_iter()
            .collect()
    }

    /// Write `rows` as a single-segment archive (the atomic writer).
    fn write_atomic_rez(rows: &[(Snapshot, u64)], out: &std::path::Path) {
        let mut r = RezRecorder::new(rez_labels(), rez_labels(), "rezolus".to_string());
        for (s, ts) in rows {
            r.ingest(s, *ts);
        }
        r.finalize(out).unwrap();
    }

    /// Write the same `rows` through the streaming writer with a tiny row cap,
    /// so every table seals into several segments.
    fn write_streamed_rez(rows: &[(Snapshot, u64)], max_rows: usize, out: &std::path::Path) {
        use crate::recorder::rez_stream::{
            ManifestSeed, RezWriterHandle, SealPolicy, StreamRecorder,
        };

        let handle = RezWriterHandle::create(
            out,
            ManifestSeed {
                dir: "rezolus".to_string(),
                labels: rez_labels(),
                metadata: rez_labels(),
                clock_anchor_wall_ns: 1_700_000_000_000_000_000,
            },
        )
        .unwrap();
        let mut rec = StreamRecorder::with_policy(
            handle,
            SealPolicy {
                max_bytes: usize::MAX,
                max_rows,
                max_age: std::time::Duration::from_secs(3600),
            },
        );
        let mut last_ts = 0;
        for (s, ts) in rows {
            rec.ingest(s, *ts, 0);
            rec.maybe_seal().unwrap();
            last_ts = *ts;
        }
        rec.finalize((last_ts, 0)).unwrap();
    }

    /// `sampler -> segment count` for a written archive.
    fn segment_counts(path: &std::path::Path) -> BTreeMap<String, usize> {
        let (manifest, _) = crate::recorder::rez::read_archive_bytes(path).unwrap();
        manifest.recordings[0]
            .tables
            .iter()
            .map(|t| (t.sampler.clone(), t.segment_files().len()))
            .collect()
    }

    /// Build a 2-sampler .rez fixture on disk; return (tempdir, path).
    pub(super) fn two_sampler_rez() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("two.rez");
        write_atomic_rez(&fixture_rows(3), &out);
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

    /// A single-sampler `.rez` holding one histogram, `n` rows, counts rising
    /// so a delta-based histogram scalar has something to report. Written
    /// through the STREAMING writer with a small row cap so the table is
    /// multi-segment and `RezReader` opens it with the segment-aware source —
    /// the reader that implements the `__run__` conflict policy.
    fn segmented_histogram_rez(n: u64, max_rows: usize, out: &std::path::Path) {
        use crate::recorder::rez_stream::{
            ManifestSeed, RezWriterHandle, SealPolicy, StreamRecorder,
        };
        use metriken_exposition::Histogram as ExpHistogram;

        let handle = RezWriterHandle::create(
            out,
            ManifestSeed {
                dir: "rezolus".to_string(),
                labels: rez_labels(),
                metadata: rez_labels(),
                clock_anchor_wall_ns: 1_700_000_000_000_000_000,
            },
        )
        .unwrap();
        let mut rec = StreamRecorder::with_policy(
            handle,
            SealPolicy {
                max_bytes: usize::MAX,
                max_rows,
                max_age: std::time::Duration::from_secs(3600),
            },
        );
        let mut last_ts = 0;
        for i in 0..n {
            let ts = 1_000_000_000 * (i + 1);
            let mut h = ::histogram::Histogram::new(7, 64).unwrap();
            for _ in 0..=i {
                h.increment(1_000).unwrap();
            }
            let hist = ExpHistogram::new(
                "latency".to_string(),
                h,
                [
                    ("metric".to_string(), "latency".to_string()),
                    ("sampler".to_string(), "scheduler_runqueue".to_string()),
                    // The query engine keys histogram decoding off these, as
                    // the agent's own snapshots carry them.
                    ("grouping_power".to_string(), "7".to_string()),
                    ("max_value_power".to_string(), "64".to_string()),
                ]
                .into_iter()
                .collect(),
            )
            .with_window(Some(Window::new(ts - 50_000_000, ts)));
            let snapshot = Snapshot::V2(SnapshotV2 {
                systemtime: SystemTime::UNIX_EPOCH + std::time::Duration::from_nanos(ts),
                duration: std::time::Duration::ZERO,
                metadata: HashMap::new(),
                counters: Vec::new(),
                gauges: Vec::new(),
                histograms: vec![hist],
            });
            rec.ingest(&snapshot, ts, 0);
            rec.maybe_seal().unwrap();
            last_ts = ts;
        }
        rec.finalize((last_ts, 0)).unwrap();
    }

    /// End-to-end check of the segmented conflict policy's escape hatch through
    /// the *front door*. `RezReader` routes every query through `columns()`
    /// first, and `columns()` requires every filter key to be present on the
    /// label set — so a `__run__`-qualified selector that `column_map` does not
    /// tag is rejected as "references no metric present in this .rez" long
    /// before `query_range` sees it. A dashboard pinning `__run__="0"` for
    /// stability across an A/B pair must keep working on the side that never
    /// drifted.
    ///
    /// Segmented tables only: `__run__` is a segment-splice concept, so a
    /// single-segment table (the atomic writer, or a slow sampler that never
    /// rolled) is opened with the plain `ParquetReader` and knows nothing
    /// about it.
    #[test]
    fn run_qualified_histogram_query_routes_through_rez_reader() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hist.rez");
        segmented_histogram_rez(6, 2, &path);
        assert!(
            segment_counts(&path)["scheduler_runqueue"] > 1,
            "the fixture must be segmented, or this proves nothing"
        );

        let pool = BufferPool::new(64 * 1024 * 1024);
        let reader = RezReader::open_with_pool(&path, pool).unwrap();
        let (start, end) = reader.time_range().unwrap();

        let plain = reader.columns("histogram_mean(latency)").unwrap();
        assert!(!plain.is_empty(), "cols: {plain:?}");
        let pinned = reader
            .columns("histogram_mean(latency{__run__=\"0\"})")
            .unwrap();
        assert_eq!(pinned, plain, "a run-qualified query must route the same");

        let q = reader.query_range(
            "histogram_mean(latency{__run__=\"0\"})",
            start,
            end + 1.0,
            1.0,
        );
        assert!(q.is_ok(), "pinned histogram query should resolve: {q:?}");
    }

    #[test]
    fn open_recordings_returns_one_reader_per_recording() {
        // Build a 2-recording .rez by reading a 1-recording fixture and writing
        // it twice under distinct dirs/arms via write_archive_bytes.
        let (_d, p) = two_sampler_rez();
        let (m, rb) = crate::recorder::rez::read_archive_bytes(&p).unwrap();
        let rec0 = m.recordings.into_iter().next().unwrap();
        let bytes0: Vec<Vec<Vec<u8>>> = rb
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

    /// The whole point of the splice design: a segmented table must answer
    /// every query exactly as the single-segment table holding the same rows
    /// does — including a `rate()` window that straddles a segment boundary,
    /// where a naive per-segment reader would lose the sample it needs.
    #[test]
    fn segmented_rez_queries_match_single_segment_equivalent() {
        let rows = fixture_rows(6);
        let dir = tempfile::tempdir().unwrap();
        let single = dir.path().join("single.rez");
        let segmented = dir.path().join("segmented.rez");
        write_atomic_rez(&rows, &single);
        write_streamed_rez(&rows, 2, &segmented);

        // The fixtures must actually differ in segmentation, or this proves
        // nothing.
        assert_eq!(
            segment_counts(&single),
            [
                ("blockio_requests".to_string(), 1),
                ("cpu_usage".to_string(), 1)
            ]
            .into_iter()
            .collect::<BTreeMap<_, _>>()
        );
        assert_eq!(
            segment_counts(&segmented),
            [
                ("blockio_requests".to_string(), 3),
                ("cpu_usage".to_string(), 3)
            ]
            .into_iter()
            .collect::<BTreeMap<_, _>>(),
            "6 rows at max_rows=2 → 3 segments per table"
        );

        let a = RezReader::open_with_pool(&single, BufferPool::new(64 * 1024 * 1024)).unwrap();
        let b = RezReader::open_with_pool(&segmented, BufferPool::new(64 * 1024 * 1024)).unwrap();

        assert_eq!(a.counter_names(), b.counter_names());
        assert_eq!(a.gauge_names(), b.gauge_names());
        assert_eq!(a.time_range_ns(), b.time_range_ns());

        let (start, end) = a.time_range().unwrap();
        assert_eq!(b.time_range(), Some((start, end)));

        let same = |expr: &str| {
            let ra = a.query_range(expr, start, end, 1.0).unwrap();
            let rb = b.query_range(expr, start, end, 1.0).unwrap();
            assert_eq!(
                serde_json::to_value(&ra).unwrap(),
                serde_json::to_value(&rb).unwrap(),
                "segmented answer differs for {expr}"
            );
            ra
        };

        // A plain gauge over the full span.
        same("frequency");
        // A rate window narrow enough that most evaluation points draw their
        // two samples from *different* segments (segments hold 2 rows each).
        let rate = same("rate(cpu_cycles[2s])");
        // Non-degenerate: the query must actually have produced values, or
        // "identical" would be vacuous.
        let json = serde_json::to_value(&rate).unwrap();
        let values = json["result"][0]["values"].as_array().unwrap();
        assert!(
            values.iter().any(|v| v[1] != "0"),
            "the boundary-spanning rate must produce non-zero values: {json}"
        );
        // Wider windows too, so the splice is exercised across >2 segments.
        same("rate(cpu_cycles[4s])");
        same("irate(cpu_cycles[2s])");
        same("rate(reads[3s])");
    }

    /// The common real shape: slow samplers seal once, fast ones many times.
    /// Both kinds of table must be openable and queryable from one archive.
    #[test]
    fn mixed_single_and_multi_segment_tables_are_queryable() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("mixed.rez");
        // `blockio_requests` reports once per 3 polls, so it accumulates one
        // row for every 3 `cpu_usage` rows and never reaches the row cap.
        let rows: Vec<(Snapshot, u64)> = (0..6u64)
            .map(|i| {
                let ts = 1_000_000_000 * (i + 1);
                let w = Some(Window::new(ts - 50_000_000, ts));
                // A stale window means the sampler did not advance → deduped.
                let slow_end = 1_000_000_000 * (i / 3 + 1);
                let slow_w = Some(Window::new(slow_end - 50_000_000, slow_end));
                (
                    snap(
                        ts,
                        vec![
                            counter("cpu_cycles", "cpu_usage", i * 1_000, w),
                            counter("reads", "blockio_requests", i / 3, slow_w),
                        ],
                        vec![gauge("frequency", "cpu_usage", 2_000 + i as i64, w)],
                    ),
                    ts,
                )
            })
            .collect();
        write_streamed_rez(&rows, 2, &out);

        let counts = segment_counts(&out);
        assert_eq!(counts.get("cpu_usage"), Some(&3), "{counts:?}");
        assert_eq!(
            counts.get("blockio_requests"),
            Some(&1),
            "the slow sampler seals exactly once, at finalize: {counts:?}"
        );

        let reader = RezReader::open_with_pool(&out, BufferPool::new(64 * 1024 * 1024)).unwrap();
        assert_eq!(
            reader.counter_names(),
            vec!["cpu_cycles".to_string(), "reads".to_string()],
            "both tables contribute to the union"
        );
        let (start, end) = reader.time_range().unwrap();
        // Routing still picks exactly one owner per query, across both kinds.
        // The 3-segment table…
        assert!(reader
            .query_range("rate(cpu_cycles[2s])", start, end, 1.0)
            .is_ok());
        // …and the 1-segment one, which never went through the splice.
        assert!(reader
            .query_range("rate(reads[4s])", start, end, 1.0)
            .is_ok());
        // And a query spanning both still errors naming both samplers.
        let err = reader
            .query_range("cpu_cycles + reads", start, end, 1.0)
            .unwrap_err();
        let msg = format!("{err:?}");
        assert!(
            msg.contains("cpu_usage") && msg.contains("blockio_requests"),
            "got: {msg}"
        );
    }

    // ---------------------------------------------------------------------
    // The v3 (SQLite) container. Same reader, same sub-sources — what is new
    // is that the newest "segment" of a table may be materialized from the
    // live WAL instead of read from `segments`.
    // ---------------------------------------------------------------------

    mod v3 {
        use super::*;
        use crate::recorder::rez_sqlite::WalRow;
        use crate::recorder::rez_stream::SealPolicy;
        use crate::recorder::rez_v3_writer::{
            encode_wal_row, ManifestSeed, RezV3Writer, StreamRecorderV3, WalCell, WalValue,
        };
        use metriken_exposition::Histogram as ExpHistogram;

        const ANCHOR: u64 = 1_700_000_000_000_000_000;

        fn seed() -> ManifestSeed {
            ManifestSeed {
                labels: rez_labels(),
                metadata: rez_labels(),
                clock_anchor_wall_ns: ANCHOR,
            }
        }

        fn policy(max_rows: usize) -> SealPolicy {
            SealPolicy {
                max_bytes: usize::MAX,
                max_rows,
                max_age: std::time::Duration::from_secs(3600),
            }
        }

        fn recorder(path: &std::path::Path, max_rows: usize) -> StreamRecorderV3 {
            StreamRecorderV3::with_policy(
                RezV3Writer::create(path, seed()).unwrap(),
                policy(max_rows),
            )
        }

        /// Ingest `rows` through the v3 writer at `max_rows` per segment.
        /// `finalize` decides whether the recording ends cleanly (every tail
        /// sealed, WAL empty) or is dropped mid-flight (tail live in the WAL).
        fn write_v3(
            rows: &[(Snapshot, u64)],
            max_rows: usize,
            finalize: bool,
            out: &std::path::Path,
        ) {
            let mut rec = recorder(out, max_rows);
            let mut last_ts = 0;
            for (s, ts) in rows {
                rec.ingest(s, *ts, 0).unwrap();
                rec.maybe_seal().unwrap();
                last_ts = *ts;
            }
            if finalize {
                rec.finalize((last_ts, 0)).unwrap();
            } else {
                drop(rec);
            }
        }

        fn open(path: &std::path::Path) -> RezReader {
            RezReader::open_with_pool(path, BufferPool::new(64 * 1024 * 1024)).unwrap()
        }

        /// `sampler -> sealed segment count` straight from the catalog, so a
        /// fixture's segmentation can be asserted instead of assumed.
        fn sealed_counts(path: &std::path::Path) -> BTreeMap<String, usize> {
            let db = RezDb::open(path).unwrap();
            let rid = db.read_recordings().unwrap()[0].id;
            db.all_samplers(rid)
                .unwrap()
                .into_iter()
                .map(|s| {
                    let n = db.read_segments(rid, &s).unwrap().len();
                    (s, n)
                })
                .collect()
        }

        /// Live (unsealed) WAL row timestamps for `sampler`.
        fn live_ts(path: &std::path::Path, sampler: &str) -> Vec<u64> {
            let db = RezDb::open(path).unwrap();
            let rid = db.read_recordings().unwrap()[0].id;
            db.live_wal(rid, sampler)
                .unwrap()
                .iter()
                .map(|r| r.ts)
                .collect()
        }

        #[test]
        fn v3_and_v2_queries_agree_on_identical_data() {
            // The container changed; the answers must not. Same rows through
            // both writers, and every question the reader can be asked must
            // come back the same — including a rate() window narrow enough
            // that most evaluation points draw their two samples from
            // different segments.
            let rows = fixture_rows(6);
            let dir = tempfile::tempdir().unwrap();
            let v2 = dir.path().join("v2.rez");
            let v3 = dir.path().join("v3.rez");
            write_atomic_rez(&rows, &v2);
            write_v3(&rows, 2, true, &v3);

            assert_eq!(
                sealed_counts(&v3),
                [
                    ("blockio_requests".to_string(), 3),
                    ("cpu_usage".to_string(), 3)
                ]
                .into_iter()
                .collect::<BTreeMap<_, _>>(),
                "6 rows at max_rows=2 → 3 segments per table, so the splice \
                 is actually exercised"
            );

            let a = open(&v2);
            let b = open(&v3);
            assert_eq!(a.counter_names(), b.counter_names());
            assert_eq!(a.gauge_names(), b.gauge_names());
            assert_eq!(a.time_range_ns(), b.time_range_ns());

            let (start, end) = a.time_range().unwrap();
            let same = |expr: &str| {
                let ra = a.query_range(expr, start, end, 1.0).unwrap();
                let rb = b.query_range(expr, start, end, 1.0).unwrap();
                assert_eq!(
                    serde_json::to_value(&ra).unwrap(),
                    serde_json::to_value(&rb).unwrap(),
                    "v3 answer differs for {expr}"
                );
                ra
            };
            same("frequency");
            let rate = same("rate(cpu_cycles[2s])");
            let json = serde_json::to_value(&rate).unwrap();
            let values = json["result"][0]["values"].as_array().unwrap();
            assert!(
                values.iter().any(|v| v[1] != "0"),
                "the boundary-spanning rate must produce non-zero values: {json}"
            );
            same("rate(reads[3s])");
        }

        #[test]
        fn the_live_wal_tail_is_queryable_before_it_seals() {
            // Under v2 the rows in an open segment did not exist in the
            // archive at all until it sealed. Here they are committed per
            // tick, and the reader must present them — so the newest data,
            // which is the data an incident is about, is readable from a file
            // that is still being written.
            let rows = fixture_rows(5);
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("tail.rez");
            // max_rows=2 → ticks 1..4 seal into two segments; tick 5 is a
            // live, unsealed tail.
            write_v3(&rows, 2, false, &path);
            assert_eq!(
                sealed_counts(&path)["cpu_usage"],
                2,
                "the fixture must have sealed segments AND an unsealed tail"
            );
            assert_eq!(
                live_ts(&path, "cpu_usage"),
                vec![5_000_000_000],
                "tick 5 is unsealed"
            );

            let reader = open(&path);
            let (_, end) = reader.time_range_ns().unwrap();
            assert_eq!(
                end, 5_000_000_000,
                "the reader's timeline must reach the unsealed tick"
            );

            // And the tail's VALUE is there, not just its timestamp: tick 5 is
            // the 5th row, whose gauge is 2_000 + 4.
            let r = reader.query("frequency", Some(5.0)).unwrap();
            let json = serde_json::to_value(&r).unwrap();
            assert_eq!(
                json["result"][0]["value"][1].as_f64(),
                Some(2004.0),
                "the unsealed tick's own value must be queryable: {json}"
            );
        }

        #[test]
        fn a_quiet_sampler_with_no_segments_at_all_is_readable() {
            // The 16-of-26 fleet case. A sampler still inside its first seal
            // period has no row in `segments`, so a reader that enumerated
            // tables from `samplers()` would not know it exists — and would
            // silently drop exactly the tables the container swap was for.
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("quiet.rez");
            // `max_rows = 4` (the stagger reduces the target by
            // `(4 / 128) * bucket = 0`, so it is exactly 4). `cpu_usage`
            // advances every tick and seals twice over 8 ticks; `drivehealth`
            // advances every third tick, so it accumulates 3 rows and never
            // reaches the threshold.
            let mut rec = recorder(&path, 4);
            for i in 0..8u64 {
                let ts = 1_000_000_000 * (i + 1);
                let w = Some(Window::new(ts - 50_000_000, ts));
                let slow_end = 1_000_000_000 * (i / 3 + 1);
                let slow = Some(Window::new(slow_end - 50_000_000, slow_end));
                let s = snap(
                    ts,
                    vec![
                        counter("cpu_cycles", "cpu_usage", i * 1_000, w),
                        counter("temperature", "drivehealth", 40 + i, slow),
                    ],
                    Vec::new(),
                );
                rec.ingest(&s, ts, 0).unwrap();
                rec.maybe_seal().unwrap();
            }
            drop(rec);

            let counts = sealed_counts(&path);
            assert_eq!(counts.get("cpu_usage"), Some(&2), "{counts:?}");
            assert_eq!(
                counts.get("drivehealth"),
                Some(&0),
                "the quiet sampler must have NO sealed segment, or this test \
                 proves nothing: {counts:?}"
            );

            let reader = open(&path);
            assert!(
                reader.counter_names().contains(&"temperature".to_string()),
                "a never-sealed table must still be named: {:?}",
                reader.counter_names()
            );
            let r = reader
                .query_range("rate(temperature[4s])", 1.0, 8.0, 1.0)
                .expect("a never-sealed table must answer a query");
            let json = serde_json::to_value(&r).unwrap();
            let values = json["result"][0]["values"].as_array().unwrap();
            assert!(
                values.iter().any(|v| v[1] != "0"),
                "and answer it with the WAL's own values: {json}"
            );
        }

        /// One sampler's table, decoded from whichever of the two forms the
        /// file holds: its single sealed segment, or the segment materialized
        /// from its live WAL.
        ///
        /// The eager decoder is used deliberately. `metriken-query` classifies
        /// a column by its ARROW type (UInt64 / Int64 / List), so the trap's
        /// symptom — a column carrying the entry's metadata verbatim, without
        /// the `metric_type` `push_row` injects — is invisible from the query
        /// front door. It is not invisible to `read_table_parquet`, and it
        /// would not be invisible to `parquet metadata` or to anything else
        /// that reads a segment's declared metric types. "The same shape as a
        /// sealed segment" has to be asserted where shape is observable.
        fn decoded_table(path: &std::path::Path, sampler: &str) -> rez::RezTable {
            let mut segments = decoded_segments(path, sampler);
            assert_eq!(
                segments.len(),
                1,
                "this helper wants a single-segment table"
            );
            segments.pop().unwrap()
        }

        /// Every segment the READER would open for `sampler`, decoded — the
        /// sealed ones plus the materialized tail, assembled by the production
        /// helper rather than re-derived here.
        fn decoded_segments(path: &std::path::Path, sampler: &str) -> Vec<rez::RezTable> {
            let db = RezDb::open(path).unwrap();
            let rid = db.read_recordings().unwrap()[0].id;
            table_segments(&db, rid, sampler)
                .unwrap()
                .into_iter()
                .map(|b| rez::read_table_parquet(sampler.to_string(), b).unwrap())
                .collect()
        }

        /// A table's full comparable shape: per column, its key, its complete
        /// metadata map, its typed values and its windows — plus the row
        /// timestamps and wall-clock sidecar.
        type TableShape = (
            Vec<u64>,
            Vec<i64>,
            Vec<(
                String,
                Vec<(String, String)>,
                rez::RezValues,
                Vec<Option<Window>>,
            )>,
        );
        fn shape(t: &rez::RezTable) -> TableShape {
            let columns = t
                .columns
                .iter()
                .map(|c| {
                    let mut meta: Vec<(String, String)> = c
                        .metadata
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect();
                    meta.sort();
                    (c.name.clone(), meta, c.values.clone(), c.windows.clone())
                })
                .collect();
            (t.timestamps.clone(), t.wall_offsets.clone(), columns)
        }

        #[test]
        fn a_materialized_tail_has_the_same_shape_as_a_sealed_segment() {
            // THE trap. `WalCell::metadata` is the snapshot ENTRY's metadata,
            // which does not carry `metric_type` — `TableBuilder::push_row`
            // injects it. A tail built by copying that metadata into a
            // `RezColumn` produces a segment a natively sealed one does not
            // match, and `read_table_parquet` then reads every gauge back as a
            // counter.
            //
            // Same rows, two recordings: one finalized (a pure sealed
            // segment), one dropped before its first seal (a pure materialized
            // tail). The two segments must be indistinguishable.
            let rows = fixture_rows(4);
            let dir = tempfile::tempdir().unwrap();
            let sealed = dir.path().join("sealed.rez");
            let tail = dir.path().join("tail.rez");
            write_v3(&rows, 4, true, &sealed);
            write_v3(&rows, usize::MAX, false, &tail);

            assert_eq!(
                sealed_counts(&sealed)["cpu_usage"],
                1,
                "the sealed fixture must have a real segment"
            );
            assert_eq!(
                sealed_counts(&tail)["cpu_usage"],
                0,
                "the tail fixture must have NO segment, only WAL"
            );

            // The segments themselves, column for column: names, the complete
            // metadata map (so `metric_type` and every label are compared),
            // the typed values, the windows, the timestamps and the
            // `:wall_offset` sidecar.
            let want = decoded_table(&sealed, "cpu_usage");
            let got = decoded_table(&tail, "cpu_usage");
            assert_eq!(shape(&want), shape(&got));

            // Non-vacuous: the fixture really does hold both a counter and a
            // gauge, and the tail really does declare them as such.
            let declared: BTreeMap<&str, &str> = got
                .columns
                .iter()
                .map(|c| (c.name.as_str(), c.metadata["metric_type"].as_str()))
                .collect();
            assert_eq!(
                declared,
                [("cpu_cycles", "counter"), ("frequency", "gauge")]
                    .into_iter()
                    .collect::<BTreeMap<_, _>>(),
                "a gauge must not come back a counter"
            );
            assert!(
                matches!(got.columns[1].values, rez::RezValues::Gauge(_)),
                "and its values must be the signed column: {:?}",
                got.columns[1].values
            );
            assert_eq!(
                got.columns[1].metadata.get("sampler").map(String::as_str),
                Some("cpu_usage"),
                "labels survive the round trip through the WAL"
            );

            // And through the front door the two files answer identically.
            let a = open(&sealed);
            let b = open(&tail);
            assert_eq!(a.gauge_names(), vec!["frequency".to_string()]);
            assert_eq!(b.gauge_names(), a.gauge_names());
            assert_eq!(a.counter_names(), b.counter_names());
            assert_eq!(a.gauge_labels("frequency"), b.gauge_labels("frequency"));
            let (start, end) = a.time_range().unwrap();
            assert_eq!(b.time_range(), Some((start, end)));
            for expr in ["frequency", "rate(cpu_cycles[2s])"] {
                assert_eq!(
                    serde_json::to_value(a.query_range(expr, start, end, 1.0).unwrap()).unwrap(),
                    serde_json::to_value(b.query_range(expr, start, end, 1.0).unwrap()).unwrap(),
                    "materialized tail differs from a sealed segment for {expr}"
                );
            }
        }

        /// The same fixture as `fixture_rows`, plus a histogram in a third
        /// sampler — so the tail's histogram reconstruction
        /// (`from_buckets(gp, mvp, buckets)`) is exercised too.
        fn histogram_rows(n: u64) -> Vec<(Snapshot, u64)> {
            (0..n)
                .map(|i| {
                    let ts = 1_000_000_000 * (i + 1);
                    let w = Some(Window::new(ts - 50_000_000, ts));
                    let mut h = ::histogram::Histogram::new(7, 64).unwrap();
                    for _ in 0..=i {
                        h.increment(1_000).unwrap();
                    }
                    let hist = ExpHistogram::new(
                        "latency".to_string(),
                        h,
                        [
                            ("metric".to_string(), "latency".to_string()),
                            ("sampler".to_string(), "scheduler_runqueue".to_string()),
                            ("grouping_power".to_string(), "7".to_string()),
                            ("max_value_power".to_string(), "64".to_string()),
                        ]
                        .into_iter()
                        .collect(),
                    )
                    .with_window(w);
                    let s = Snapshot::V2(SnapshotV2 {
                        systemtime: SystemTime::UNIX_EPOCH + std::time::Duration::from_nanos(ts),
                        duration: std::time::Duration::ZERO,
                        metadata: HashMap::new(),
                        counters: Vec::new(),
                        gauges: Vec::new(),
                        histograms: vec![hist],
                    });
                    (s, ts)
                })
                .collect()
        }

        #[test]
        fn a_materialized_tail_reconstructs_histograms() {
            // A histogram cell carries its H2 config with its buckets, so the
            // tail rebuilds one without consulting metadata. If it did not,
            // the column would come back the wrong shape and the reader would
            // not name it a histogram at all.
            let dir = tempfile::tempdir().unwrap();
            let sealed = dir.path().join("hsealed.rez");
            let tail = dir.path().join("htail.rez");
            let rows = histogram_rows(4);
            write_v3(&rows, 4, true, &sealed);
            write_v3(&rows, usize::MAX, false, &tail);
            assert_eq!(sealed_counts(&tail)["scheduler_runqueue"], 0);

            // Bucket for bucket against the natively sealed segment: the
            // reconstruction has to reproduce the H2 config AND the counts.
            let want = decoded_table(&sealed, "scheduler_runqueue");
            let got = decoded_table(&tail, "scheduler_runqueue");
            assert_eq!(shape(&want), shape(&got));
            match &got.columns[0].values {
                rez::RezValues::Histogram(v) => {
                    let last = v.last().unwrap().as_ref().expect("a histogram cell");
                    assert_eq!(last.config().grouping_power(), 7);
                    assert_eq!(last.config().max_value_power(), 64);
                    assert_eq!(
                        last.as_slice().iter().sum::<u64>(),
                        4,
                        "the 4th tick's histogram holds 4 increments"
                    );
                }
                other => panic!("the tail must rebuild a histogram column: {other:?}"),
            }

            let a = open(&sealed);
            let b = open(&tail);
            assert_eq!(a.histogram_names(), vec!["latency".to_string()]);
            assert_eq!(b.histogram_names(), a.histogram_names());
            let (start, end) = a.time_range().unwrap();
            assert_eq!(
                serde_json::to_value(
                    a.query_range("histogram_mean(latency)", start, end, 1.0)
                        .unwrap()
                )
                .unwrap(),
                serde_json::to_value(
                    b.query_range("histogram_mean(latency)", start, end, 1.0)
                        .unwrap()
                )
                .unwrap(),
            );
        }

        #[test]
        fn a_recovered_recording_reads_with_its_tail_spliced_after_its_segments() {
            // The kill path. Segments sealed, a tail left live, no finalize:
            // one continuous timeline, tail last, and no duplicated row at the
            // seam — `live_wal`'s watermark excludes the rows the segments
            // already cover, and the reader must rely on exactly that.
            let rows = fixture_rows(7);
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("killed.rez");
            write_v3(&rows, 2, false, &path);

            let want: Vec<u64> = (1..=7).map(|i| 1_000_000_000 * i).collect();
            assert_eq!(sealed_counts(&path)["cpu_usage"], 3, "6 rows sealed");
            assert_eq!(live_ts(&path, "cpu_usage"), vec![7_000_000_000]);

            // The crash window, reproduced. The prune runs OUTSIDE the seal
            // transaction (inside it measured p90 78 ms), so a recording
            // killed between the commit and the delete keeps WAL rows a
            // sealed segment already covers. The in-process writer always
            // gets to its prune, so that straddle has to be put back by hand
            // — and without it this test cannot tell `live_wal` from
            // `read_wal` at all.
            {
                let mut db = RezDb::open(&path).unwrap();
                let rid = db.read_recordings().unwrap()[0].id;
                let straddling: Vec<WalRow> = (1..=6u64)
                    .map(|i| {
                        let ts = 1_000_000_000 * i;
                        WalRow {
                            sampler: "cpu_usage".to_string(),
                            ts,
                            wall_offset: 0,
                            row: encode_wal_row(&[WalCell {
                                name: "cpu_cycles".to_string(),
                                metadata: Some(
                                    [
                                        ("metric".to_string(), "cpu_cycles".to_string()),
                                        ("sampler".to_string(), "cpu_usage".to_string()),
                                    ]
                                    .into_iter()
                                    .collect(),
                                ),
                                value: WalValue::Counter(i * 1_000),
                                window: Some((ts - 50_000_000, ts)),
                            }])
                            .unwrap(),
                        }
                    })
                    .collect();
                db.insert_wal_rows(rid, &straddling).unwrap();
                assert_eq!(
                    db.read_wal(rid, "cpu_usage").unwrap().len(),
                    7,
                    "the raw WAL now straddles the sealed watermark"
                );
                assert_eq!(
                    db.live_wal(rid, "cpu_usage").unwrap().len(),
                    1,
                    "…but only one row is past it"
                );
            }

            // The seam, examined directly: the sealed segments' rows followed
            // by the materialized tail's rows must be exactly the ingested
            // timestamps, once each, in order. A reader that replayed the raw
            // WAL instead of the live one would repeat the sealed rows here.
            let segments = decoded_segments(&path, "cpu_usage");
            assert_eq!(
                segments.len(),
                4,
                "3 sealed segments plus the materialized tail"
            );
            assert_eq!(
                segments.last().unwrap().timestamps,
                vec![7_000_000_000],
                "the tail is LAST, and holds only the unsealed tick"
            );
            let seen: Vec<u64> = segments.iter().flat_map(|t| t.timestamps.clone()).collect();
            assert_eq!(
                seen, want,
                "one continuous timeline, tail last, no duplicate at the seam"
            );

            // And through the front door.
            let reader = open(&path);
            assert_eq!(
                reader.time_range_ns(),
                Some((1_000_000_000, 7_000_000_000)),
                "the timeline spans the sealed segments AND the tail"
            );
            let r = reader.query_range("rate(cpu_cycles[2s])", 1.0, 7.0, 1.0);
            assert!(r.is_ok(), "the spliced timeline must answer: {r:?}");
            assert!(
                !reader.metadata_get("source").unwrap_or_default().is_empty(),
                "the recording's manifest metadata survives"
            );
        }

        // -------------------------------------------------------------
        // Native V3 acquisition-group ingest: end-to-end proof that Part A's
        // table-level window columns and this writer agree — a group table
        // sealed by `StreamRecorderV3` must answer `rate()` with real
        // uncertainty bands, the same way a V2 per-metric-sidecar table does.
        // -------------------------------------------------------------

        use metriken_exposition::{GroupSchema, GroupSnapshot, MetricDesc, SnapshotV3};

        fn group_schema(members: &[&str], sampler: &str) -> GroupSchema {
            GroupSchema {
                counters: members
                    .iter()
                    .map(|m| MetricDesc {
                        name: m.to_string(),
                        metadata: [
                            ("metric".to_string(), m.to_string()),
                            ("sampler".to_string(), sampler.to_string()),
                        ]
                        .into_iter()
                        .collect(),
                    })
                    .collect(),
                gauges: Vec::new(),
                histograms: Vec::new(),
            }
        }

        /// `n` ticks of one acquisition group (`cpu_usage/percpu`, one member
        /// `cpu_cycles`), one second apart, each with a 50 ms window ending at
        /// the tick — the same shape `fixture_rows` uses for its V2 counter,
        /// so a `rate()` query narrow enough to span segment boundaries is
        /// exercised the same way. The schema is sent on every tick: this
        /// fixture is about proving the write/read path agrees, not about
        /// exercising the schema-hash cache (see `rez_v3_writer`'s own tests
        /// for that).
        fn group_fixture_rows(n: u64) -> Vec<(Snapshot, u64)> {
            let schema = std::sync::Arc::new(group_schema(&["cpu_cycles"], "cpu_usage"));
            (0..n)
                .map(|i| {
                    let ts = 1_000_000_000 * (i + 1);
                    let w = Some(Window::new(ts - 50_000_000, ts));
                    let group = GroupSnapshot {
                        name: "cpu_usage/percpu".to_string(),
                        schema_hash: schema.hash(),
                        schema: Some(std::sync::Arc::clone(&schema)),
                        window: w,
                        counters: vec![Some(i)],
                        gauges: Vec::new(),
                        histograms: Vec::new(),
                    };
                    let s = Snapshot::V3(SnapshotV3 {
                        systemtime: SystemTime::UNIX_EPOCH + std::time::Duration::from_nanos(ts),
                        duration: std::time::Duration::ZERO,
                        metadata: HashMap::new(),
                        groups: vec![group],
                    });
                    (s, ts)
                })
                .collect()
        }

        #[test]
        fn a_native_v3_group_table_answers_rate_with_uncertainty_bands() {
            let rows = group_fixture_rows(6);
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("groups.rez");
            // max_rows=2 forces multiple segments, so the splice at segment
            // boundaries is exercised the same way `v3_and_v2_queries_agree`
            // exercises it for the sampler-keyed path.
            write_v3(&rows, 2, true, &path);

            assert_eq!(
                sealed_counts(&path)["cpu_usage/percpu"],
                3,
                "6 rows at max_rows=2 -> 3 segments, so the reader splices \
                 across a table-level-window segment boundary"
            );

            let reader = open(&path);
            assert_eq!(reader.counter_names(), vec!["cpu_cycles".to_string()]);

            let json = serde_json::to_value(
                reader
                    .query_range("rate(cpu_cycles[2s])", 1.0, 6.0, 1.0)
                    .unwrap(),
            )
            .unwrap();
            let values = json["result"][0]["values"].as_array().unwrap();
            assert!(
                values.iter().any(|v| v[1] != "0"),
                "a boundary-spanning rate over the native V3 group table must \
                 produce non-zero values: {json}"
            );
            // `series.intervals` (`metriken_query::QueryResult::Matrix`) is
            // the acquisition-window uncertainty band `rezolus mcp query`
            // reports as `[lo, hi]` for rate()/irate(). Its presence here —
            // resolved with no special-case "this is a group table" logic on
            // the reader's part — is the proof that Part A's table-level
            // `:window_begin`/`:window_width` columns and this writer's
            // group-table layout actually agree end to end: the bare pair
            // this writer emitted (not a per-metric sidecar) is what fed it.
            let intervals = json["result"][0]["intervals"]
                .as_array()
                .expect("a rate() query over a windowed group table must carry bands");
            assert!(
                intervals.iter().any(|iv| iv.is_array()),
                "at least one point must carry a resolved [lo, hi] band: {json}"
            );
        }
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
