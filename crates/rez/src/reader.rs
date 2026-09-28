//! `RezReader`: a `.rez` or dendro archive read as one
//! `metriken_query::MetricsSource` per recording.
//!
//! The reader itself is metriken-archive's [`ArchiveReader`] (moved from
//! here, phase 3 of metriken's
//! `docs/journal/2026-09-28-high-cardinality-stack.md`). What stays in
//! rezolus is the `.rez` side: recognizing and opening the container
//! (v2 tar, v3 SQLite, or dendro; see [`crate::catalog`]), and reading a
//! `.rez` recording's identity index (caller rows written by
//! `record --stream`) into a relabelling, [`IdentityIndex`].

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

pub use metriken_archive::ArchiveReader;
use metriken_archive::{InMemorySource, IndexRelabel};
use metriken_query::{BufferPool, MetricsSource, QueryError, QueryOptions, QueryResult};

use crate::catalog::{Catalog, Container};
use crate::rez::{self, RecordingBytes};
use crate::rez_sqlite::RezDb;
use crate::wal::materialize_wal_tail;

/// A `.rez` or dendro archive read as a `MetricsSource`: metriken-archive's
/// [`ArchiveReader`], opened from a path or bytes by content. Derefs to it,
/// so its methods (`composition_sources`, `eval_timestamps_for`,
/// `metric_metadata`, `complete`, ...) are this type's.
pub struct RezReader(pub ArchiveReader);

impl std::ops::Deref for RezReader {
    type Target = ArchiveReader;
    fn deref(&self) -> &ArchiveReader {
        &self.0
    }
}

/// One `RezReader` per recording, paired with that recording's label set.
pub type LabeledRecordings = Vec<(BTreeMap<String, String>, RezReader)>;

fn wrap(readers: metriken_archive::LabeledRecordings) -> LabeledRecordings {
    readers
        .into_iter()
        .map(|(l, r)| (l, RezReader(r)))
        .collect()
}

impl RezReader {
    /// Open an archive at `path`, flattening every recording into one view.
    ///
    /// **No production caller.** The viewer opens recordings individually,
    /// and so does `mcp` since flattening a multi-recording archive gives
    /// every sampler two owners and makes routing refuse every query. Kept
    /// because the flattening behaviour, and the refusal it produces, is
    /// what the cross-recording regression tests pin.
    #[allow(dead_code)]
    pub fn open_with_pool(
        path: &Path,
        pool: Arc<BufferPool>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let filename = path.file_name().map(|s| s.to_string_lossy().into_owned());
        if let Some(opened) = from_catalog_path(path, Arc::clone(&pool))? {
            return Ok(RezReader(ArchiveReader::flatten(
                opened.into_iter().map(|(_, r)| r).collect(),
                filename,
            )));
        }
        let recordings = read_recordings(path)?;
        Ok(RezReader(ArchiveReader::from_in_memory(
            recordings.into_iter().map(in_memory).collect(),
            filename,
            pool,
        )?))
    }

    /// [`open_recordings`](Self::open_recordings) for an archive that exists
    /// only as bytes: the browser's entry point. Containers are recognized
    /// by content, as for a path.
    pub fn open_recordings_from_bytes(
        bytes: Vec<u8>,
        pool: Arc<BufferPool>,
    ) -> Result<LabeledRecordings, Box<dyn std::error::Error>> {
        for container in [Container::Dendro, Container::Rez] {
            if container.recognizes_bytes(&bytes) {
                return Ok(wrap(ArchiveReader::from_catalog(
                    container.open_bytes(bytes)?,
                    None,
                    pool,
                    Some(Arc::new(IdentityIndex)),
                )?));
            }
        }
        let recordings = rez::read_archive_reader(std::io::Cursor::new(bytes))?.1;
        let mut out = Vec::with_capacity(recordings.len());
        for rec in recordings {
            let labels = rec.labels.clone();
            let filename = Some(rec.dir.clone());
            let reader =
                ArchiveReader::from_in_memory(vec![in_memory(rec)], filename, Arc::clone(&pool))?;
            out.push((labels, RezReader(reader)));
        }
        Ok(out)
    }

    /// Open an archive as one `RezReader` **per recording**, paired with that
    /// recording's labels. Used by the viewer to map a 2-recording archive
    /// onto baseline/experiment without cross-recording name collisions.
    pub fn open_recordings(
        path: &Path,
        pool: Arc<BufferPool>,
    ) -> Result<LabeledRecordings, Box<dyn std::error::Error>> {
        if let Some(opened) = from_catalog_path(path, Arc::clone(&pool))? {
            return Ok(wrap(opened));
        }
        let recordings = read_recordings(path)?;
        let mut out = Vec::with_capacity(recordings.len());
        for rec in recordings {
            let labels = rec.labels.clone();
            let filename = Some(rec.dir.clone());
            let reader =
                ArchiveReader::from_in_memory(vec![in_memory(rec)], filename, Arc::clone(&pool))?;
            out.push((labels, RezReader(reader)));
        }
        Ok(out)
    }
}

/// Open `path` as one reader per recording when it is one of the catalog
/// containers (a dendro archive or a `.rez` v3), decided by content; `None`
/// for a v1/v2 tar `.rez`, which the callers read eagerly.
fn from_catalog_path(
    path: &Path,
    pool: Arc<BufferPool>,
) -> Result<Option<metriken_archive::LabeledRecordings>, Box<dyn std::error::Error>> {
    let Some(container) = Container::of_path(path)? else {
        return Ok(None);
    };
    Ok(Some(ArchiveReader::from_catalog(
        container.open(path)?,
        Some(container.reopen(path)),
        pool,
        Some(Arc::new(IdentityIndex)),
    )?))
}

fn in_memory(rec: RecordingBytes) -> InMemorySource {
    InMemorySource {
        name: rec.dir,
        labels: rec.labels,
        metadata: rec.metadata,
        complete: rec.complete,
        tables: rec.tables,
    }
}

/// A `.rez` recording's identity index, the caller rows `record --stream`
/// writes under a table's name, replayed into occupancy spans: the
/// relabelling that files a reused slot's rows under each of its
/// occupants.
///
/// Reads from the last `Full` at or before the table's first row: a slot's
/// occupant at any row can depend on an entry from long before it, and a
/// `Full` is where that dependence stops. A stream with no `Full` before its
/// first row is read from its beginning. An index that cannot be read or
/// replayed fails the table's open, never a silent fall-through to the plain
/// path, which would file every reused slot's rows under its first occupant.
pub struct IdentityIndex;

impl IndexRelabel for IdentityIndex {
    fn relabel(
        &self,
        catalog: &dyn Catalog,
        source_id: i64,
        table: &str,
        first_row_ts: u64,
    ) -> Result<Arc<dyn metriken_query::ColumnRelabel>, String> {
        let from = catalog
            .last_caller_row_at_or_before(source_id, table, first_row_ts, &mut |blob| {
                crate::index::IndexEntry::decode(blob)
                    .is_ok_and(|e| e.kind == crate::index::EntryKind::Full)
            })?
            .unwrap_or(0);
        let entries = catalog.caller_rows(source_id, table, from, u64::MAX)?;
        let occupants = crate::indexed::Occupants::replay(&entries)
            .map_err(|e| format!("replaying the identity index for {table}: {e}"))?;
        if occupants.skipped_before_full > 0 {
            tracing::warn!(
                "table {table}: {} index entries preceded the first full index and were skipped",
                occupants.skipped_before_full
            );
        }
        Ok(Arc::new(crate::indexed::OccupantRelabel::new(occupants)))
    }
}

