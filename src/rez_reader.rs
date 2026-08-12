//! `RezReader`: reads a `.rez` archive as a unified `metriken_query::MetricsSource`
//! by composing one sub-source per per-sampler table — a `ParquetReader` for a
//! single-segment table, a `SegmentedParquetReader` for one the streaming
//! writer sealed more than once.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use metriken_query::{
    BufferPool, MetricsSource, ParquetReader, QueryError, QueryOptions, QueryResult,
    SegmentedParquetReader,
};

use crate::recorder::rez::{self, RecordingBytes};

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
        let (_manifest, recordings) = rez::read_archive_bytes(path)?;
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