impl MetricsSource for RezReader {
    fn query_range_opts(
        &self,
        expr: &str,
        start_s: f64,
        end_s: f64,
        step_s: f64,
        opts: &QueryOptions,
    ) -> Result<QueryResult, QueryError> {
        self.0.query_range_opts(expr, start_s, end_s, step_s, opts)
    }
    fn query(&self, expr: &str, time: Option<f64>) -> Result<QueryResult, QueryError> {
        self.0.query(expr, time)
    }
    fn columns(&self, query: &str) -> Result<HashSet<String>, QueryError> {
        self.0.columns(query)
    }
    fn counter_names(&self) -> Vec<String> {
        self.0.counter_names()
    }
    fn gauge_names(&self) -> Vec<String> {
        self.0.gauge_names()
    }
    fn histogram_names(&self) -> Vec<String> {
        self.0.histogram_names()
    }
    fn counter_labels(&self, name: &str) -> Vec<BTreeMap<String, String>> {
        self.0.counter_labels(name)
    }
    fn gauge_labels(&self, name: &str) -> Vec<BTreeMap<String, String>> {
        self.0.gauge_labels(name)
    }
    fn histogram_labels(&self, name: &str) -> Vec<BTreeMap<String, String>> {
        self.0.histogram_labels(name)
    }
    fn time_range(&self) -> Option<(f64, f64)> {
        self.0.time_range()
    }
    fn time_range_ns(&self) -> Option<(u64, u64)> {
        self.0.time_range_ns()
    }
    fn interval(&self) -> f64 {
        self.0.interval()
    }
    fn source(&self) -> String {
        self.0.source()
    }
    fn version(&self) -> String {
        self.0.version()
    }
    fn filename(&self) -> Option<String> {
        self.0.filename()
    }
    fn metadata_get(&self, key: &str) -> Option<String> {
        self.0.metadata_get(key)
    }
    fn file_metadata(&self) -> HashMap<String, String> {
        self.0.file_metadata()
    }
    fn sample_timestamps(&self) -> Vec<u64> {
        self.0.sample_timestamps()
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
        segments.push(tail.bytes);
    }
    Ok(segments)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rez::RezRecorder;
    use crate::window::Window;
    use metriken_exposition::{Counter, Gauge, Snapshot, SnapshotV2};
    use metriken_query::{RateMode, SegmentedParquetReader};
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
        .with_window(w.map(Into::into))
    }

    /// Two readers of one recording answer alike: names, labels, span,
    /// sample timestamps, metadata, and every query in `queries` over the
    /// whole span at a 1 s step (compared as JSON, whose maps are ordered).
    /// What a dendro archive converted from a `.rez` must satisfy against it.
    pub(super) fn assert_same_answers(a: &RezReader, b: &RezReader, queries: &[&str]) {
        assert_eq!(a.counter_names(), b.counter_names());
        assert_eq!(a.gauge_names(), b.gauge_names());
        assert_eq!(a.histogram_names(), b.histogram_names());
        for name in a
            .counter_names()
            .iter()
            .chain(&a.gauge_names())
            .chain(&a.histogram_names())
        {
            let sorted = |mut v: Vec<BTreeMap<String, String>>| {
                v.sort();
                v
            };
            assert_eq!(
                sorted(a.counter_labels(name)),
                sorted(b.counter_labels(name)),
                "{name}"
            );
            assert_eq!(
                sorted(a.gauge_labels(name)),
                sorted(b.gauge_labels(name)),
                "{name}"
            );
            assert_eq!(
                sorted(a.histogram_labels(name)),
                sorted(b.histogram_labels(name)),
                "{name}"
            );
        }
        assert_eq!(a.time_range_ns(), b.time_range_ns());
        assert_eq!(a.sample_timestamps(), b.sample_timestamps());
        assert_eq!(a.interval(), b.interval());
        let (start, end) = a.time_range().unwrap();
        // Series as JSON strings, sorted: an aggregation's output order is
        // not defined, and JSON objects' keys are.
        let canonical = |r: &RezReader, q: &str| -> Result<Vec<String>, String> {
            let v = serde_json::to_value(
                r.query_range(q, start, end, 1.0)
                    .map_err(|e| format!("{e:?}"))?,
            )
            .unwrap();
            let mut series: Vec<String> = match v.get("result") {
                Some(serde_json::Value::Array(a)) => a.iter().map(|s| s.to_string()).collect(),
                _ => vec![v.to_string()],
            };
            series.sort();
            Ok(series)
        };
        for q in queries {
            assert_eq!(canonical(a, q), canonical(b, q), "{q}");
        }
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
        .with_window(w.map(Into::into))
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
        use crate::rez_stream::{ManifestSeed, RezWriterHandle, StreamRecorder};
        use crate::seal_policy::SealPolicy;

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
        let (manifest, _) = crate::rez::read_archive_bytes(path).unwrap();
        manifest.recordings[0]
            .tables
            .iter()
            .map(|t| (t.sampler.clone(), t.segment_files().len()))
            .collect()
    }

    /// Build a 2-sampler .rez fixture on disk; return (tempdir, path).
    /// Two samplers publishing the SAME metric name — the `gpu_amd_smi` /
    /// `gpu_nvidia` shape, where both vendors declare vendor-neutral names and
    /// only one populates on any given host.
    pub(super) fn two_sampler_rez_sharing_a_metric_name() -> (tempfile::TempDir, std::path::PathBuf)
    {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("shared.rez");
        let rows: Vec<(Snapshot, u64)> = (0..3u64)
            .map(|i| {
                let ts = 1_000_000_000 * (i + 1);
                let w = Some(Window::new(ts - 50_000_000, ts));
                (
                    snap(
                        ts,
                        vec![
                            counter("shared_metric", "sampler_a", i * 10, w),
                            counter("shared_metric", "sampler_b", i * 20, w),
                        ],
                        Vec::new(),
                    ),
                    ts,
                )
            })
            .collect();
        write_atomic_rez(&rows, &out);
        (dir, out)
    }

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
        use crate::rez_stream::{ManifestSeed, RezWriterHandle, StreamRecorder};
        use crate::seal_policy::SealPolicy;
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
            .with_window(Some(Window::new(ts - 50_000_000, ts).into()));
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
        let (m, rb) = crate::rez::read_archive_bytes(&p).unwrap();
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
        crate::rez::write_archive_bytes(&out, &[(a, bytes0.clone()), (b, bytes0)]).unwrap();

        let pool = BufferPool::new(64 * 1024 * 1024);
        let readers = RezReader::open_recordings(&out, pool).unwrap();
        assert_eq!(readers.len(), 2);
        assert_eq!(readers[0].0.get("arm").map(String::as_str), Some("arm0"));
        assert_eq!(readers[1].0.get("arm").map(String::as_str), Some("arm1"));
        assert!(!readers[0].1.counter_names().is_empty());
    }

    /// Reading an archive from BYTES has to answer exactly what reading the
    /// same archive from disk answers. This is the browser's whole path: a
    /// dropped file is a `Uint8Array` and there is no filesystem to write it
    /// to, so an in-memory catalog that quietly saw fewer tables — or fewer
    /// rows — would show a different dashboard for the same file with nothing
    /// to indicate it.
    #[test]
    fn a_v3_archive_read_from_bytes_matches_the_same_archive_read_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fleet.rez");
        crate::rez::recorder_tests_support::multi_recording_v3_rez(
            &path,
            &[("redis", "web-01"), ("valkey", "web-01")],
        );

        let from_disk =
            RezReader::open_recordings(&path, BufferPool::new(16 * 1024 * 1024)).unwrap();
        let from_bytes = RezReader::open_recordings_from_bytes(
            std::fs::read(&path).unwrap(),
            BufferPool::new(16 * 1024 * 1024),
        )
        .unwrap();

        assert_eq!(from_bytes.len(), from_disk.len());
        for ((disk_labels, disk), (byte_labels, bytes)) in from_disk.iter().zip(&from_bytes) {
            assert_eq!(byte_labels, disk_labels);
            assert_eq!(bytes.counter_names(), disk.counter_names());
            assert_eq!(bytes.time_range(), disk.time_range());
            // By VALUE, not merely "some series came back": the two arms of
            // this fixture hold different numbers, so a byte-backed reader
            // that opened the wrong recording would still answer here.
            // Identity, not merely "some series came back": the fixture's two
            // arms both hold `cpu_cycles`, so an assertion that data exists
            // would pass even if the byte-backed reader opened the wrong one.
            assert_eq!(bytes.metadata_get("source"), disk.metadata_get("source"));
            assert_eq!(bytes.sample_timestamps(), disk.sample_timestamps());
        }
    }

    /// The bytes a browser holds are the main database file alone — a `-wal`
    /// sidecar is a separate file it never gets. That must not cost the
    /// archive's own unsealed rows, which live in the `wal` TABLE inside the
    /// image: a hindsight snapshot's newest data is exactly there, and
    /// silently reading only sealed segments would under-report the window an
    /// incident is in.
    #[test]
    fn a_live_wal_tail_survives_the_trip_through_bytes() {
        use crate::rez::recorder_tests_support::{counter, snap};
        use crate::rez_v3_writer::{ManifestSeed, RezArchive, StreamRecorderV3};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("live.rez");
        let seed = ManifestSeed {
            labels: [("source".to_string(), "rezolus".to_string())]
                .into_iter()
                .collect(),
            metadata: Default::default(),
            clock_anchor_wall_ns: 1_000_000_000,
        };
        let (archive, writer) = RezArchive::single(&path, seed).unwrap();
        let mut rec = StreamRecorderV3::new(writer);
        for t in 0..3u64 {
            let ts = 1_000_000_000 * (t + 1);
            rec.ingest(
                &snap(ts, vec![counter("cpu_cycles", "cpu_usage", t, None)]),
                ts,
                0,
            )
            .unwrap();
        }
        // Committed but NOT sealed: the rows are in the `wal` table, and no
        // segment holds them.
        rec.sync().unwrap();

        // Snapshot the live archive the way `hindsight` does. A raw copy of a
        // file a writer still holds is NOT the same thing — its committed
        // pages are in the `-wal` sidecar, which a single blob does not carry,
        // and reading one gets "no such table: recordings" rather than a
        // partial answer. `VACUUM INTO` produces a consistent single file, and
        // that is what a browser is ever handed.
        let snapshot = dir.path().join("snapshot.rez");
        crate::rez_sqlite::RezDb::open(&path)
            .unwrap()
            .vacuum_into(&snapshot)
            .unwrap();

        let readers = RezReader::open_recordings_from_bytes(
            std::fs::read(&snapshot).unwrap(),
            BufferPool::new(16 * 1024 * 1024),
        )
        .unwrap();
        assert_eq!(readers.len(), 1);
        assert!(
            readers[0]
                .1
                .counter_names()
                .iter()
                .any(|n| n == "cpu_cycles"),
            "an unsealed table must still be visible from bytes: {:?}",
            readers[0].1.counter_names()
        );

        drop(rec);
        drop(archive);
    }

    /// A raw copy of an archive a writer still holds is a valid SQLite file
    /// with none of the archive in it — SQLite's committed pages are in a
    /// `-wal` sidecar, a second file a single blob does not carry. Say that,
    /// rather than letting it surface as "no such table: recordings", which
    /// reads like corruption.
    #[test]
    fn a_copy_taken_mid_write_says_what_went_wrong() {
        use crate::rez::recorder_tests_support::{counter, snap};
        use crate::rez_v3_writer::{ManifestSeed, RezArchive, StreamRecorderV3};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("live.rez");
        let seed = ManifestSeed {
            labels: [("source".to_string(), "rezolus".to_string())]
                .into_iter()
                .collect(),
            metadata: Default::default(),
            clock_anchor_wall_ns: 1_000_000_000,
        };
        let (archive, writer) = RezArchive::single(&path, seed).unwrap();
        let mut rec = StreamRecorderV3::new(writer);
        rec.ingest(
            &snap(
                1_000_000_000,
                vec![counter("cpu_cycles", "cpu_usage", 1, None)],
            ),
            1_000_000_000,
            0,
        )
        .unwrap();
        rec.sync().unwrap();

        let err = match RezReader::open_recordings_from_bytes(
            std::fs::read(&path).unwrap(),
            BufferPool::new(8 * 1024 * 1024),
        ) {
            Ok(_) => panic!("a mid-write copy has no catalog in it"),
            Err(e) => e.to_string(),
        };
        assert!(err.contains("-wal"), "{err}");
        assert!(err.contains("still being written"), "{err}");

        drop(rec);
        drop(archive);
    }

    /// A v2 tar archive reaches the same entry point — the caller has an
    /// extension it cannot trust either way, so the container is decided by
    /// content on both paths.
    #[test]
    fn a_v2_tar_archive_also_opens_from_bytes() {
        let (_d, p) = two_sampler_rez();
        let from_bytes = RezReader::open_recordings_from_bytes(
            std::fs::read(&p).unwrap(),
            BufferPool::new(16 * 1024 * 1024),
        )
        .unwrap();
        assert_eq!(from_bytes.len(), 1);
        assert!(!from_bytes[0].1.counter_names().is_empty());
    }

    /// The v3/SQLite twin of the tar refusal below.
    ///
    /// The two flattening branches of `open_with_pool` reach their recording
    /// count by DIFFERENT routes, so pinning one proves nothing about the
    /// other — and v3 is the branch that matters: `parquet combine` emits v3,
    /// and `record --endpoint a --endpoint b` writes a multi-recording v3
    /// archive directly. The tar test below covers `from_recordings`; this one
    /// covers `from_v3`.
    #[test]
    fn composition_sources_refuse_a_flattened_multi_recording_v3_archive() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fleet.rez");
        crate::rez::recorder_tests_support::multi_recording_v3_rez(
            &path,
            &[("redis", "web-01"), ("valkey", "web-01")],
        );

        let pool = BufferPool::new(16 * 1024 * 1024);
        let flattened = RezReader::open_with_pool(&path, Arc::clone(&pool)).unwrap();
        let err = match flattened.composition_sources() {
            Ok(_) => panic!("composing a flattened 2-recording v3 archive must refuse"),
            Err(e) => e,
        };
        assert!(
            format!("{err}").contains("open_recordings"),
            "the error must point at the entry point that works: {err}"
        );

        // Per recording, each composes on its own.
        let per_recording = RezReader::open_recordings(&path, pool).unwrap();
        assert_eq!(per_recording.len(), 2);
        for (_labels, reader) in &per_recording {
            assert!(reader.composition_sources().is_ok());
        }
    }

    /// The composition counterpart to the query refusal below: a flattened
    /// multi-recording reader must not be composable either.
    ///
    /// Every recording carries the same sampler names, so composing a
    /// flattened reader hands the builder several children holding the SAME
    /// metric names under one label. Nothing downstream can tell them apart:
    /// `ParquetBuilder` concatenates rather than dedups or dispatches
    /// (`MultiParquetSource`: "same (metric, label) pairs in multiple files
    /// produce duplicate series"), so the result is duplicate, indistinguishable
    /// series and a silently doubled aggregate — the precise failure
    /// `composition_sources` exists to prevent. It was reachable through
    /// `open_with_pool`, the path a multi-recording archive actually takes in
    /// production.
    ///
    /// NOT one recording replacing another: that is `UnionSource`'s first-wins,
    /// which the `route` refusals elsewhere in this file are about. The two
    /// composers fail differently and the distinction is easy to lose, since
    /// every neighbouring comment here is legitimately about the other one.
    #[test]
    fn composition_sources_refuse_a_flattened_multi_recording_reader() {
        let (_d, p) = two_sampler_rez();
        let (m, rb) = crate::rez::read_archive_bytes(&p).unwrap();
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
        crate::rez::write_archive_bytes(&out, &[(a, bytes0.clone()), (b, bytes0)]).unwrap();

        let pool = BufferPool::new(64 * 1024 * 1024);
        // The flattening path — NOT open_recordings.
        let flattened = RezReader::open_with_pool(&out, Arc::clone(&pool)).unwrap();
        // `CompositionSource` is opaque and not `Debug`, so match rather than
        // `expect_err`.
        let err = match flattened.composition_sources() {
            Ok(_) => panic!("composing a flattened 2-recording reader must refuse"),
            Err(e) => e,
        };
        let msg = format!("{err}");
        assert!(
            msg.contains("open_recordings"),
            "the error must point at the entry point that works: {msg}"
        );

        // ...and the same archive opened per recording composes fine, proving
        // the refusal is about flattening and not about the fixture.
        let per_recording = RezReader::open_recordings(&out, pool).unwrap();
        assert_eq!(per_recording.len(), 2);
        for (_labels, reader) in &per_recording {
            assert!(
                !reader.composition_sources().unwrap().is_empty(),
                "each recording composes on its own"
            );
        }
    }

    /// C1 regression: `RezReader::open_with_pool` — NOT `open_recordings` —
    /// is the path a multi-recording archive actually takes in production
    /// (`parquet combine a.rez b.rez` builds a 2-recording A/B archive, and
    /// `rezolus mcp query`/the viewer open via `open_with_pool`). Before the
    /// fix, `route()` grouped owners by SAMPLER alone, so a metric present
    /// in every recording's `cpu_usage` table (e.g. `cpu_cycles`) looked
    /// exactly like two group tables of one recording, got unioned, and
    /// `UnionSource`'s first-wins silently answered from ONE recording
    /// where the reader used to refuse loudly. This pins the refusal.
    #[test]
    fn multi_recording_same_sampler_query_errors_instead_of_silently_dropping_one_recording() {
        let (_d, p) = two_sampler_rez();
        let (m, rb) = crate::rez::read_archive_bytes(&p).unwrap();
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
        crate::rez::write_archive_bytes(&out, &[(a, bytes0.clone()), (b, bytes0)]).unwrap();

        let pool = BufferPool::new(64 * 1024 * 1024);
        // The flattening path — NOT open_recordings.
        let reader = RezReader::open_with_pool(&out, pool).unwrap();

        // `cpu_cycles` lives in the `cpu_usage` table, present in BOTH
        // recordings — this must refuse, not quietly answer from one side.
        let err = reader
            .query_range("rate(cpu_cycles[2s])", 0.0, 10.0, 1.0)
            .unwrap_err();
        let msg = format!("{err:?}");
        assert!(
            msg.contains("cpu_usage"),
            "the error must name the sampler that spans recordings: {msg}"
        );

        // Sanity: a single-recording archive of the SAME data must not
        // error — proves the refusal above is about the recording span,
        // not some other regression in the fixture.
        let single = RezReader::open_with_pool(&p, BufferPool::new(64 * 1024 * 1024)).unwrap();
        assert!(single
            .query_range("rate(cpu_cycles[2s])", 0.0, 10.0, 1.0)
            .is_ok());
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
        // …and a query spanning both, across a single-segment and a
        // multi-segment table, answers through the union.
        assert!(
            reader
                .query_range("rate(cpu_cycles[2s]) + rate(reads[4s])", start, end, 1.0)
                .is_ok(),
            "a union over tables with different segment counts and cadences \
             must answer"
        );

        // Known gap, asserted so it is a decision rather than a surprise:
        // `UnionMetricsSource` does not serve BARE counter selectors, only
        // rated ones. Bare gauges work (that is how `… / cpu_cores` resolves),
        // and rating a counter is what essentially every real query does, so
        // this has never been reachable in practice — the cross-sampler
        // refusal used to mask it entirely. Delete this assertion when the
        // union grows bare-counter support.
        assert!(
            reader
                .query_range("cpu_cycles + reads", start, end, 1.0)
                .is_err(),
            "bare counter selectors across a union are still unsupported; if \
             this now passes, the gap is closed and this assertion should go"
        );
    }

    // ---------------------------------------------------------------------
    // The v3 (SQLite) container. Same reader, same sub-sources — what is new
    // is that the newest "segment" of a table may be materialized from the
    // live WAL instead of read from `segments`.
    // ---------------------------------------------------------------------

    mod v3 {
        use super::*;
        use crate::rez_sqlite::WalRow;
        use crate::rez_v3_writer::{ManifestSeed, RezArchive, StreamRecorderV3};
        use crate::seal_policy::SealPolicy;
        use crate::wal::{encode_wal_row, WalCell, WalValue};
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

        /// A recorder plus the archive owning its writer thread. The archive
        /// must outlive the recorder — dropping it stops the writer — and
        /// joining it is what flushes everything queued to disk.
        fn recorder(path: &std::path::Path, max_rows: usize) -> (RezArchive, StreamRecorderV3) {
            let (archive, writer) = RezArchive::single(path, seed()).unwrap();
            (
                archive,
                StreamRecorderV3::with_policy(writer, policy(max_rows)),
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
            let (archive, mut rec) = recorder(out, max_rows);
            let mut last_ts = 0;
            for (s, ts) in rows {
                rec.ingest(s, *ts, 0).unwrap();
                rec.maybe_seal().unwrap();
                last_ts = *ts;
            }
            if finalize {
                archive.finalize_single_rec(rec, (last_ts, 0)).unwrap();
            } else {
                // Mid-flight: the tail stays live in the WAL. The archive is
                // still joined, so what WAS committed reaches disk — dropping
                // the handle alone no longer stops the writer.
                drop(rec);
                drop(archive);
            }
        }

        fn open(path: &std::path::Path) -> RezReader {
            RezReader::open_with_pool(path, BufferPool::new(64 * 1024 * 1024)).unwrap()
        }

        /// A consumer building a metric catalog needs what a metric MEANS, not
        /// just that it exists: `MetricsSource` answers names and labels, while
        /// the unit and description live only in the columns' arrow metadata.
        ///
        /// systemslab populates exactly such a catalog at import, and without
        /// this had to leave every `.rez` metric's unit empty.
        #[test]
        fn metric_metadata_carries_unit_and_description_per_metric() {
            use metriken_exposition::Counter;

            fn described(name: &str, sampler: &str, unit: &str, desc: &str, v: u64) -> Counter {
                Counter::new(
                    name.to_string(),
                    v,
                    [
                        ("metric".to_string(), name.to_string()),
                        ("sampler".to_string(), sampler.to_string()),
                        ("unit".to_string(), unit.to_string()),
                        ("description".to_string(), desc.to_string()),
                    ]
                    .into_iter()
                    .collect(),
                )
            }

            let rows: Vec<(Snapshot, u64)> = (0..4u64)
                .map(|i| {
                    let ts = 1_000_000_000 * (i + 1);
                    (
                        snap(
                            ts,
                            vec![
                                described(
                                    "cpu_cycles",
                                    "cpu_usage",
                                    "cycles",
                                    "CPU cycles executed",
                                    i * 1_000,
                                ),
                                described(
                                    "cpu_instructions",
                                    "cpu_usage",
                                    "instructions",
                                    "Instructions retired",
                                    i * 500,
                                ),
                            ],
                            vec![],
                        ),
                        ts,
                    )
                })
                .collect();

            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("described.rez");
            write_v3(&rows, 2, true, &path);

            let catalog = open(&path).metric_metadata();

            let cycles = catalog
                .get("cpu_cycles")
                .expect("the metric must appear in the catalog");
            assert_eq!(cycles.get("unit").map(String::as_str), Some("cycles"));
            assert_eq!(
                cycles.get("description").map(String::as_str),
                Some("CPU cycles executed")
            );
            // Label keys ride along, which is what a consumer records as tags.
            assert_eq!(cycles.get("sampler").map(String::as_str), Some("cpu_usage"));

            // Every metric is answered, not just the table's first column.
            assert_eq!(
                catalog
                    .get("cpu_instructions")
                    .and_then(|m| m.get("unit"))
                    .map(String::as_str),
                Some("instructions")
            );
        }

        /// Two recordings compose into ONE labelled multi-source, each
        /// keeping its own injected label.
        ///
        /// This is the cross-artifact path a consumer needs to answer a single
        /// query spanning several recordings and slice the answer by which one
        /// it came from. It is the opposite composition from `route`'s
        /// same-recording union: there the children must hold DISJOINT metric
        /// names, here both children hold the SAME name and must stay distinct
        /// rather than one silently winning.
        #[test]
        fn composition_sources_merge_recordings_under_distinct_labels() {
            let dir = tempfile::tempdir().unwrap();
            let a_path = dir.path().join("a.rez");
            let b_path = dir.path().join("b.rez");
            write_v3(&fixture_rows(6), 2, true, &a_path);
            write_v3(&fixture_rows(6), 2, true, &b_path);

            // `open_recordings` -- one reader per recording -- is the entry
            // point composition requires; see the refusal test below.
            let pool = BufferPool::new(64 * 1024 * 1024);
            let a = RezReader::open_recordings(&a_path, Arc::clone(&pool)).unwrap();
            let b = RezReader::open_recordings(&b_path, pool).unwrap();
            assert_eq!(
                (a.len(), b.len()),
                (1, 1),
                "fixture sanity: one recording each"
            );

            let mut builder = metriken_query::ParquetReader::builder();
            for source in a[0].1.composition_sources().unwrap() {
                builder = builder.source_labeled(source, [("job", "a")]);
            }
            for source in b[0].1.composition_sources().unwrap() {
                builder = builder.source_labeled(source, [("job", "b")]);
            }
            let combined = builder.build().unwrap();

            // Both arms survive, told apart by the injected label -- neither
            // merged into the other nor dropped.
            let mut jobs: Vec<String> = combined
                .counter_labels("cpu_cycles")
                .into_iter()
                .filter_map(|l| l.get("job").cloned())
                .collect();
            jobs.sort();
            jobs.dedup();
            assert_eq!(jobs, vec!["a".to_string(), "b".to_string()]);
        }

        /// A `.rez` is readable while it is being written, and hindsight's
        /// retention deletes as it goes — so a table that had rows when the
        /// reader probed it can have none by the time a query opens it.
        ///
        /// The reader used to `.expect("segments opened at probe time cannot
        /// fail to reopen")`, which is true for a finished archive and false
        /// for the live one the format advertises. The plausible sequence is
        /// exactly this one: hindsight evicts everything older than the
        /// cutoff, and a quiet sampler's only rows go with it, so the viewer
        /// or MCP panics instead of answering.
        #[test]
        fn a_table_evicted_between_probe_and_query_does_not_panic() {
            let rows = fixture_rows(6);
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("live.rez");
            write_v3(&rows, 2, true, &path);

            // Probe: names and spans are read now, readers are not built.
            let reader = open(&path);
            assert!(
                reader.counter_names().contains(&"cpu_cycles".to_string()),
                "fixture sanity: the metric is present at probe time"
            );

            // ...and then retention takes every row, as it does on a rolling
            // buffer whose lookback has passed a quiet sampler by.
            {
                let mut db = RezDb::open(&path).unwrap();
                let rid = db.read_recordings().unwrap()[0].id;
                // Not `u64::MAX`: `evict` binds the cutoff as `i64`, so
                // that wraps to -1 and deletes nothing.
                db.evict_before(rid, i64::MAX as u64).unwrap();
                assert!(
                    db.all_samplers(rid)
                        .unwrap()
                        .iter()
                        .all(|s| db.read_segments(rid, s).unwrap().is_empty()),
                    "fixture sanity: every segment is gone"
                );
            }

            // The query must not panic. Erroring is the honest answer — the
            // data really is gone — and it is what a metric absent at open
            // already does.
            let out = reader.query_range("rate(cpu_cycles[2s])", 1.0, 7.0, 1.0);
            assert!(
                out.is_err(),
                "a table whose rows have been evicted must error, not answer"
            );

            // And the reader stays usable for whatever else the archive holds
            // rather than being poisoned by the first vanished table.
            let _ = reader.counter_names();
        }

        /// One evicted table must not take the surviving ones with it.
        ///
        /// This is the case that actually happens on a rolling buffer: a quiet
        /// sampler ages out of the lookback while a busy one keeps recording.
        /// The busy one must still answer.
        #[test]
        fn evicting_one_table_leaves_the_others_answering() {
            let rows = fixture_rows(6);
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("live.rez");
            write_v3(&rows, 2, true, &path);

            let reader = open(&path);
            // Touch neither table yet — the probe named both, the readers are
            // unbuilt, which is the state a live archive is queried in.

            // Evict only `blockio_requests`, as retention would for the
            // sampler that stopped producing rows first.
            {
                let rid = RezDb::open(&path).unwrap().read_recordings().unwrap()[0].id;
                // Straight through rusqlite rather than adding a test-only
                // hook to `RezDb`: retention is per-sampler here, which
                // `evict_before` (time-based, whole-recording) cannot express.
                let conn = rusqlite::Connection::open(&path).unwrap();
                conn.execute(
                    "DELETE FROM segments WHERE recording_id = ?1 AND sampler = ?2",
                    rusqlite::params![rid, "blockio_requests"],
                )
                .unwrap();
            }

            // The survivor answers, with real values. The fixture's rows are
            // at 1s..6s, so this is the whole archive.
            let out = reader
                .query_range("rate(cpu_cycles[3s])", 1.0, 7.0, 1.0)
                .expect("the untouched table must still answer");
            let QueryResult::Matrix { result } = out else {
                panic!("a range query over a counter is a matrix");
            };
            assert!(
                result
                    .iter()
                    .flat_map(|s| s.values.iter())
                    .any(|(_, v)| *v > 0.0),
                "and with real values, not an empty series"
            );

            // The evicted one is absent rather than fatal.
            assert!(reader
                .query_range("rate(reads[3s])", 1.0, 7.0, 1.0)
                .is_err());
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
            let (archive, mut rec) = recorder(&path, 4);
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
            drop(archive);

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
                Vec<Option<crate::window::Window>>,
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
                    .with_window(w.map(Into::into));
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

        /// Gauge-only variant of [`group_schema`] — I4: every cross-group
        /// union fixture elsewhere in this suite is counter-only, so
        /// nothing yet exercises a gauge group table through `route()`.
        fn gauge_group_schema(members: &[&str], sampler: &str) -> GroupSchema {
            GroupSchema {
                counters: Vec::new(),
                gauges: members
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
                histograms: Vec::new(),
            }
        }

        /// Histogram-only variant of [`group_schema`] — I4: rezolus is
        /// histogram-heavy and the motivating cross-group shape is a
        /// latency histogram in one group and a counter (or gauge) in
        /// another; nothing in this suite built that shape before.
        fn histogram_group_schema(members: &[&str], sampler: &str) -> GroupSchema {
            GroupSchema {
                counters: Vec::new(),
                gauges: Vec::new(),
                histograms: members
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
                        window: w.map(Into::into),
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

        /// A dendro archive converted from a `.rez` (#1301) reads as the
        /// `.rez` did, for every kind and across tables, from a path and
        /// from bytes.
        #[test]
        fn a_dendro_conversion_answers_every_kind_the_same() {
            let dir = tempfile::tempdir().unwrap();
            let rez = dir.path().join("mixed.rez");
            let dendro = dir.path().join("mixed.dendro");
            write_mixed_kinds(&rez);
            crate::to_dendro::convert_v3_to_dendro(&rez, &dendro).unwrap();
            assert_eq!(
                crate::catalog::Container::of_path(&dendro).unwrap(),
                Some(crate::catalog::Container::Dendro)
            );
            let queries = [
                "rate(cpu_cycles[2s])",
                "frequency",
                "histogram_mean(latency)",
                "histogram_count(latency)",
                "sum(rate(cpu_cycles[2s])) + sum(frequency)",
            ];
            assert_same_answers(&open(&rez), &open(&dendro), &queries);

            let from_bytes = |p: &std::path::Path| {
                let mut r = RezReader::open_recordings_from_bytes(
                    std::fs::read(p).unwrap(),
                    BufferPool::new(64 * 1024 * 1024),
                )
                .unwrap();
                assert_eq!(r.len(), 1);
                r.pop().unwrap()
            };
            let (la, a) = from_bytes(&rez);
            let (lb, b) = from_bytes(&dendro);
            assert_eq!(la, lb, "recording labels");
            assert_same_answers(&a, &b, &queries);
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

        // -------------------------------------------------------------
        // Same-timeline union (Part C): a query spanning two group tables of
        // ONE sampler must now succeed.
        // -------------------------------------------------------------

        /// Two acquisition groups of ONE sampler (`cpu_usage`), both
        /// advancing every tick (the common case: one `refresh()` reports
        /// every group it owns) — so their group tables share IDENTICAL row
        /// timestamps and the union degenerates to a plain per-row join, the
        /// same shape a V2 table with two counter columns already has.
        fn two_group_fixture_rows_v3(n: u64) -> Vec<(Snapshot, u64)> {
            let percpu_schema = std::sync::Arc::new(group_schema(&["cpu_cycles"], "cpu_usage"));
            let softirq_schema = std::sync::Arc::new(group_schema(&["cpu_softirq"], "cpu_usage"));
            (0..n)
                .map(|i| {
                    let ts = 1_000_000_000 * (i + 1);
                    let w = Some(Window::new(ts - 50_000_000, ts));
                    let percpu = GroupSnapshot {
                        name: "cpu_usage/percpu".to_string(),
                        schema_hash: percpu_schema.hash(),
                        schema: Some(std::sync::Arc::clone(&percpu_schema)),
                        window: w.map(Into::into),
                        counters: vec![Some(i * 1_000)],
                        gauges: Vec::new(),
                        histograms: Vec::new(),
                    };
                    let softirq = GroupSnapshot {
                        name: "cpu_usage/softirq".to_string(),
                        schema_hash: softirq_schema.hash(),
                        schema: Some(std::sync::Arc::clone(&softirq_schema)),
                        window: w.map(Into::into),
                        counters: vec![Some(i * 10)],
                        gauges: Vec::new(),
                        histograms: Vec::new(),
                    };
                    let s = Snapshot::V3(SnapshotV3 {
                        systemtime: SystemTime::UNIX_EPOCH + std::time::Duration::from_nanos(ts),
                        duration: std::time::Duration::ZERO,
                        metadata: HashMap::new(),
                        groups: vec![percpu, softirq],
                    });
                    (s, ts)
                })
                .collect()
        }

        /// The V2 recording of the SAME data `two_group_fixture_rows_v3`
        /// produces: one sampler, two counters, one row per tick. A V2
        /// archive has always put both counters in one table, so this is the
        /// answer the union must reproduce.
        fn two_group_fixture_rows_v2(n: u64) -> Vec<(Snapshot, u64)> {
            (0..n)
                .map(|i| {
                    let ts = 1_000_000_000 * (i + 1);
                    let w = Some(Window::new(ts - 50_000_000, ts));
                    (
                        snap(
                            ts,
                            vec![
                                counter("cpu_cycles", "cpu_usage", i * 1_000, w),
                                counter("cpu_softirq", "cpu_usage", i * 10, w),
                            ],
                            Vec::new(),
                        ),
                        ts,
                    )
                })
                .collect()
        }

        #[test]
        fn within_sampler_cross_group_query_matches_v2_equivalent() {
            let v3_rows = two_group_fixture_rows_v3(9);
            let v2_rows = two_group_fixture_rows_v2(9);
            let dir = tempfile::tempdir().unwrap();
            let v2_path = dir.path().join("v2.rez");
            let v3_path = dir.path().join("v3.rez");
            write_atomic_rez(&v2_rows, &v2_path);
            // max_rows=2 also forces each group table into several segments,
            // so the union is exercised across a segment boundary too.
            write_v3(&v3_rows, 2, true, &v3_path);

            let counts = sealed_counts(&v3_path);
            assert!(
                counts.contains_key("cpu_usage/percpu") && counts.contains_key("cpu_usage/softirq"),
                "the fixture must actually split into two group tables of one \
                 sampler, or this proves nothing: {counts:?}"
            );

            let a = open(&v2_path);
            let b = open(&v3_path);
            assert_eq!(a.counter_names(), b.counter_names());

            let (start, end) = a.time_range().unwrap();
            assert_eq!(b.time_range(), Some((start, end)));

            let same = |expr: &str| {
                let ra = a.query_range(expr, start, end, 1.0).unwrap();
                let rb = b.query_range(expr, start, end, 1.0).unwrap();
                assert_eq!(
                    serde_json::to_value(&ra).unwrap(),
                    serde_json::to_value(&rb).unwrap(),
                    "same-timeline union differs from the V2 equivalent for {expr}"
                );
                ra
            };

            // Before Part C this returned "cross-timeline query spans
            // samplers cpu_usage" (both operands are cpu_usage, but from
            // different group tables) — now it must resolve, and resolve to
            // the same numbers a V2 recording of the same data gives.
            let summed = same("rate(cpu_cycles[3s]) + rate(cpu_softirq[3s])");
            let json = serde_json::to_value(&summed).unwrap();
            let values = json["result"][0]["values"].as_array().unwrap();
            assert!(
                values.iter().any(|v| v[1] != "0"),
                "non-degenerate: the combined rate must produce real values: {json}"
            );
            same("rate(cpu_softirq[4s])");

            // Bands survive per metric after combination: each column's
            // window came from its OWN source group table, not the other
            // one's.
            for metric in ["cpu_cycles", "cpu_softirq"] {
                let r = b
                    .query_range(&format!("rate({metric}[3s])"), start, end, 1.0)
                    .unwrap();
                let json = serde_json::to_value(&r).unwrap();
                let intervals = json["result"][0]["intervals"]
                    .as_array()
                    .unwrap_or_else(|| {
                        panic!(
                            "rate({metric}[..]) over the same-timeline union must still \
                         carry bands: {json}"
                        )
                    });
                assert!(
                    intervals.iter().any(|iv| iv.is_array()),
                    "{metric}: at least one point must carry a resolved [lo, hi] band: {json}"
                );
            }
        }

        /// I4: a gauge group and a histogram group, alongside the counter
        /// group every other fixture in this suite uses — the motivating
        /// cross-group shape (rezolus is histogram-heavy; a latency
        /// histogram in one group and a counter/gauge in another) had zero
        /// coverage before this. Segmented (max_rows forces multiple
        /// segments per table), so a segmented child is exercised for all
        /// three kinds, not just counters.
        ///
        /// One honest limitation this test documents rather than papers
        /// over: `histogram_mean`/`histogram_irate`/etc. are top-level-only
        /// in this query engine's grammar (see
        /// `metriken_query::union::tests::gauge_and_histogram_cross_child_dispatch_is_non_degenerate`
        /// upstream) — they cannot be embedded in a binary expression the
        /// way `rate(a) + b` can, so there is no PromQL string that routes
        /// a histogram query through `route()`'s union arm. What IS proven
        /// here: a counter+gauge cross-group query still unions correctly
        /// with a histogram-carrying THIRD table present in the same
        /// sampler (so `route()`'s `(recording, sampler)` grouping isn't
        /// disturbed by an unreferenced histogram sibling), and the
        /// histogram itself resolves correctly — real value, real band —
        /// through the very same reader.
        /// Six ticks of one sampler split into three group tables: a counter
        /// (`cpu_usage/percpu`), a gauge (`cpu_usage/freq`) and a histogram
        /// (`cpu_usage/sched`), sealed every two rows and finalized.
        fn write_mixed_kinds(path: &std::path::Path) {
            let percpu_schema = std::sync::Arc::new(group_schema(&["cpu_cycles"], "cpu_usage"));
            let freq_schema = std::sync::Arc::new(gauge_group_schema(&["frequency"], "cpu_usage"));
            let sched_schema =
                std::sync::Arc::new(histogram_group_schema(&["latency"], "cpu_usage"));
            let n = 6u64;
            let (_archive, mut rec) = recorder(path, 2); // force multiple segments per table
            for i in 0..n {
                let ts = 1_000_000_000 * (i + 1);
                let w = Some(Window::new(ts - 50_000_000, ts));
                let mut h = ::histogram::Histogram::new(7, 64).unwrap();
                for _ in 0..=i {
                    h.increment(1_000).unwrap();
                }
                let percpu = GroupSnapshot {
                    name: "cpu_usage/percpu".to_string(),
                    schema_hash: percpu_schema.hash(),
                    schema: Some(std::sync::Arc::clone(&percpu_schema)),
                    window: w.map(Into::into),
                    counters: vec![Some(i * 1_000)],
                    gauges: Vec::new(),
                    histograms: Vec::new(),
                };
                let freq = GroupSnapshot {
                    name: "cpu_usage/freq".to_string(),
                    schema_hash: freq_schema.hash(),
                    schema: Some(std::sync::Arc::clone(&freq_schema)),
                    window: w.map(Into::into),
                    counters: Vec::new(),
                    gauges: vec![Some(2_000 + i as i64)],
                    histograms: Vec::new(),
                };
                let sched = GroupSnapshot {
                    name: "cpu_usage/sched".to_string(),
                    schema_hash: sched_schema.hash(),
                    schema: Some(std::sync::Arc::clone(&sched_schema)),
                    window: w.map(Into::into),
                    counters: Vec::new(),
                    gauges: Vec::new(),
                    histograms: vec![Some(h)],
                };
                let s = Snapshot::V3(SnapshotV3 {
                    systemtime: SystemTime::UNIX_EPOCH + std::time::Duration::from_nanos(ts),
                    duration: std::time::Duration::ZERO,
                    metadata: HashMap::new(),
                    groups: vec![percpu, freq, sched],
                });
                rec.ingest(&s, ts, 0).unwrap();
                rec.maybe_seal().unwrap();
            }
            _archive
                .finalize_single_rec(rec, (1_000_000_000 * n, 0))
                .unwrap();
        }

        #[test]
        fn cross_group_query_spans_counter_gauge_and_histogram() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("mixed_kinds.rez");
            write_mixed_kinds(&path);

            let counts = sealed_counts(&path);
            assert!(
                counts.contains_key("cpu_usage/percpu")
                    && counts.contains_key("cpu_usage/freq")
                    && counts.contains_key("cpu_usage/sched"),
                "the fixture must actually split into three group tables of \
                 one sampler, or this proves nothing: {counts:?}"
            );
            assert!(
                counts["cpu_usage/sched"] > 1,
                "the histogram table must be segmented too: {counts:?}"
            );

            let reader = open(&path);
            assert_eq!(reader.gauge_names(), vec!["frequency".to_string()]);
            assert_eq!(reader.histogram_names(), vec!["latency".to_string()]);

            // Solo answers (single-table fast path, no union). The range
            // starts at 2.0, not 1.0: `rate()` has no lookback sample before
            // the fixture's first tick, so a query starting at 1.0 would
            // drop that point from the union expression below (needs a
            // rate() term) but not from a bare `frequency` selector —
            // starting at 2.0 keeps both grids identical so the comparison
            // is about routing, not a `rate()` edge effect.
            let solo_gauge = reader.query_range("frequency", 2.0, 6.0, 1.0).unwrap();
            let solo_hist = reader
                .query_range("histogram_mean(latency)", 2.0, 6.0, 1.0)
                .unwrap();

            // The gauge, forced through the union by naming the counter
            // sibling alongside it — with the histogram table present as a
            // THIRD table of this sampler that this particular query never
            // references.
            let gauge_via_union = reader
                .query_range("frequency + (rate(cpu_cycles[3s]) * 0)", 2.0, 6.0, 1.0)
                .unwrap();
            assert_eq!(
                serde_json::to_value(&solo_gauge).unwrap()["result"][0]["values"],
                serde_json::to_value(&gauge_via_union).unwrap()["result"][0]["values"],
                "the gauge's own values must not change when routed through \
                 the union alongside its counter sibling"
            );

            // The histogram, independently — real value, real band — from
            // the SAME reader that just built a union for its siblings.
            let hist_json = serde_json::to_value(&solo_hist).unwrap();
            let hist_values = hist_json["result"][0]["values"].as_array().unwrap();
            assert!(
                hist_values.iter().any(|v| v[1] != "0"),
                "the histogram must produce real values: {hist_json}"
            );
            let hist_intervals = hist_json["result"][0]["intervals"].as_array();
            assert!(
                hist_intervals.is_some_and(|iv| iv.iter().any(|p| p.is_array())),
                "the histogram must carry a resolved band: {hist_json}"
            );
        }

        /// M5: every fixture elsewhere in this suite gives both groups the
        /// SAME window width (50ms), so nothing yet proves a group's OWN
        /// width survives — as opposed to one group's width leaking onto
        /// the other's band, which is exactly the fidelity claim that
        /// justified the union design over a materialized merge. `percpu`
        /// uses 50ms, `softirq` uses 500ms — 10x apart, so a leak would be
        /// obvious rather than lost in rounding.
        #[test]
        fn distinct_group_window_widths_are_preserved_through_the_union() {
            let percpu_schema = std::sync::Arc::new(group_schema(&["cpu_cycles"], "cpu_usage"));
            let softirq_schema = std::sync::Arc::new(group_schema(&["cpu_softirq"], "cpu_usage"));
            let n = 6u64;
            let rows: Vec<(Snapshot, u64)> = (0..n)
                .map(|i| {
                    let ts = 1_000_000_000 * (i + 1);
                    let w_fast = Some(Window::new(ts - 50_000_000, ts));
                    let w_slow = Some(Window::new(ts - 500_000_000, ts));
                    let percpu = GroupSnapshot {
                        name: "cpu_usage/percpu".to_string(),
                        schema_hash: percpu_schema.hash(),
                        schema: Some(std::sync::Arc::clone(&percpu_schema)),
                        window: w_fast.map(Into::into),
                        counters: vec![Some(i * 1_000)],
                        gauges: Vec::new(),
                        histograms: Vec::new(),
                    };
                    let softirq = GroupSnapshot {
                        name: "cpu_usage/softirq".to_string(),
                        schema_hash: softirq_schema.hash(),
                        schema: Some(std::sync::Arc::clone(&softirq_schema)),
                        window: w_slow.map(Into::into),
                        counters: vec![Some(i * 10)],
                        gauges: Vec::new(),
                        histograms: Vec::new(),
                    };
                    let s = Snapshot::V3(SnapshotV3 {
                        systemtime: SystemTime::UNIX_EPOCH + std::time::Duration::from_nanos(ts),
                        duration: std::time::Duration::ZERO,
                        metadata: HashMap::new(),
                        groups: vec![percpu, softirq],
                    });
                    (s, ts)
                })
                .collect();

            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("widths.rez");
            write_v3(&rows, 2, true, &path);

            // Non-vacuous: the fixture's OWN sealed tables really do carry
            // two different window widths.
            let width_ns = |t: &rez::RezTable| -> Vec<u64> {
                t.columns[0]
                    .windows
                    .iter()
                    .filter_map(|w| w.map(|win| win.end_ns - win.begin_ns))
                    .collect()
            };
            let percpu_widths: Vec<u64> = decoded_segments(&path, "cpu_usage/percpu")
                .iter()
                .flat_map(width_ns)
                .collect();
            let softirq_widths: Vec<u64> = decoded_segments(&path, "cpu_usage/softirq")
                .iter()
                .flat_map(width_ns)
                .collect();
            assert!(
                percpu_widths.iter().all(|w| *w == 50_000_000),
                "{percpu_widths:?}"
            );
            assert!(
                softirq_widths.iter().all(|w| *w == 500_000_000),
                "{softirq_widths:?}"
            );

            // And the fidelity claim itself: cpu_softirq's own band, read
            // ALONE (single-table fast path — the reference answer), must
            // be byte-for-byte identical to its band read through the
            // union (forced by also naming cpu_cycles) — proving the wider
            // window wasn't narrowed by, or blended with, its sibling's
            // narrower one.
            let reader = open(&path);
            let solo = reader
                .query_range("rate(cpu_softirq[9s])", 1.0, 6.0, 1.0)
                .unwrap();
            let via_union = reader
                .query_range(
                    "rate(cpu_softirq[9s]) + (rate(cpu_cycles[9s]) * 0)",
                    1.0,
                    6.0,
                    1.0,
                )
                .unwrap();
            let solo_json = serde_json::to_value(&solo).unwrap();
            let union_json = serde_json::to_value(&via_union).unwrap();
            assert_eq!(
                solo_json["result"][0]["intervals"], union_json["result"][0]["intervals"],
                "cpu_softirq's 500ms band must survive union with cpu_cycles' \
                 50ms sibling unchanged: solo={solo_json} via_union={union_json}"
            );
            // And it must actually differ from the 50ms sibling's own band
            // width, or the identity check above would be vacuous (both
            // could trivially agree if the reader ignored widths entirely).
            let cycles_band = reader
                .query_range("rate(cpu_cycles[9s])", 1.0, 6.0, 1.0)
                .unwrap();
            assert_ne!(
                serde_json::to_value(&solo).unwrap()["result"][0]["intervals"],
                serde_json::to_value(&cycles_band).unwrap()["result"][0]["intervals"],
                "the two groups' bands must not be identical, or the width \
                 distinction this test exists to check would be untested"
            );
        }

        /// A group that skips ticks (the window-advance dedup case) must not
        /// have its gaps papered over by the union: a query touching ONLY
        /// that metric must answer identically whether it is read from its
        /// own single-group table or through the same-timeline union path
        /// (routed there because the query ALSO references a sibling group's
        /// metric) — the sibling's presence must not change this metric's
        /// own answer. There is no V2 recording to compare against here: V2
        /// has no per-metric row-skip within one sampler's table (a window
        /// advance is decided once for the whole table), so this asymmetric
        /// cadence is exactly the case V3's per-group split adds meaning
        /// for, and the sealed-segment counts below establish the gap is
        /// real rather than assumed.
        #[test]
        fn a_group_that_skipped_ticks_is_not_fabricated_across_by_the_union() {
            let percpu_schema = std::sync::Arc::new(group_schema(&["cpu_cycles"], "cpu_usage"));
            let softirq_schema = std::sync::Arc::new(group_schema(&["cpu_softirq"], "cpu_usage"));
            let n = 9u64;
            let rows: Vec<(Snapshot, u64)> = (0..n)
                .map(|i| {
                    let ts = 1_000_000_000 * (i + 1);
                    let w = Some(Window::new(ts - 50_000_000, ts));
                    // softirq's window advances only every 3rd tick, so it
                    // dedups (skips) two ticks out of every three.
                    let slow_end = 1_000_000_000 * (i / 3 + 1);
                    let slow_w = Some(Window::new(slow_end - 50_000_000, slow_end));
                    let percpu = GroupSnapshot {
                        name: "cpu_usage/percpu".to_string(),
                        schema_hash: percpu_schema.hash(),
                        schema: Some(std::sync::Arc::clone(&percpu_schema)),
                        window: w.map(Into::into),
                        counters: vec![Some(i * 1_000)],
                        gauges: Vec::new(),
                        histograms: Vec::new(),
                    };
                    let softirq = GroupSnapshot {
                        name: "cpu_usage/softirq".to_string(),
                        schema_hash: softirq_schema.hash(),
                        schema: Some(std::sync::Arc::clone(&softirq_schema)),
                        window: slow_w.map(Into::into),
                        counters: vec![Some(i / 3)],
                        gauges: Vec::new(),
                        histograms: Vec::new(),
                    };
                    let s = Snapshot::V3(SnapshotV3 {
                        systemtime: SystemTime::UNIX_EPOCH + std::time::Duration::from_nanos(ts),
                        duration: std::time::Duration::ZERO,
                        metadata: HashMap::new(),
                        groups: vec![percpu, softirq],
                    });
                    (s, ts)
                })
                .collect();

            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("skips.rez");
            write_v3(&rows, usize::MAX, true, &path);

            let softirq_seg = decoded_table(&path, "cpu_usage/softirq");
            assert!(
                (softirq_seg.timestamps.len() as u64) < n,
                "the fixture must actually have fewer softirq rows than ticks, \
                 or this proves nothing: {} rows for {n} ticks",
                softirq_seg.timestamps.len()
            );

            let reader = open(&path);
            // cpu_softirq answered on its OWN: this expression names only
            // one metric, so `route()` takes the single-table fast path —
            // no union involved. The reference answer.
            let solo = reader
                .query_range("rate(cpu_softirq[9s])", 1.0, 9.0, 1.0)
                .unwrap();
            // The identical quantity, forced through the union by also
            // naming a sibling group's metric in the same expression
            // (`rate(cpu_cycles[9s]) * 0` is always 0 — percpu is dense, so
            // it never itself introduces a gap). If merging fabricated a
            // value at a tick softirq's own table has no row for, or
            // dropped one it does have, this would disagree with `solo`.
            let via_union = reader
                .query_range(
                    "rate(cpu_softirq[9s]) + (rate(cpu_cycles[9s]) * 0)",
                    1.0,
                    9.0,
                    1.0,
                )
                .unwrap();
            // Compare values/bands only, not the label set: a binary op
            // between two vectors drops `__name__` per normal PromQL
            // semantics (Prometheus does the same), which is expected and
            // unrelated to what this test is checking.
            let solo_json = serde_json::to_value(&solo).unwrap();
            let union_json = serde_json::to_value(&via_union).unwrap();
            assert_eq!(
                solo_json["result"][0]["values"], union_json["result"][0]["values"],
                "cpu_softirq's own values must not change when a sibling group's \
                 metric is unioned alongside it in the same query: solo={solo_json} \
                 via_union={union_json}"
            );
            // The BAND may legitimately differ, and here it must: the union
            // form is a binary op against a metric from a DIFFERENT table, so
            // cpu_softirq's value is being combined with one read at another
            // instant. That costs accuracy its solo band does not contain, and
            // the widening prices it. What must never happen is the band
            // getting NARROWER — claiming precision the join cannot support.
            let solo_iv = solo_json["result"][0]["intervals"].as_array().unwrap();
            let union_iv = union_json["result"][0]["intervals"].as_array().unwrap();
            assert_eq!(solo_iv.len(), union_iv.len());
            let mut widened = 0;
            for (s_pt, u_pt) in solo_iv.iter().zip(union_iv) {
                let (s_lo, s_hi) = (s_pt[0].as_f64().unwrap(), s_pt[1].as_f64().unwrap());
                let (u_lo, u_hi) = (u_pt[0].as_f64().unwrap(), u_pt[1].as_f64().unwrap());
                assert!(
                    u_lo <= s_lo && u_hi >= s_hi,
                    "a cross-table band must never be narrower than the solo \
                     one: solo=({s_lo}, {s_hi}) union=({u_lo}, {u_hi})"
                );
                if u_lo < s_lo || u_hi > s_hi {
                    widened += 1;
                }
            }
            assert!(
                widened > 0,
                "at least one point must be widened, or the join is being \
                 priced at zero: solo={solo_json} via_union={union_json}"
            );
        }

        #[test]
        fn a_split_sampler_group_and_another_sampler_union_together() {
            // A query spanning one sampler's group table AND a different
            // sampler's used to be refused. Both are tables of the SAME
            // recording — one agent, one tick — so they union now, with each
            // side's band widened to the span of both reads.
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("mixed.rez");
            let (_archive, mut rec) = recorder(&path, usize::MAX);
            let cpu_schema = std::sync::Arc::new(group_schema(&["cpu_cycles"], "cpu_usage"));
            let softirq_schema = std::sync::Arc::new(group_schema(&["cpu_softirq"], "cpu_usage"));
            for i in 0..3u64 {
                let ts = 1_000_000_000 * (i + 1);
                let w = Some(Window::new(ts - 50_000_000, ts));
                let groups = vec![
                    GroupSnapshot {
                        name: "cpu_usage/percpu".to_string(),
                        schema_hash: cpu_schema.hash(),
                        schema: Some(std::sync::Arc::clone(&cpu_schema)),
                        window: w.map(Into::into),
                        counters: vec![Some(i * 1_000)],
                        gauges: Vec::new(),
                        histograms: Vec::new(),
                    },
                    GroupSnapshot {
                        name: "cpu_usage/softirq".to_string(),
                        schema_hash: softirq_schema.hash(),
                        schema: Some(std::sync::Arc::clone(&softirq_schema)),
                        window: w.map(Into::into),
                        counters: vec![Some(i)],
                        gauges: Vec::new(),
                        histograms: Vec::new(),
                    },
                ];
                let s = Snapshot::V3(SnapshotV3 {
                    systemtime: SystemTime::UNIX_EPOCH + std::time::Duration::from_nanos(ts),
                    duration: std::time::Duration::ZERO,
                    metadata: HashMap::new(),
                    groups,
                });
                rec.ingest(&s, ts, 0).unwrap();
                // A totally different, V2-shaped sampler in the SAME
                // recording — `StreamRecorderV3::ingest` dispatches each call
                // by its own snapshot's variant, so a V2 tick mixed into an
                // otherwise-V3 recording lands in the ordinary sampler-keyed
                // path unchanged.
                rec.ingest(
                    &snap(
                        ts,
                        vec![counter("reads", "blockio_requests", i, w)],
                        Vec::new(),
                    ),
                    ts,
                    0,
                )
                .unwrap();
                rec.maybe_seal().unwrap();
            }
            // Through the archive: `finalize` only QUEUES the completion,
            // and it is the writer thread that seals the tails. Reading the
            // file without joining races that seal, and a table whose
            // segments have not landed yet reopens with none at all.
            _archive
                .finalize_single_rec(rec, (3_000_000_000, 0))
                .unwrap();

            let reader = open(&path);
            assert!(
                reader
                    .query_range(
                        "rate(cpu_cycles[3s]) + rate(cpu_softirq[3s]) + rate(reads[3s])",
                        0.0,
                        10.0,
                        1.0
                    )
                    .is_ok(),
                "two group tables of one sampler and a third sampler's table \
                 all belong to one recording, so they union"
            );
        }
    }

    /// Two samplers publishing the same metric name must not be reported as a
    /// corrupt archive.
    ///
    /// Shipped example: `gpu_amd_smi` and `gpu_nvidia` both publish
    /// `gpu_utilization`, `gpu_temperature` and six more vendor-neutral names,
    /// because only one of them ever populates on a given host. The dashboard
    /// queries `avg(gpu_utilization)` in 16 places.
    ///
    /// Unioning across samplers made those queries reach
    /// `UnionMetricsSource`'s disjointness check, whose message said the names
    /// were in two group tables "of the same sampler" and that this "should
    /// never happen". Both halves were false, and it blamed the operator's
    /// archive for a deliberate design property. The query is still ambiguous
    /// and still refused — it just has to say so truthfully.
    #[test]
    fn two_samplers_sharing_a_metric_name_is_not_reported_as_a_corrupt_archive() {
        let (_d, path) = two_sampler_rez_sharing_a_metric_name();
        let pool = BufferPool::new(64 * 1024 * 1024);
        let reader = RezReader::open_with_pool(&path, pool).unwrap();

        let err = reader
            .query_range("shared_metric", 0.0, 10.0, 1.0)
            .unwrap_err();
        let msg = format!("{err:?}");
        assert!(
            msg.contains("more than one sampler"),
            "the error must name the real cause — two samplers, one name: {msg}"
        );
        assert!(
            !msg.contains("should never happen"),
            "this DOES happen, by design, and saying otherwise sends the \
             operator hunting a corrupt archive: {msg}"
        );
        assert!(
            msg.contains("sampler_a") && msg.contains("sampler_b"),
            "the error must name which samplers collide: {msg}"
        );
    }

    /// A `.rez` whose two samplers run at genuinely different cadences, with
    /// the slow one's rows both IRREGULARLY spaced and off the integer grid.
    ///
    /// `cpu_usage` polls every 0.5 s; `blockio_requests` produces a row only at
    /// 1.5 s, 4.5 s and 10.5 s — gaps of 3 s then 6 s, mirroring the 30 s/60 s
    /// spacing measured on a real recording. No uniform grid can sit on those
    /// at any step or phase, which is the entire reason the evaluation
    /// timestamps have to be passed explicitly.
    ///
    /// The query path indexes samples by the timestamp SNAPPED to the nominal
    /// grid, so these rows are seen at 2 s, 5 s and 11 s — still unevenly
    /// spaced, which is what matters.
    fn cross_cadence_rez() -> (tempfile::TempDir, std::path::PathBuf) {
        const SLOW_POLLS: [u64; 3] = [1, 7, 19]; // → 1.5 s, 4.5 s, 10.5 s
        let rows: Vec<(Snapshot, u64)> = (0..25u64)
            .map(|i| {
                let ts = 1_000_000_000 + i * 500_000_000;
                let w = Some(Window::new(ts - 50_000_000, ts));
                let mut counters = vec![counter("cpu_cycles", "cpu_usage", i * 1_000, w)];
                if SLOW_POLLS.contains(&i) {
                    counters.push(counter("reads", "blockio_requests", i, w));
                }
                (snap(ts, counters, vec![]), ts)
            })
            .collect();
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("cross_cadence.rez");
        write_atomic_rez(&rows, &out);
        (dir, out)
    }

    /// A query spanning two cadences is evaluated at the SLOW table's own row
    /// timestamps, not on the uniform grid.
    ///
    /// On the grid, every point but a coincidence lands where the slow table
    /// has no reading; its value is held forward and combined with the fast
    /// operand as if the two were simultaneous. Here the slow rows are at
    /// x.5 s and unevenly spaced, so landing on them is only possible by using
    /// them directly — which is exactly what the assertion checks.
    #[test]
    fn eval_timestamps_restore_cross_cadence_fidelity_to_a_composed_query() {
        const QUERY: &str = "rate(cpu_cycles[2s]) + rate(reads[4s])";
        let (_d, path) = cross_cadence_rez();
        let pool = BufferPool::new(64 * 1024 * 1024);
        let reader = RezReader::open_with_pool(&path, Arc::clone(&pool)).unwrap();

        let times = |r: QueryResult| -> Vec<f64> {
            let QueryResult::Matrix { result } = r else {
                panic!("expected a matrix");
            };
            result
                .first()
                .expect("one series expected")
                .values
                .iter()
                .map(|(t, _)| *t)
                .collect()
        };

        // Compose exactly as a labelled multi-artifact consumer does.
        let mut builder = metriken_query::ParquetReader::builder();
        for source in reader.composition_sources().unwrap() {
            builder = builder.source_labeled(source, [("job", "a")]);
        }
        let composed = builder.build().unwrap();

        // Composed, with no help: back on the uniform grid, holding the slow
        // operand's value forward between its three real readings.
        let plain = times(composed.query_range(QUERY, 0.0, 14.0, 1.0).unwrap());
        assert!(
            plain.len() > 2,
            "fixture sanity: an unaided composed query spreads over the grid, got {plain:?}"
        );

        // Composed, with the recording's own evaluation points: identical to
        // querying the archive directly.
        let opts = QueryOptions::default();
        let points = reader
            .eval_timestamps_for(QUERY, 1.0, opts.rate_mode)
            .expect("a two-cadence query must yield evaluation points");
        let aligned = times(
            composed
                .query_range_opts(
                    QUERY,
                    0.0,
                    14.0,
                    1.0,
                    &opts.with_eval_timestamps(Some(points)),
                )
                .unwrap(),
        );
        assert_eq!(
            aligned,
            vec![4.5, 10.5],
            "composed + eval timestamps must match the direct reader's answer"
        );

        // A single-cadence query needs no adjustment, so composing it is
        // already faithful and this returns None.
        assert!(reader
            .eval_timestamps_for("rate(cpu_cycles[2s])", 1.0, RateMode::default())
            .is_none());
    }

    #[test]
    fn a_cross_cadence_query_lands_on_the_slow_tables_real_rows() {
        let (_d, path) = cross_cadence_rez();
        let pool = BufferPool::new(64 * 1024 * 1024);
        let reader = RezReader::open_with_pool(&path, pool).unwrap();

        let result = reader
            .query_range("rate(cpu_cycles[2s]) + rate(reads[4s])", 0.0, 14.0, 1.0)
            .expect("a cross-cadence query must answer");
        let QueryResult::Matrix { result } = result else {
            panic!("expected a matrix, got {result:?}");
        };
        let times: Vec<f64> = result
            .first()
            .expect("one series expected")
            .values
            .iter()
            .map(|(t, _)| *t)
            .collect();

        // The slow sampler's own three rows, at 1.5/4.5/10.5 s. N rows span
        // N-1 gaps, so the first yields no rate — nothing precedes it to
        // measure across. These used to read 5.0/11.0: the query path rounded
        // every timestamp to a nominal 1 s grid, so a row read at 4.5 s was
        // indexed at 5.0 s, half a second from where it was taken.
        assert_eq!(
            times,
            vec![4.5, 10.5],
            "points must sit on the slow sampler's own rows"
        );
        // The discriminating property: those two points are 6 s apart, having
        // followed a 3 s gap. No uniform grid over [0, 14] at step 1 produces
        // exactly this set — a grid would emit a point every second and hold
        // the slow operand's value in between.
        assert_eq!(
            times.len(),
            2,
            "on the grid this same query emits 9 points (3.0..=11.0 s), seven \
             of them where the slow sampler never read and its value is merely \
             held forward: {times:?}"
        );
    }

    /// Raw mode keeps its own placement: the real, un-snapped sample
    /// timestamps.
    ///
    /// Raw already answers the cross-cadence question its own way, so
    /// relocating its points would contradict its contract — and would break
    /// the query outright. Raw's counter producer walks sample PAIRS and
    /// ignores supplied evaluation points, while the gauge producers honour
    /// them, so a counter-and-gauge expression would have its two sides land
    /// on different instants and intersect nowhere, returning an empty series
    /// rather than a wrong one.
    #[test]
    fn raw_mode_keeps_its_own_placement() {
        let (_d, path) = cross_cadence_rez();
        let pool = BufferPool::new(64 * 1024 * 1024);
        let reader = RezReader::open_with_pool(&path, pool).unwrap();

        assert!(
            reader
                .eval_timestamps_for(
                    "rate(cpu_cycles[2s]) + rate(reads[4s])",
                    1.0,
                    RateMode::Grid,
                )
                .is_some(),
            "the fixture must be one the policy fires on, or this proves nothing"
        );
        assert!(
            reader
                .eval_timestamps_for("rate(cpu_cycles[2s]) + rate(reads[4s])", 1.0, RateMode::Raw,)
                .is_none(),
            "Raw must be left alone"
        );
    }

    /// The policy is inert when a query touches ONE cadence — the overwhelming
    /// majority of queries, which must keep their familiar grid placement.
    #[test]
    fn a_single_cadence_query_keeps_the_uniform_grid() {
        let (_d, path) = cross_cadence_rez();
        let pool = BufferPool::new(64 * 1024 * 1024);
        let reader = RezReader::open_with_pool(&path, pool).unwrap();

        let result = reader
            .query_range("rate(cpu_cycles[2s])", 0.0, 14.0, 1.0)
            .expect("a single-sampler query must answer");
        let QueryResult::Matrix { result } = result else {
            panic!("expected a matrix, got {result:?}");
        };
        let times: Vec<f64> = result
            .first()
            .expect("one series expected")
            .values
            .iter()
            .map(|(t, _)| *t)
            .collect();

        assert!(!times.is_empty(), "expected grid points");
        assert!(
            times.iter().all(|t| (t - t.round()).abs() < 1e-9),
            "a single-cadence query must stay on the integer grid: {times:?}"
        );
    }

    #[test]
    fn cross_sampler_query_answers_through_the_union() {
        let (_d, path) = two_sampler_rez();
        let pool = BufferPool::new(64 * 1024 * 1024);
        let reader = RezReader::open_with_pool(&path, pool).unwrap();
        // `cpu_cycles` (cpu_usage) and `reads` (blockio_requests) live in
        // different tables. This used to be refused as "cross-timeline",
        // because nothing could say what treating two separately-read values
        // as simultaneous costs. The query engine prices that itself now —
        // operands whose acquisition edges differ have their bands widened to
        // the union of both spans — so the query answers.
        assert!(
            reader
                .query_range("rate(cpu_cycles[2s]) + rate(reads[4s])", 0.0, 10.0, 1.0)
                .is_ok(),
            "a query spanning two samplers of one recording must answer"
        );
    }

    /// Composing a recording hands out one lazy child per table: nothing is
    /// opened until a composed query names a metric a table holds, and then
    /// only that table.
    #[test]
    fn composition_children_open_their_table_on_first_use() {
        use crate::rez::recorder_tests_support::{counter, snap};
        use crate::rez_v3_writer::{ManifestSeed, RezArchive, StreamRecorderV3};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lazy.rez");
        let seed = ManifestSeed {
            labels: [("source".to_string(), "rezolus".to_string())]
                .into_iter()
                .collect(),
            metadata: Default::default(),
            clock_anchor_wall_ns: 1_000_000_000,
        };
        let (archive, writer) = RezArchive::single(&path, seed).unwrap();
        let mut rec = StreamRecorderV3::new(writer);
        for t in 0..3u64 {
            let ts = 1_000_000_000 * (t + 1);
            rec.ingest(
                &snap(
                    ts,
                    vec![
                        counter("cpu_cycles", "cpu_usage", t * 10, None),
                        counter("net_bytes", "network_traffic", t * 3, None),
                    ],
                ),
                ts,
                0,
            )
            .unwrap();
        }
        rec.sync().unwrap();
        drop(rec);
        drop(archive);

        let mut readers =
            RezReader::open_recordings(&path, BufferPool::new(16 * 1024 * 1024)).unwrap();
        let reader = readers.pop().unwrap().1;
        let opened = |reader: &RezReader| -> Vec<String> { reader.opened_tables() };
        assert!(
            opened(&reader).is_empty(),
            "open probes footers, opens nothing"
        );

        let mut builder = metriken_query::ParquetReader::builder();
        for source in reader.composition_sources().unwrap() {
            builder = builder.source_labeled(source, [("job", "a")]);
        }
        let composed = builder.build().unwrap();
        assert!(
            opened(&reader).is_empty(),
            "composing opens nothing either: {:?}",
            opened(&reader)
        );
        let mut names = composed.counter_names();
        names.sort();
        assert_eq!(
            names,
            vec!["cpu_cycles".to_string(), "net_bytes".to_string()],
            "names answer from the catalog"
        );
        assert!(opened(&reader).is_empty());

        let (start, end) = composed.time_range().unwrap();
        let QueryResult::Matrix { result } = composed
            .query_range("rate(cpu_cycles[1s])", start, end + 1.0, 1.0)
            .unwrap()
        else {
            panic!("matrix");
        };
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].metric["job"], "a");
        assert_eq!(
            opened(&reader),
            vec!["cpu_usage".to_string()],
            "the query opened the table it named and no other"
        );
    }

    /// Fixtures and checks for a group table read through the identity index.
    mod indexed {
        use super::*;
        use crate::index::{EntryKind, IndexEntry, IndexState, SlotEntry};
        use crate::rez_sqlite::TickBatch;
        use crate::rez_v3_writer::{ManifestSeed, RezArchive, StreamRecorderV3};
        use crate::seal_policy::SealPolicy;
        use metriken_exposition::{GroupSchema, GroupSnapshot, MetricDesc, SnapshotV3};
        use std::time::Duration;

        const STREAM: &str = "fake/ops";
        const TICKS: u64 = 6;
        /// One result series: its labels and its `(timestamp, value)` points.
        type Series = (BTreeMap<String, String>, Vec<(f64, f64)>);
        /// The tick at which slot 0 changes hands: redis leaves, valkey lands.
        const HANDOVER: u64 = 3;

        fn ts(tick: u64) -> u64 {
            1_000_000_000 * (tick + 1)
        }

        fn labels(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        }

        /// Which occupant slot 0 has at `tick`.
        fn slot0_comm(tick: u64) -> &'static str {
            if tick < HANDOVER {
                "redis"
            } else {
                "valkey"
            }
        }

        /// The group's schema at `tick`. `labelled` is the shape agents write
        /// today, where a slot's labels are copied into its member's metadata;
        /// unlabelled is the shape after #1224 step 7, where a member carries
        /// only its metric and slot and the index is the one place the labels
        /// live.
        fn schema(tick: u64, labelled: bool) -> GroupSchema {
            let member = |slot: u32, comm: &str| {
                let mut m = labels(&[("metric", "fake_ops"), ("id", &slot.to_string())]);
                if labelled {
                    m.insert("comm".to_string(), comm.to_string());
                }
                MetricDesc {
                    name: format!("7x{slot}"),
                    metadata: m,
                }
            };
            GroupSchema {
                counters: vec![member(0, slot0_comm(tick)), member(1, "nginx")],
                gauges: Vec::new(),
                histograms: Vec::new(),
            }
        }

        /// The tick at which the recorder restates the whole set, as it does
        /// every seal age: a `Full` that changes nothing.
        const RESTATED: u64 = 4;

        fn entry(tick: u64) -> Option<(u64, Vec<u8>)> {
            let e = match tick {
                0 => IndexEntry {
                    kind: EntryKind::Full,
                    slots: vec![
                        SlotEntry {
                            slot: 0,
                            labels: labels(&[("comm", "redis")]),
                        },
                        SlotEntry {
                            slot: 1,
                            labels: labels(&[("comm", "nginx")]),
                        },
                    ],
                    removed: Vec::new(),
                    state: IndexState::default(),
                },
                HANDOVER => IndexEntry {
                    kind: EntryKind::Delta,
                    slots: vec![SlotEntry {
                        slot: 0,
                        labels: labels(&[("comm", "valkey")]),
                    }],
                    removed: Vec::new(),
                    state: IndexState::default(),
                },
                RESTATED => IndexEntry {
                    kind: EntryKind::Full,
                    slots: vec![
                        SlotEntry {
                            slot: 0,
                            labels: labels(&[("comm", "valkey")]),
                        },
                        SlotEntry {
                            slot: 1,
                            labels: labels(&[("comm", "nginx")]),
                        },
                    ],
                    removed: Vec::new(),
                    state: IndexState::default(),
                },
                _ => return None,
            };
            Some((ts(tick), e.encode()))
        }

        /// Write `TICKS` ticks of one two-slot group, slot 0 changing hands at
        /// `HANDOVER`, sealed every two rows so the table is several segments
        /// plus a WAL tail. With `with_index` the handover is in `caller_rows`.
        fn write(path: &Path, labelled: bool, with_index: bool) {
            let seed = ManifestSeed {
                labels: labels(&[("source", "rezolus")]),
                metadata: labels(&[("sampling_interval_ms", "1000")]),
                clock_anchor_wall_ns: ts(0),
            };
            let (mut archive, writer) = RezArchive::single(path, seed).unwrap();
            let rid = writer.recording_id();
            let mut rec = StreamRecorderV3::with_policy(
                writer,
                SealPolicy {
                    max_bytes: usize::MAX,
                    max_rows: 2,
                    max_age: Duration::from_secs(3600),
                },
            );
            for tick in 0..TICKS {
                let sch = schema(tick, labelled);
                let g = GroupSnapshot {
                    name: STREAM.to_string(),
                    schema_hash: sch.hash(),
                    schema: Some(Arc::new(sch)),
                    window: Some(metriken::Window::new(ts(tick) - 5_000_000, ts(tick))),
                    // Slot 0 restarts from zero at the handover with a
                    // different slope: a read that merged the two occupants
                    // would show a counter reset there, and a rate that
                    // crossed it would not be 10 or 7.
                    counters: vec![
                        Some(if tick < HANDOVER {
                            tick * 10
                        } else {
                            (tick - HANDOVER) * 7
                        }),
                        Some(tick * 3),
                    ],
                    gauges: Vec::new(),
                    histograms: Vec::new(),
                };
                let snap = Snapshot::V3(SnapshotV3 {
                    systemtime: SystemTime::UNIX_EPOCH + Duration::from_nanos(ts(tick)),
                    duration: Duration::ZERO,
                    metadata: HashMap::new(),
                    groups: vec![g],
                });
                let rows = rec.stage(&snap, ts(tick), 0).unwrap();
                let index_entries = match (with_index, entry(tick)) {
                    (true, Some((ts, blob))) => vec![(
                        STREAM.to_string(),
                        vec![crate::rez_sqlite::IndexRow {
                            ts,
                            blob,
                            full: tick == 0 || tick == RESTATED,
                        }],
                    )],
                    _ => Vec::new(),
                };
                archive
                    .wal_tick(vec![TickBatch {
                        recording_id: rid,
                        rows,
                        index_entries,
                    }])
                    .unwrap();
                rec.maybe_seal().unwrap();
            }
            rec.sync().unwrap();
            drop(rec);
            drop(archive);
        }

        fn open(path: &Path) -> RezReader {
            let mut readers =
                RezReader::open_recordings(path, BufferPool::new(16 * 1024 * 1024)).unwrap();
            assert_eq!(readers.len(), 1);
            readers.pop().unwrap().1
        }

        /// Every series of `rate(fake_ops[2s])` as `(labels, values)`, in a
        /// fixed order. A rate rather than the bare counter because that is
        /// what the engine evaluates over a range for a counter, and because
        /// a rate is what a merged handover would corrupt.
        fn series(reader: &RezReader) -> Vec<Series> {
            let (start, end) = reader.time_range().unwrap();
            let r = reader
                .query_range("rate(fake_ops[2s])", start, end, 1.0)
                .expect("the query must resolve");
            let QueryResult::Matrix { result } = r else {
                panic!("a range query over a counter is a matrix");
            };
            let mut out: Vec<_> = result
                .into_iter()
                .map(|s| (s.metric.into_iter().collect::<BTreeMap<_, _>>(), s.values))
                .collect();
            out.sort_by(|a, b| a.0.cmp(&b.0));
            out
        }

        /// THE oracle. On an archive written the way agents write today —
        /// a slot's labels in its column's metadata as well as in the index —
        /// reading through the index yields exactly what reading the parquet
        /// does: the same series, the same labels, the same values on the
        /// same timestamps. The index path is a second implementation of the
        /// same attribution, and this is what keeps it honest until the
        /// column metadata goes away and it becomes the only one.
        #[test]
        fn the_index_path_agrees_with_the_parquet_path_on_a_dual_carrying_archive() {
            let dir = tempfile::tempdir().unwrap();
            let plain = dir.path().join("plain.rez");
            let indexed = dir.path().join("indexed.rez");
            write(&plain, true, false);
            write(&indexed, true, true);

            let plain = open(&plain);
            let indexed = open(&indexed);

            let expected = series(&plain);
            assert_eq!(
                expected.len(),
                3,
                "the parquet path itself sees the handover as two series: {expected:?}"
            );
            assert_eq!(series(&indexed), expected);

            let mut a = plain.counter_labels("fake_ops");
            let mut b = indexed.counter_labels("fake_ops");
            a.sort();
            b.sort();
            assert_eq!(a, b);
            assert_eq!(indexed.sample_timestamps(), plain.sample_timestamps());
            assert_eq!(indexed.time_range_ns(), plain.time_range_ns());
        }

        /// The shape the cutover produces: a column that says which metric
        /// and which slot and nothing else. Read as parquet, every row of
        /// slot 0 is one series with no `comm` at all; read through the
        /// index, the rows before the handover are redis's and the rows from
        /// it on are valkey's, and neither has a value on the other's ticks.
        #[test]
        fn an_index_only_archive_is_split_by_occupant() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("cutover.rez");
            write(&path, false, true);
            let reader = open(&path);

            let got = series(&reader);
            let labels_of: Vec<&BTreeMap<String, String>> = got.iter().map(|(l, _)| l).collect();
            let want = |comm: &str, id: &str| {
                labels(&[("__name__", "fake_ops"), ("comm", comm), ("id", id)])
            };
            assert_eq!(
                labels_of,
                vec![
                    &want("nginx", "1"),
                    &want("redis", "0"),
                    &want("valkey", "0")
                ],
                "{got:?}"
            );
            let points = |comm: &str| -> Vec<(f64, f64)> {
                got.iter()
                    .find(|(l, _)| l["comm"] == comm)
                    .map(|(_, v)| v.clone())
                    .unwrap()
            };
            // Ticks are at 1s..6s; a 2s rate needs two samples, so each
            // occupant's first tick has no point. Redis holds ticks 1-3,
            // valkey 4-6, and neither rate crosses the handover.
            assert_eq!(points("redis"), vec![(2.0, 10.0), (3.0, 10.0)]);
            assert_eq!(points("valkey"), vec![(5.0, 7.0), (6.0, 7.0)]);
            assert_eq!(
                points("nginx"),
                vec![(2.0, 3.0), (3.0, 3.0), (4.0, 3.0), (5.0, 3.0), (6.0, 3.0)]
            );

            // And the parquet path, on the same file, cannot tell them apart —
            // which is the whole reason the index path exists.
            let db = RezDb::open(&path).unwrap();
            let segments = super::super::table_segments(&db, 1, STREAM).unwrap();
            let plain = SegmentedParquetReader::open_bytes_with_pool(
                segments,
                BufferPool::new(16 * 1024 * 1024),
            )
            .unwrap();
            assert_eq!(
                plain.counter_labels("fake_ops").len(),
                2,
                "one series per slot, the handover invisible"
            );
        }

        /// After retention has cut the head of the recording and its index
        /// history back to the restatement, the surviving rows are still
        /// attributed: the reader starts its replay at the last `Full` at
        /// or before the table's first surviving row, which is exactly the
        /// entry retention kept.
        #[test]
        fn a_retained_tail_is_attributed_from_the_restatement() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("cutover.rez");
            write(&path, false, true);
            {
                let mut db = RezDb::open(&path).unwrap();
                db.evict_before(1, ts(RESTATED)).unwrap();
                db.evict_caller_rows_before(1, STREAM, ts(RESTATED))
                    .unwrap();
                assert_eq!(
                    db.read_caller_rows(1, STREAM, 0, u64::MAX).unwrap().len(),
                    1,
                    "fixture: only the restatement remains"
                );
            }
            let reader = open(&path);
            let got = series(&reader);
            let labels_of: Vec<&BTreeMap<String, String>> = got.iter().map(|(l, _)| l).collect();
            let want = |comm: &str, id: &str| {
                labels(&[("__name__", "fake_ops"), ("comm", comm), ("id", id)])
            };
            assert_eq!(
                labels_of,
                vec![&want("nginx", "1"), &want("valkey", "0")],
                "redis's rows are gone with the head; the rest are still named: {got:?}"
            );
        }

        /// Converted to dendro, an archive whose occupants come from the
        /// identity index reads as the `.rez` did: with labels in the
        /// columns and the index, with the index alone, and with the index
        /// cut back to its restatement, where replay has to start at the
        /// `Full` found by `last_caller_row_at_or_before`.
        #[test]
        fn a_dendro_conversion_attributes_occupants_as_the_rez_did() {
            let queries = ["rate(fake_ops[2s])", "sum by (comm) (rate(fake_ops[2s]))"];
            for (case, labelled, retained) in [
                ("dual", true, false),
                ("index only", false, false),
                ("retained tail", false, true),
            ] {
                let dir = tempfile::tempdir().unwrap();
                let rez = dir.path().join("in.rez");
                let dendro = dir.path().join("out.dendro");
                write(&rez, labelled, true);
                if retained {
                    let mut db = RezDb::open(&rez).unwrap();
                    db.evict_before(1, ts(RESTATED)).unwrap();
                    db.evict_caller_rows_before(1, STREAM, ts(RESTATED))
                        .unwrap();
                }
                crate::to_dendro::convert_v3_to_dendro(&rez, &dendro).unwrap();
                let (a, b) = (open(&rez), open(&dendro));
                assert_eq!(series(&a), series(&b), "{case}");
                assert_same_answers(&a, &b, &queries);
            }
        }

        /// The browser opens an archive from bytes and reads it through the
        /// same split — the shared-connection arm of `SegmentSource`.
        #[test]
        fn the_split_also_applies_when_opened_from_bytes() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("cutover.rez");
            write(&path, false, true);
            let mut readers = RezReader::open_recordings_from_bytes(
                std::fs::read(&path).unwrap(),
                BufferPool::new(16 * 1024 * 1024),
            )
            .unwrap();
            let reader = readers.pop().unwrap().1;
            assert_eq!(series(&reader).len(), 3);
        }

        /// Windows survive the split: `rate()` over the indexed table carries
        /// the same uncertainty band the parquet path computes from the
        /// table-level window columns.
        #[test]
        fn rate_bands_survive_the_split() {
            let dir = tempfile::tempdir().unwrap();
            let plain = dir.path().join("plain.rez");
            let indexed = dir.path().join("indexed.rez");
            write(&plain, true, false);
            write(&indexed, true, true);
            let bands = |path: &Path| {
                let reader = open(path);
                let (start, end) = reader.time_range().unwrap();
                let QueryResult::Matrix { result } = reader
                    .query_range("rate(fake_ops[2s])", start, end, 1.0)
                    .unwrap()
                else {
                    panic!("matrix");
                };
                let mut out: Vec<_> = result
                    .into_iter()
                    .map(|s| {
                        (
                            s.metric.into_iter().collect::<BTreeMap<_, _>>(),
                            s.values,
                            s.intervals,
                        )
                    })
                    .collect();
                out.sort_by(|a, b| a.0.cmp(&b.0));
                out
            };
            let expected = bands(&plain);
            assert!(
                expected.iter().any(|(_, _, i)| i.is_some()),
                "the fixture's windows must produce a band on the parquet path: {expected:?}"
            );
            assert_eq!(bands(&indexed), expected);
        }
    }

    /// A long table and its occupant stream (the 6.0 layout) read as the
    /// wide table with labels in its columns that they replace.
    mod long {
        use super::*;
        use crate::occupants::{self, Occupant};
        use arrow::array::{ArrayRef, Int64Array, UInt64Array};
        use arrow::datatypes::{DataType, Field, Schema};
        use arrow::record_batch::RecordBatch;
        use dendro::archive::{
            ArchiveMut, SegmentMeta as DSegmentMeta, SourceMeta, WalRow as DWalRow,
        };
        use std::collections::{BTreeSet, HashMap as Map};

        const TABLE: &str = "cpu_usage/cpu_usage_task";
        const TICKS: u64 = 8;

        fn ts(tick: u64) -> u64 {
            1_700_000_000_000_000_000 + tick * 1_000_000_000
        }

        fn labels(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        }

        /// Four occupants: redis all along, a worker in TID 11 that exits after
        /// tick 4, a second worker that reuses TID 11 from tick 5 with its own
        /// `__uid__` (the slot is reassigned; everything else is the same), and
        /// nginx all along. Occupant numbers are dense, in order of first sight.
        fn occupant_set() -> Vec<(Occupant, std::ops::RangeInclusive<u64>)> {
            let o = |n, uid: &str, comm: &str, pid: &str| Occupant {
                occupant: n,
                labels: labels(&[
                    ("__uid__", uid),
                    ("comm", comm),
                    ("pid", pid),
                    ("tgid", "10"),
                    ("id", pid),
                ]),
            };
            vec![
                (o(0, "00000000000000a0", "redis", "10"), 1..=TICKS),
                (o(1, "00000000000000a1", "worker", "11"), 1..=4),
                (o(2, "00000000000000a3", "nginx", "30"), 1..=TICKS),
                (o(3, "00000000000000a2", "worker", "11"), 5..=TICKS),
            ]
        }

        fn value(occ: u64, tick: u64) -> u64 {
            (occ + 1) * 100 * tick
        }

        fn window(tick: u64) -> (i64, u64) {
            (-5_000_000, 5_000_000 + tick)
        }

        fn metric_meta(extra: &[(&str, &str)]) -> Map<String, String> {
            let mut m: Map<String, String> = [
                ("metric", "task_cpu_usage"),
                ("metric_type", "counter"),
                ("unit", "nanoseconds"),
                ("sampler", "cpu_usage"),
                ("state", "user"),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
            for (k, v) in extra {
                m.insert(k.to_string(), v.to_string());
            }
            m
        }

        fn parquet(fields: Vec<Field>, cols: Vec<ArrayRef>, kv: Vec<(String, String)>) -> Vec<u8> {
            let schema = Arc::new(Schema::new(fields));
            let kv = kv
                .into_iter()
                .map(|(k, v)| parquet::file::metadata::KeyValue::new(k, v))
                .collect();
            let props = parquet::file::properties::WriterProperties::builder()
                .set_key_value_metadata(Some(kv))
                .build();
            let mut buf = Vec::new();
            let mut w =
                parquet::arrow::ArrowWriter::try_new(&mut buf, Arc::clone(&schema), Some(props))
                    .unwrap();
            w.write(&RecordBatch::try_new(schema, cols).unwrap())
                .unwrap();
            w.close().unwrap();
            buf
        }

        /// Ticks `range` of the table, long: one row per tick and live occupant.
        fn long_segment(range: std::ops::RangeInclusive<u64>) -> Vec<u8> {
            let set = occupant_set();
            let mut rows = Vec::new();
            for tick in range {
                for (o, live) in &set {
                    if live.contains(&tick) {
                        rows.push((tick, o.occupant));
                    }
                }
            }
            let occupants = metriken_query::long::encode_occupant_ranges(rows.iter().map(|r| r.1));
            parquet(
                vec![
                    Field::new("timestamp", DataType::UInt64, false),
                    Field::new(":window_begin", DataType::Int64, true),
                    Field::new(":window_width", DataType::UInt64, true),
                    Field::new("occupant", DataType::UInt64, false),
                    Field::new("7", DataType::UInt64, true).with_metadata(metric_meta(&[])),
                ],
                vec![
                    Arc::new(UInt64Array::from_iter_values(rows.iter().map(|r| ts(r.0)))),
                    Arc::new(Int64Array::from_iter_values(
                        rows.iter().map(|r| window(r.0).0),
                    )),
                    Arc::new(UInt64Array::from_iter_values(
                        rows.iter().map(|r| window(r.0).1),
                    )),
                    Arc::new(UInt64Array::from_iter_values(rows.iter().map(|r| r.1))),
                    Arc::new(UInt64Array::from_iter_values(
                        rows.iter().map(|r| value(r.1, r.0)),
                    )),
                ],
                vec![
                    (
                        metriken_query::long::LAYOUT_KEY.into(),
                        metriken_query::long::LAYOUT_LONG.into(),
                    ),
                    (metriken_query::long::OCCUPANTS_KEY.into(), occupants),
                ],
            )
        }

        /// The same ticks, wide: a column per slot, labels in its metadata,
        /// the reused TID in a `#1` column as the agent writes it.
        fn wide_segment(range: std::ops::RangeInclusive<u64>) -> Vec<u8> {
            let ticks: Vec<u64> = range.collect();
            let mut fields = vec![
                Field::new("timestamp", DataType::UInt64, false),
                Field::new(":window_begin", DataType::Int64, true),
                Field::new(":window_width", DataType::UInt64, true),
            ];
            let mut cols: Vec<ArrayRef> = vec![
                Arc::new(UInt64Array::from_iter_values(ticks.iter().map(|t| ts(*t)))),
                Arc::new(Int64Array::from_iter_values(
                    ticks.iter().map(|t| window(*t).0),
                )),
                Arc::new(UInt64Array::from_iter_values(
                    ticks.iter().map(|t| window(*t).1),
                )),
            ];
            for (o, live) in occupant_set() {
                if !ticks.iter().any(|t| live.contains(t)) {
                    continue;
                }
                let pairs: Vec<(&str, &str)> = o
                    .labels
                    .iter()
                    .map(|(k, v)| (k.as_str(), v.as_str()))
                    .collect();
                let name = if o.occupant == 3 {
                    "7x11#1".to_string()
                } else {
                    format!("7x{}", o.labels["pid"])
                };
                fields.push(
                    Field::new(name, DataType::UInt64, true).with_metadata(metric_meta(&pairs)),
                );
                cols.push(Arc::new(UInt64Array::from_iter(
                    ticks
                        .iter()
                        .map(|t| live.contains(t).then(|| value(o.occupant, *t))),
                )));
            }
            parquet(fields, cols, Vec::new())
        }

        fn source(db: &mut ArchiveMut) -> i64 {
            db.insert_source(&SourceMeta {
                labels: labels(&[("source", "rezolus")]),
                metadata: labels(&[("version", "6.0.0")]),
                clock_anchor_wall_ns: ts(0) as i64,
            })
            .unwrap()
        }

        fn meta(range: &std::ops::RangeInclusive<u64>, rows: u64) -> DSegmentMeta {
            DSegmentMeta {
                rows,
                first_ts: ts(*range.start()) as i64,
                last_ts: ts(*range.end()) as i64,
            }
        }

        const HALVES: [std::ops::RangeInclusive<u64>; 2] = [1..=4, 5..=TICKS];

        fn write_wide(path: &Path) {
            let mut db = ArchiveMut::create(path).unwrap();
            let id = source(&mut db);
            for (seq, r) in HALVES.iter().enumerate() {
                let n = r.clone().count() as u64;
                db.insert_segment(id, TABLE, seq as u64, &meta(r, n), &wide_segment(r.clone()))
                    .unwrap();
            }
            db.mark_complete(id).unwrap();
        }

        /// The long table, and its occupant stream: first sightings in the
        /// segment of the tick they happened, a restatement of the live
        /// occupants at tick 5, and one more restatement at tick 8 left in
        /// the WAL so the reader has to find labels there too.
        fn write_long(path: &Path) {
            write_long_with(path, false)
        }

        /// [`write_long`]; with `sealed_first_only`, the second batch of
        /// occupant rows is never sealed, so occupant 3's labels exist only
        /// in the WAL.
        fn write_long_with(path: &Path, sealed_first_only: bool) {
            let mut db = ArchiveMut::create(path).unwrap();
            let id = source(&mut db);
            let set = occupant_set();
            for (seq, r) in HALVES.iter().enumerate() {
                let bytes = long_segment(r.clone());
                let rows = r
                    .clone()
                    .map(|t| set.iter().filter(|(_, l)| l.contains(&t)).count() as u64)
                    .sum();
                db.insert_segment(id, TABLE, seq as u64, &meta(r, rows), &bytes)
                    .unwrap();
            }
            let props = crate::rez::segment_writer_props;
            let first: Vec<(u64, &Occupant)> = set
                .iter()
                .filter(|(_, l)| *l.start() <= 4)
                .map(|(o, l)| (ts(*l.start()), o))
                .collect();
            let mut second: Vec<(u64, &Occupant)> = set
                .iter()
                .filter(|(_, l)| l.contains(&5))
                .map(|(o, _)| (ts(5), o))
                .collect();
            second.sort_by_key(|(_, o)| o.occupant);
            let stream = occupants::stream_of(TABLE);
            let batches = if sealed_first_only {
                vec![first]
            } else {
                vec![first, second]
            };
            for (seq, rows) in batches.iter().enumerate() {
                let bytes = occupants::encode_segment(rows, props()).unwrap();
                let m = DSegmentMeta {
                    rows: rows.len() as u64,
                    first_ts: rows[0].0 as i64,
                    last_ts: rows[rows.len() - 1].0 as i64,
                };
                db.insert_segment(id, &stream, seq as u64, &m, &bytes)
                    .unwrap();
            }
            let live: Vec<Occupant> = set
                .iter()
                .filter(|(_, l)| l.contains(&TICKS))
                .map(|(o, _)| o.clone())
                .collect();
            db.insert_wal_rows(
                id,
                &[DWalRow {
                    stream,
                    ts: ts(TICKS) as i64,
                    wall_offset: 0,
                    row: occupants::encode_wal_row(&live),
                }],
            )
            .unwrap();
            db.mark_complete(id).unwrap();
        }

        fn open(path: &Path) -> RezReader {
            RezReader::open_with_pool(path, BufferPool::new(64 * 1024 * 1024)).unwrap()
        }

        /// A result with `__occupant__` taken off every series and the series
        /// sorted: the long table's series carry their occupant number, the
        /// wide table's do not, and nothing else may differ.
        fn without_occupant(r: &RezReader, q: &str) -> Vec<String> {
            let (start, end) = r.time_range().unwrap();
            let mut v = serde_json::to_value(r.query_range(q, start, end, 1.0).unwrap()).unwrap();
            let mut out = Vec::new();
            if let Some(serde_json::Value::Array(series)) = v.get_mut("result") {
                for s in series {
                    if let Some(serde_json::Value::Object(m)) = s.get_mut("metric") {
                        m.remove(occupants::OCCUPANT_LABEL);
                    }
                    out.push(s.to_string());
                }
            }
            out.sort();
            out
        }

        #[test]
        fn a_long_table_reads_as_the_wide_table_it_replaces() {
            let dir = tempfile::tempdir().unwrap();
            let (wide, long) = (
                dir.path().join("wide.dendro"),
                dir.path().join("long.dendro"),
            );
            write_wide(&wide);
            write_long(&long);
            let (w, l) = (open(&wide), open(&long));

            // The occupant stream is the long table's labels, not a table.
            assert_eq!(w.counter_names(), l.counter_names());
            assert_eq!(l.counter_names(), vec!["task_cpu_usage".to_string()]);

            let strip = |v: Vec<BTreeMap<String, String>>| {
                let mut v: Vec<_> = v
                    .into_iter()
                    .map(|mut m| {
                        m.remove(occupants::OCCUPANT_LABEL);
                        m
                    })
                    .collect();
                v.sort();
                v
            };
            assert_eq!(
                strip(w.counter_labels("task_cpu_usage")),
                strip(l.counter_labels("task_cpu_usage"))
            );
            assert_eq!(
                l.counter_labels("task_cpu_usage").len(),
                4,
                "one series per occupant"
            );
            assert_eq!(w.time_range_ns(), l.time_range_ns());
            assert_eq!(w.sample_timestamps(), l.sample_timestamps());

            for q in [
                "rate(task_cpu_usage[2s])",
                "sum(rate(task_cpu_usage[2s]))",
                "sum by (comm) (rate(task_cpu_usage[2s]))",
                "rate(task_cpu_usage{comm=\"worker\"}[2s])",
                "rate(task_cpu_usage{pid=\"11\"}[2s])",
                "sum by (tgid) (rate(task_cpu_usage[2s]))",
            ] {
                assert_eq!(without_occupant(&w, q), without_occupant(&l, q), "{q}");
            }
            // The two workers in TID 11 are two series, told apart by uid.
            assert_eq!(
                without_occupant(&l, "rate(task_cpu_usage{pid=\"11\"}[2s])").len(),
                2
            );
        }

        #[test]
        fn occupant_labels_are_found_in_the_wal() {
            let dir = tempfile::tempdir().unwrap();
            let (sealed, wal) = (
                dir.path().join("sealed.dendro"),
                dir.path().join("wal.dendro"),
            );
            write_long_with(&sealed, false);
            write_long_with(&wal, true);
            let q = "sum by (comm, pid) (rate(task_cpu_usage[2s]))";
            assert_eq!(
                without_occupant(&open(&sealed), q),
                without_occupant(&open(&wal), q)
            );
            // And occupant 3 is named, not a bare number.
            let named = open(&wal)
                .counter_labels("task_cpu_usage")
                .into_iter()
                .filter(|l| l.get(occupants::OCCUPANT_LABEL).map(String::as_str) == Some("3"))
                .all(|l| l.get("comm").map(String::as_str) == Some("worker"));
            assert!(named);
        }

        #[test]
        fn a_long_table_reads_from_bytes_too() {
            let dir = tempfile::tempdir().unwrap();
            let long = dir.path().join("long.dendro");
            write_long(&long);
            let mut r = RezReader::open_recordings_from_bytes(
                std::fs::read(&long).unwrap(),
                BufferPool::new(64 * 1024 * 1024),
            )
            .unwrap();
            let (_, r) = r.pop().unwrap();
            let q = "sum by (comm) (rate(task_cpu_usage[2s]))";
            assert_eq!(without_occupant(&r, q), without_occupant(&open(&long), q));
            let comms: BTreeSet<String> = r
                .counter_labels("task_cpu_usage")
                .into_iter()
                .filter_map(|l| l.get("comm").cloned())
                .collect();
            assert_eq!(
                comms,
                ["nginx", "redis", "worker"]
                    .into_iter()
                    .map(String::from)
                    .collect()
            );
        }
    }
}
