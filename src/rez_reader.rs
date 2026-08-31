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
//!
//! **Same-timeline union (Stage 4 Part C).** A V3-native archive tables its
//! acquisition GROUPS, not its samplers — `cpu_usage/percpu` and
//! `cpu_usage/softirq` are two physical tables of one sampler, with disjoint
//! metric sets. `route()` answers a query naming metrics from only one
//! physical table straight from that table's own (lazy, footer-only)
//! reader, exactly as it always has. A query naming metrics from more than
//! one table of the SAME sampler OF THE SAME RECORDING is answered by
//! composing those tables' already-open readers into one
//! [`metriken_query::UnionMetricsSource`] (built via its checking
//! `try_new`, not `new` — the composition set here is derived from archive
//! bytes, not hand-picked by trusted code, so a producer/archive bug that
//! put the same metric name in two "disjoint" tables must be a loud error,
//! not `UnionSource`'s silent first-wins) — a dispatch-by-metric-name union
//! with no timestamp join and no window reconstruction: each metric keeps
//! resolving its acquisition window from its own physical table, exactly as
//! it did before union, and the PromQL engine already aligns two
//! independently-timestamped series onto its evaluation grid whenever a
//! query combines them (that's how `a / b` has always worked, even within
//! one table). Building the union is cheap — it only touches each table's
//! already-open, footer-level name catalog, no row-group decode — so
//! `route()` builds one fresh per qualifying query rather than caching. A
//! query naming metrics from two DIFFERENT samplers still refuses,
//! unchanged — and so does one spanning the SAME sampler across two
//! DIFFERENT recordings of a multi-recording `.rez` (an A/B archive): every
//! recording's tables are flattened into one `tables` vec (see
//! `from_recordings`), so a metric present in both recordings' `cpu_usage`
//! table would otherwise look, from `owners()`'s point of view, exactly
//! like two group tables of one recording — and unioning them would let
//! `UnionSource`'s first-wins silently answer from ONE recording instead of
//! refusing. `route()` therefore groups by `(recording, sampler)`, not
//! `sampler` alone: only tables from the SAME recording ever union.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use metriken_query::{
    BufferPool, MetricsSource, ParquetReader, QueryError, QueryOptions, QueryResult, RateMode,
    SegmentedParquetReader, UnionChild, UnionError, UnionMetricsSource,
};

use crate::recorder::rez::{self, RecordingBytes};
use crate::recorder::rez_sqlite::RezDb;
use crate::recorder::rez_v3_writer::materialize_wal_tail;

/// The two concrete reader shapes a `.rez` table opens as, kept concrete
/// (not type-erased behind `Box<dyn MetricsSource>`) so a same-timeline
/// union can borrow each table's raw `DataSource` handle via
/// `UnionChild::from(&ParquetReader)`/`from(&SegmentedParquetReader)` —
/// that composition needs the concrete type, not the trait object.
enum TableReader {
    Single(ParquetReader),
    Segmented(SegmentedParquetReader),
}

/// One table awaiting its footer probe: its key, the segment bytes to parse,
/// and the span the catalog already answered.
type PendingProbe = (String, Vec<u8>, Option<(u64, u64)>);

/// What a probe yields: the table's key, its metric names, its row spacing and
/// its span.
type ProbedTable = (String, TableNames, f64, Option<(u64, u64)>);

/// Where a table's segment payloads come from.
///
/// v2 (tar) has no index — the whole archive is already in memory by the time
/// the reader sees it, so its bytes are handed over directly. v3 (SQLite) is
/// indexed by `(recording_id, sampler, seq)`, so a table's payload can be
/// fetched when it is first queried and never before. That is the difference
/// worth having: at open the reader needs one segment per table for its name
/// catalog, and the catalog answers everything else.
enum SegmentSource {
    Bytes(Vec<Vec<u8>>),
    Db {
        path: std::path::PathBuf,
        recording_id: i64,
        sampler: String,
    },
}

impl SegmentSource {
    /// Every segment of this table, materialized. Called only when the table
    /// is actually queried.
    fn all(&self) -> Result<Vec<Vec<u8>>, Box<dyn std::error::Error>> {
        match self {
            SegmentSource::Bytes(b) => Ok(b.clone()),
            SegmentSource::Db {
                path,
                recording_id,
                sampler,
            } => {
                let db = RezDb::open(path)?;
                table_segments(&db, *recording_id, sampler)
            }
        }
    }
}

/// A table's metric names by kind, probed from one segment's footer.
///
/// Kept split rather than merged because `counter_names`/`gauge_names`/
/// `histogram_names` are distinct questions on `MetricsSource` — a merged set
/// would answer all three with the same list, which is wrong and would not
/// have failed loudly.
#[derive(Default)]
struct TableNames {
    counters: std::collections::HashSet<String>,
    gauges: std::collections::HashSet<String>,
    histograms: std::collections::HashSet<String>,
}

impl TableNames {
    /// Whether this table holds `metric` under any kind — the routing question.
    fn holds(&self, metric: &str) -> bool {
        self.counters.contains(metric)
            || self.gauges.contains(metric)
            || self.histograms.contains(metric)
    }
}

impl TableReader {
    fn as_dyn(&self) -> &dyn MetricsSource {
        match self {
            TableReader::Single(r) => r,
            TableReader::Segmented(r) => r,
        }
    }

    fn union_child(&self) -> UnionChild {
        match self {
            TableReader::Single(r) => UnionChild::from(r),
            TableReader::Segmented(r) => UnionChild::from(r),
        }
    }
}

/// One opened per-table reader. A table is one or more parquet segments, so
/// the backing source is either a plain `ParquetReader` (single segment) or a
/// `SegmentedParquetReader` (many) — both are `MetricsSource`, and everything
/// below this point treats them identically.
///
/// `sampler` is the table's own key — `<sampler>/<group>` for a V3
/// acquisition-group table, just `<sampler>` for a V2 (or V3 windowless)
/// table. `rez::table_sampler` recovers the manifest-level sampler name from
/// it; this field is never split eagerly because most tables (every V2
/// table, and any V3 sampler with only one group) don't need it to be.
///
/// `recording` is the index (within `from_recordings`'s input) of the
/// recording this table belongs to — carried so `route()` can tell "two
/// group tables of one sampler in one recording" (union) apart from "the
/// same sampler's table in two DIFFERENT recordings" (still a refusal; see
/// the module docs). An index rather than the recording's `dir` string:
/// `dir` is a display name derived from labels (`recording_dir_slug`), not a
/// guaranteed-unique identity — two recordings with the same labels are
/// entirely legal and would collide on `dir`.
struct SamplerReader {
    recording: usize,
    sampler: String,
    /// Every metric name this table holds, probed from ONE segment's footer at
    /// open. A table's segments share a schema — `schema_hash` is what asserts
    /// it — so one probe answers routing for all of them.
    ///
    /// This exists so `owners` can decide whether a query could touch this
    /// table WITHOUT building its reader. Routing used to ask each table's open
    /// reader for its columns, which meant every table in the archive was
    /// opened before any query was parsed: measured at ~1.37 ms per segment
    /// over 418 segments, of which a typical query needs 11%.
    names: TableNames,
    /// The table's row-time span, probed from its FIRST and LAST segment's
    /// footers at open.
    ///
    /// `time_range` is asked on paths that never query — `mcp query` calls it
    /// before evaluating anything — and answering it from the full readers
    /// forced every table open, which defeated the whole point of the lazy
    /// build. Two footers per table answers it instead of all of them.
    span: Option<(u64, u64)>,
    /// The table's row spacing, probed from the same first segment.
    ///
    /// A table is one sampler's cadence, so any of its segments answers for all
    /// of them. Like `span`, this is asked on paths that never query
    /// (`describe-metrics`, `analyze-correlation`) and would otherwise open
    /// every table to find out.
    interval: f64,
    /// Where the full segment set comes from, resolved on first use.
    segments: SegmentSource,
    pool: Arc<BufferPool>,
    /// Built on first access, never at open.
    reader: std::sync::OnceLock<Option<TableReader>>,
    /// Row timestamps as the query path indexes them, read once.
    ///
    /// Reading them means decoding a whole column, and the cross-cadence
    /// policy asks for them on every query that spans samplers. A reader's
    /// view is fixed once it is open — a live archive is re-opened, and the
    /// WAL tail is materialized at open — so one read is enough, and the
    /// viewer holds its readers for the life of the process.
    snapped_timestamps: std::sync::OnceLock<Vec<u64>>,
}

impl SamplerReader {
    /// The table's reader, built on first use — `None` if its segments have
    /// gone since the probe.
    ///
    /// Single-segment tables keep the plain reader — the streaming writer's
    /// slow samplers and every atomically written archive land there, and there
    /// is nothing for the splice to do. Multi-segment tables get the
    /// segment-aware source, which splices raw samples below PromQL evaluation
    /// so a `rate()` window straddling a seal boundary still computes on
    /// complete data.
    ///
    /// **Fallible because a `.rez` is readable while it is written.** This used
    /// to `.expect("segments opened at probe time cannot fail to reopen")`,
    /// which holds for a finished archive and not for the live one the format
    /// advertises: `table_segments` returns the sealed segments plus the
    /// materialized WAL tail, and hindsight's retention deletes as it goes, so
    /// a quiet sampler's only rows can be evicted between the probe that named
    /// this table and the query that opens it. That left the viewer and MCP
    /// panicking on a rolling buffer — the one thing the buffer exists to be
    /// read as.
    ///
    /// A vanished table is reported as absent, which is what a table with no
    /// rows already is: `from_v3` skips it at open, and a query naming only
    /// metrics it held gets the ordinary "references no metric present in this
    /// .rez" error rather than a crash. `OnceLock<Option<_>>` so a table that
    /// has gone is not re-fetched on every subsequent query.
    fn reader(&self) -> Option<&TableReader> {
        self.reader
            .get_or_init(|| {
                let pool = Arc::clone(&self.pool);
                let segments = match self.segments.all() {
                    Ok(s) if !s.is_empty() => s,
                    // Empty is the eviction case; an error is a genuine read
                    // failure. Both mean this table cannot answer, and neither
                    // is worth taking the process down for.
                    Ok(_) => {
                        tracing::warn!(
                            "table {} had rows at open and none now; it was evicted or \
                             rotated while being read, and is reported as absent",
                            self.sampler
                        );
                        return None;
                    }
                    Err(e) => {
                        tracing::warn!("fetching segments for {}: {e}", self.sampler);
                        return None;
                    }
                };
                match <[Vec<u8>; 1]>::try_from(segments) {
                    Ok([bytes]) => ParquetReader::open_bytes_with_pool(bytes, pool)
                        .map(TableReader::Single)
                        .map_err(|e| format!("{e}")),
                    Err(segments) => SegmentedParquetReader::open_bytes_with_pool(segments, pool)
                        .map(TableReader::Segmented)
                        .map_err(|e| format!("{e}")),
                }
                .map_err(|e| {
                    tracing::warn!("reopening table {}: {e}", self.sampler);
                })
                .ok()
            })
            .as_ref()
    }

    fn snapped_timestamps(&self) -> &[u64] {
        self.snapped_timestamps.get_or_init(|| {
            self.reader()
                .map(|r| r.as_dyn().snapped_sample_timestamps())
                .unwrap_or_default()
        })
    }
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
    /// Whether this recording holds no tables at all.
    ///
    /// An arm that produced no rows — an endpoint that was reachable but never
    /// scraped successfully — still gets a manifest row, so "how many
    /// recordings" and "how many recordings carry data" are different
    /// questions. Consumers that can only read one recording care about the
    /// second one: an empty arm collides with nothing.
    pub fn is_empty(&self) -> bool {
        self.tables.is_empty()
    }

    /// Open a `.rez` at `path`, opening each per-sampler table against `pool`,
    /// flattening every recording into one view.
    ///
    /// **No production caller.** The viewer opens recordings individually, and
    /// so does `mcp` since flattening a multi-recording archive gives every
    /// sampler two owners and makes `route` refuse every query. Kept because
    /// the flattening behaviour — and the refusal it produces — is what the
    /// cross-recording regression tests pin; a future single-recording
    /// consumer can use it, but should prefer `open_recordings`.
    #[allow(dead_code)]
    pub fn open_with_pool(
        path: &Path,
        pool: Arc<BufferPool>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let filename = path.file_name().map(|s| s.to_string_lossy().into_owned());
        if rez::detect_rez_format(path)? == rez::RezFormat::V3Sqlite {
            // Flatten every recording's tables, as the tar path does — this
            // entry point is the single-source view.
            let mut tables = Vec::new();
            let mut metadata = BTreeMap::new();
            for (i, (_, reader)) in Self::from_v3(path, pool)?.into_iter().enumerate() {
                if i == 0 {
                    metadata = reader.metadata;
                }
                tables.extend(reader.tables);
            }
            return Ok(Self {
                tables,
                metadata,
                filename,
            });
        }
        let recordings = read_recordings(path)?;
        Self::from_recordings(recordings, filename, pool)
    }

    /// Open a `.rez` as one `RezReader` **per recording**, paired with that
    /// recording's labels. Used by the viewer to map a 2-recording `.rez` onto
    /// baseline/experiment without cross-recording sampler-name collisions.
    pub fn open_recordings(
        path: &Path,
        pool: Arc<BufferPool>,
    ) -> Result<LabeledRecordings, Box<dyn std::error::Error>> {
        if rez::detect_rez_format(path)? == rez::RezFormat::V3Sqlite {
            return Self::from_v3(path, pool);
        }
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

    /// Build from a v3 (SQLite) archive without materializing it.
    ///
    /// The catalog answers spans with no BLOB read (`segment_span`), and one
    /// segment per table answers the name catalog. Everything else waits until
    /// a query actually asks for that table. Contrast `from_recordings`, which
    /// the tar path still uses because tar has no index — its bytes are already
    /// in memory before the reader is called.
    fn from_v3(
        path: &Path,
        pool: Arc<BufferPool>,
    ) -> Result<LabeledRecordings, Box<dyn std::error::Error>> {
        let db = RezDb::open(path)?;
        let mut out = Vec::new();

        for (recording, rec) in db.read_recordings()?.into_iter().enumerate() {
            if !rec.complete {
                tracing::warn!(
                    "recording {} was not cleanly finalized; it was recovered up to its \
                     last checkpoint and data after that may be missing",
                    rez::recording_dir_slug(&rec.meta.labels)
                );
            }
            // Two phases on purpose. SQLite is serial (one connection, and
            // `rusqlite::Connection` is not `Sync`) but cheap; parsing a
            // segment footer is the expensive half and is independent per
            // table. Measured at ~0.45 ms per table before splitting them —
            // linear in TABLE COUNT, not archive bytes, so it is the cost that
            // grows as samplers and acquisition groups multiply.
            let mut pending: Vec<PendingProbe> = Vec::new();

            for sampler in db.all_samplers(rec.id)? {
                let metas = db.read_segment_meta(rec.id, &sampler)?;

                // The probe segment is the first SEALED one — or, when a table
                // has none, its materialized WAL tail. A quiet sampler in a
                // live hindsight buffer is exactly that: rows in the WAL, no
                // seal yet. Skipping it here would make it invisible to the
                // reader, which the eager path never did because
                // `table_segments` splices the tail in.
                let probe_bytes = match metas.first() {
                    Some((seq, _)) => db.read_segment_bytes(rec.id, &sampler, *seq)?,
                    None => materialize_wal_tail(&sampler, &db.live_wal(rec.id, &sampler)?)?
                        .map(|t| t.bytes),
                };
                // Nothing sealed and nothing live: the table has no rows at
                // all, so there is nothing to open. Same skip as the eager path.
                let Some(bytes) = probe_bytes else {
                    continue;
                };

                // Span from the catalog, widened by the live WAL: a hindsight
                // buffer's newest rows are unsealed, and a span that stopped at
                // the last seal would report the archive as ending before its
                // most recent data. No BLOB is read for either.
                let (_, sealed) = db.segment_span(rec.id, &sampler)?;
                let wal = db.live_wal_span(rec.id, &sampler)?;
                let span = match (
                    sealed.first_ts.into_iter().chain(wal.first_ts).min(),
                    sealed.last_ts.into_iter().chain(wal.last_ts).max(),
                ) {
                    (Some(b), Some(e)) => Some((b, e)),
                    _ => None,
                };

                pending.push((sampler, bytes, span));
            }

            // Phase two: parse the footers in parallel. Each probe is
            // independent and touches only the shared `BufferPool`, which is
            // `Mutex`-guarded.
            // Chunked, and each worker gets its OWN small pool.
            //
            // One thread per table spawned 50 threads that then serialized on
            // the shared pool's mutex — 0.45 ms/table became 0.25 ms, where the
            // core count says it should have collapsed. A probe reader is
            // thrown away the moment its names are read, so it has nothing to
            // gain from the shared pool and everything to lose by contending
            // for it. Chunking amortizes spawn cost over the same threads.
            let workers = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
                .min(pending.len().max(1));
            let chunk = pending.len().div_ceil(workers.max(1));
            let probed: Vec<Result<ProbedTable, String>> = std::thread::scope(|scope| {
                let handles: Vec<_> = pending
                    .chunks(chunk.max(1))
                    .map(|batch| {
                        scope.spawn(move || {
                            // 8 MiB is ample for footer-only reads and is
                            // never shared, so it cannot be contended.
                            let pool = BufferPool::new(8 * 1024 * 1024);
                            batch
                                .iter()
                                .map(|(sampler, bytes, span)| {
                                    let probe = ParquetReader::open_bytes_with_pool(
                                        bytes.clone(),
                                        Arc::clone(&pool),
                                    )
                                    .map_err(|e| format!("probing table {sampler}: {e}"))?;
                                    let names = TableNames {
                                        counters: probe.counter_names().into_iter().collect(),
                                        gauges: probe.gauge_names().into_iter().collect(),
                                        histograms: probe.histogram_names().into_iter().collect(),
                                    };
                                    Ok((sampler.clone(), names, probe.interval(), *span))
                                })
                                .collect::<Vec<_>>()
                        })
                    })
                    .collect();
                handles
                    .into_iter()
                    .flat_map(|h| {
                        h.join()
                            .unwrap_or_else(|_| vec![Err("probe panicked".to_string())])
                    })
                    .collect()
            });

            let mut tables = Vec::new();
            for probe in probed {
                let (sampler, names, interval, span) = probe?;
                tables.push(SamplerReader {
                    recording,
                    sampler: sampler.clone(),
                    names,
                    span,
                    interval,
                    segments: SegmentSource::Db {
                        path: path.to_path_buf(),
                        recording_id: rec.id,
                        sampler,
                    },
                    pool: Arc::clone(&pool),
                    reader: std::sync::OnceLock::new(),
                    snapped_timestamps: std::sync::OnceLock::new(),
                });
            }

            out.push((
                rec.meta.labels.clone(),
                Self {
                    tables,
                    metadata: rec.meta.metadata,
                    filename: Some(rez::recording_dir_slug(&rec.meta.labels)),
                },
            ));
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
        for (recording, rec) in recordings.into_iter().enumerate() {
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
                // Probe ONE segment for the name catalog. A plain
                // `ParquetReader` is footer-only and skips the four per-segment
                // identity indexes `SegmentedParquetReader` builds, which is
                // the bulk of what open used to cost.
                let probe = segments
                    .first()
                    .ok_or_else(|| format!("table {sampler} has no segments"))?;
                let probe = ParquetReader::open_bytes_with_pool(probe.clone(), Arc::clone(&pool))
                    .map_err(|e| format!("probing table {sampler}: {e}"))?;
                let names = TableNames {
                    counters: probe.counter_names().into_iter().collect(),
                    gauges: probe.gauge_names().into_iter().collect(),
                    histograms: probe.histogram_names().into_iter().collect(),
                };
                let first_span = probe.time_range_ns();
                let interval = probe.interval();
                drop(probe);

                // Segments are in segment order, so the last one carries the
                // table's end. Skipped when there is only one — it is the probe.
                let last_span = match segments.len() {
                    0 | 1 => first_span,
                    _ => {
                        let last = ParquetReader::open_bytes_with_pool(
                            segments[segments.len() - 1].clone(),
                            Arc::clone(&pool),
                        )
                        .map_err(|e| format!("probing table {sampler} tail: {e}"))?;
                        let s = last.time_range_ns();
                        drop(last);
                        s
                    }
                };
                let span = match (first_span, last_span) {
                    (Some((b, _)), Some((_, e))) => Some((b, e)),
                    (only, None) | (None, only) => only,
                };

                tables.push(SamplerReader {
                    recording,
                    sampler,
                    names,
                    span,
                    interval,
                    segments: SegmentSource::Bytes(segments),
                    pool: Arc::clone(&pool),
                    reader: std::sync::OnceLock::new(),
                    snapped_timestamps: std::sync::OnceLock::new(),
                });
            }
        }
        Ok(Self {
            tables,
            metadata,
            filename,
        })
    }

    /// Sub-readers that hold at least one metric the query references.
    ///
    /// Answered from each table's name catalog, so a table the query cannot
    /// touch is never opened. `referenced_metrics` is parse-only — it does not
    /// need a source, which is exactly why routing can use it and `columns`
    /// (which expands selectors through the source's column map) cannot.
    ///
    /// Label matchers are not consulted: a table holding the metric with no
    /// matching series answers with an empty result, which is correct, and
    /// skipping it here would route on data the catalog does not carry.
    fn owners(&self, query: &str) -> Result<Vec<&SamplerReader>, QueryError> {
        let referenced = metriken_query::referenced_metrics(query)?;
        Ok(self
            .tables
            .iter()
            .filter(|t| referenced.iter().any(|m| t.names.holds(m)))
            .collect())
    }

    /// Resolve the reader that answers every metric a query references: the
    /// single owning table directly, or — when every owner lives in ONE
    /// RECORDING — a fresh [`UnionMetricsSource`] over exactly those tables.
    ///
    /// Tables of different samplers union freely within a recording. They did
    /// not always: a query spanning two samplers used to be refused as
    /// "cross-timeline", because there was no way to say what treating two
    /// separately-read values as simultaneous costs. The query engine now
    /// prices that itself — operands whose acquisition edges differ have their
    /// bands widened to the union of both spans — so the refusal has nothing
    /// left to protect. Measured, the join costs 1.5–3.0 ms on a 32-core host,
    /// under 1% of a 200 ms interval.
    ///
    /// Still refused: the SAME sampler across two DIFFERENT recordings of a
    /// multi-recording (A/B) archive. Those are genuinely different timelines
    /// — different agents, hosts or arms — and unioning them would let
    /// first-wins silently answer from one recording. Errors too when the
    /// query references no known metric.
    fn route(&self, query: &str) -> Result<Routed<'_>, QueryError> {
        let owners = self.owners(query)?;
        match owners.as_slice() {
            [] => Err(QueryError::ParseError(format!(
                "query references no metric present in this .rez: {query}"
            ))),
            // A table whose segments have gone since the probe is absent, so
            // its metrics are too: the same error a query naming a metric this
            // archive never held gets.
            [one] => one.reader().map(Routed::Direct).ok_or_else(|| {
                QueryError::ParseError(format!(
                    "query references {query}, whose table ({}) has been evicted since \
                     this archive was opened",
                    one.sampler
                ))
            }),
            many => {
                // Group owners by (RECORDING, SAMPLER), not by sampler alone:
                // two group tables of one sampler are a same-timeline union
                // ONLY within one recording. `from_recordings` flattens every
                // recording's tables into one `tables` vec, so a metric
                // present in every recording of a multi-recording (A/B)
                // archive — e.g. `cpu_cycles` in each side's `cpu_usage`
                // table — would otherwise look exactly like two group tables
                // of one sampler, and unioning them would let
                // `UnionSource`'s first-wins silently answer from ONE
                // recording instead of refusing (see the module docs).
                // `rez::table_sampler` is the identity function for every V2
                // (or unsplit V3) table, so within one recording this
                // reduces to today's behavior whenever nothing actually
                // split.
                let mut groups: Vec<usize> = many.iter().map(|t| t.recording).collect();
                groups.sort();
                groups.dedup();
                match groups.as_slice() {
                    [_] => {
                        // Building the union only touches each table's
                        // already-open, footer-level name catalog (no
                        // row-group decode), so a fresh one per query is
                        // cheap enough not to need caching.
                        //
                        // `try_new`, not `new`: this composition set is
                        // derived from archive bytes (table schemas plus
                        // parsed table keys), not hand-picked by trusted
                        // code, so a producer/archive bug that put the same
                        // metric name in two "disjoint" group tables of this
                        // sampler must be a loud error, not `UnionSource`'s
                        // silent first-wins.
                        // `filter_map`, not `map`: on a live archive one of
                        // several owners can have been evicted between the
                        // probe and here, and the rest still answer.
                        let children: Vec<UnionChild> = many
                            .iter()
                            .filter_map(|t| t.reader().map(TableReader::union_child))
                            .collect();
                        if children.is_empty() {
                            return Err(QueryError::ParseError(format!(
                                "query references {query}, whose tables have all been \
                                 evicted since this archive was opened"
                            )));
                        }
                        UnionMetricsSource::try_new(children)
                            .map(Routed::Union)
                            .map_err(|e| match e {
                                UnionError::NonDisjoint { duplicates } => {
                                    // Two very different situations produce
                                    // this, and conflating them tells the
                                    // operator their archive is corrupt when
                                    // it is not. Distinct SAMPLERS sharing a
                                    // metric name is legitimate and shipped:
                                    // `gpu_amd_smi` and `gpu_nvidia` both
                                    // publish the vendor-neutral
                                    // `gpu_utilization`, `gpu_temperature` and
                                    // six more, because only one of them ever
                                    // populates on a given host. Two group
                                    // tables of ONE sampler sharing a name is
                                    // a real archive defect.
                                    let mut samplers: Vec<&str> = many
                                        .iter()
                                        .map(|t| rez::table_sampler(&t.sampler))
                                        .collect();
                                    samplers.sort();
                                    samplers.dedup();
                                    if samplers.len() > 1 {
                                        QueryError::ParseError(format!(
                                            "query {query} references metric name(s) published \
                                             by more than one sampler ({}), so it is ambiguous \
                                             which is meant: {}. This is not an archive fault — \
                                             those samplers deliberately share vendor-neutral \
                                             names. Query one of them at a time.",
                                            samplers.join(", "),
                                            duplicates.join(", ")
                                        ))
                                    } else {
                                        QueryError::ParseError(format!(
                                            "query {query} references metric name(s) present in \
                                             more than one acquisition-group table of the same \
                                             sampler — the archive's own tables are not \
                                             disjoint, which should never happen: {}",
                                            duplicates.join(", ")
                                        ))
                                    }
                                }
                                UnionError::Empty => {
                                    unreachable!("the `many` arm always has at least 2 owners")
                                }
                            })
                    }
                    _ => {
                        // Only one case reaches here now: metrics drawn from
                        // more than one RECORDING of a multi-recording
                        // archive. Those are different agents, hosts or arms
                        // on genuinely different timelines, and the widened
                        // band does not make them comparable — unioning them
                        // would let first-wins silently answer from one side.
                        let mut samplers: Vec<&str> = many
                            .iter()
                            .map(|t| rez::table_sampler(&t.sampler))
                            .collect();
                        samplers.sort();
                        samplers.dedup();
                        Err(QueryError::ParseError(format!(
                            "query {query} references metrics ({}) from {} different \
                             recordings of this multi-recording .rez — cross-recording \
                             queries are not supported; query one recording at a time \
                             (see `RezReader::open_recordings`)",
                            samplers.join(", "),
                            groups.len()
                        )))
                    }
                }
            }
        }
    }
}

/// What `route()` resolves a query to: either a borrowed reference straight
/// into one of `RezReader`'s own tables (the common, zero-allocation case),
/// or an owned same-timeline union built fresh for this one query.
enum Routed<'a> {
    Direct(&'a TableReader),
    Union(UnionMetricsSource),
}

impl Routed<'_> {
    fn as_dyn(&self) -> &dyn MetricsSource {
        match self {
            Routed::Direct(r) => r.as_dyn(),
            Routed::Union(u) => u,
        }
    }
}

/// The typical spacing between consecutive rows, or `None` for fewer than two
/// rows (no gap to measure).
///
/// The median, not the mean: a sampler's rows are irregular — 30 s then 60 s
/// apart on a real recording — and a mean is dragged around by the long gaps
/// and by any restart-sized hole in the middle of a recording. The median
/// answers "how often does this table usually produce a row", which is the
/// question being asked.
fn typical_gap_ns(timestamps: &[u64]) -> Option<u64> {
    if timestamps.len() < 2 {
        return None;
    }
    let mut gaps: Vec<u64> = timestamps
        .windows(2)
        .map(|w| w[1].saturating_sub(w[0]))
        .collect();
    gaps.sort_unstable();
    Some(gaps[gaps.len() / 2]).filter(|g| *g > 0)
}

impl RezReader {
    /// The timestamps a query should be evaluated at, when it spans samplers of
    /// different cadence — `None` when it does not and the uniform grid is
    /// right.
    ///
    /// The grid walks `start + k·step`. A query combining a fast sampler with a
    /// slow one therefore produces most of its points where the slow sampler
    /// has no reading at all: that value is held forward and combined with the fast
    /// operand as if the two were simultaneous.
    ///
    /// The grid cannot be tuned out of this. A slow sampler's rows are not
    /// evenly spaced — measured on a real recording, one sampler's readings
    /// fell 30 s apart and then 60 s apart — so no step and no phase puts a
    /// uniform grid on them. Two earlier attempts are worth recording:
    /// coarsening the STEP relocated the grid and made the combined band
    /// explode (0.85% wide before, 6.7x after), and widening only the averaging
    /// SPAN left the points on the grid, still between the slow sampler's real
    /// readings.
    ///
    /// So hand the engine the slow sampler's own row timestamps. Every point then
    /// lands where both operands genuinely have data, and each rate averages
    /// over the gap it actually spans.
    ///
    /// Returns `None` unless the query really touches more than one cadence, so
    /// single-sampler queries — the overwhelming majority — are untouched.
    ///
    /// Also `None` under [`RateMode::Raw`], which already answers this question
    /// its own way: Raw places points at the real, un-snapped sample
    /// timestamps. Relocating them would contradict that contract, and would
    /// break the query outright — Raw's counter producer reads sample pairs
    /// and ignores supplied points, while the gauge producers honour them, so
    /// a counter-and-gauge expression would have its two sides land on
    /// different instants and intersect nowhere.
    fn cross_cadence_eval_timestamps(
        &self,
        query: &str,
        step_s: f64,
        rate_mode: RateMode,
    ) -> Option<Arc<[u64]>> {
        use crate::recorder::rez::table_sampler;

        if matches!(rate_mode, RateMode::Raw) {
            return None;
        }

        let owners = self.owners(query).ok()?;
        if owners.len() < 2 {
            return None;
        }

        // Cadence comes from the ROWS, not from `interval()`: that reports the
        // recording's nominal interval, which every table in an archive shares
        // — on a real recording a 1 s sampler and a 30 s one both answered 1.0,
        // so asking it can never detect a cadence difference. And it must be
        // the SNAPPED rows, because those are the instants the query path
        // indexes samples by; a raw row at 1.5 s on a 1 s grid is indexed at
        // 2.0 s, so asking for 1.5 s falls before the series starts and
        // silently yields nothing.
        //
        // Cadence is a property of the SAMPLER, not of a table. Two group
        // tables of one sampler are read together on one schedule; a group that
        // dedups or skips ticks is sparse WITHIN that cadence, not a second
        // cadence, and relocating a query onto its rows would silently change
        // the answer for a query that merely named a sibling group's metric.
        //
        // So a sampler's cadence is the spacing of its DENSEST participating
        // table — the one that shows the underlying read schedule.
        let mut by_sampler: BTreeMap<&str, (u64, &[u64])> = BTreeMap::new();
        for t in &owners {
            let ts = t.snapped_timestamps();
            let Some(gap) = typical_gap_ns(ts) else {
                continue;
            };
            by_sampler
                .entry(table_sampler(&t.sampler))
                .and_modify(|slot| {
                    if gap < slot.0 {
                        *slot = (gap, ts);
                    }
                })
                .or_insert((gap, ts));
        }
        if by_sampler.len() < 2 {
            return None;
        }

        let fastest = by_sampler.values().map(|(gap, _)| *gap).min()?;
        let (slowest, timestamps) = by_sampler.into_values().max_by_key(|(gap, _)| *gap)?;
        // Deliberately a ratio, not equality: gaps measured from real rows are
        // never exactly equal, so "different cadence" has to mean *materially*
        // different. A sampler read at least twice as far apart as another is a
        // different cadence in any sense that matters here.
        if slowest < fastest.saturating_mul(2) {
            return None;
        }
        // A slow sampler finer than the step is already oversampled by the
        // grid; moving off it would only lose points.
        if (slowest as f64) <= step_s * 1e9 {
            return None;
        }
        Some(timestamps.into())
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
        let aligned;
        let opts = match self.cross_cadence_eval_timestamps(expr, step_s, opts.rate_mode) {
            Some(points) => {
                // Clone and set the one field: `QueryOptions` is
                // `#[non_exhaustive]`, so it cannot be built by literal from
                // here — and cloning preserves whatever else the caller set.
                aligned = opts.clone().with_eval_timestamps(Some(points));
                &aligned
            }
            None => opts,
        };
        self.route(expr)?
            .as_dyn()
            .query_range_opts(expr, start_s, end_s, step_s, opts)
    }
    fn query(&self, expr: &str, time: Option<f64>) -> Result<QueryResult, QueryError> {
        self.route(expr)?.as_dyn().query(expr, time)
    }
    fn columns(&self, query: &str) -> Result<HashSet<String>, QueryError> {
        // columns() is answerable as the union — it never crosses timelines.
        let mut out = HashSet::new();
        for t in self.owners(query)? {
            let Some(r) = t.reader() else {
                continue;
            };
            out.extend(r.as_dyn().columns(query)?);
        }
        Ok(out)
    }

    // ── Union metadata / naming / labels ──
    fn counter_names(&self) -> Vec<String> {
        union_sorted(
            self.tables
                .iter()
                .map(|t| t.names.counters.iter().cloned().collect()),
        )
    }
    fn gauge_names(&self) -> Vec<String> {
        union_sorted(
            self.tables
                .iter()
                .map(|t| t.names.gauges.iter().cloned().collect()),
        )
    }
    fn histogram_names(&self) -> Vec<String> {
        union_sorted(
            self.tables
                .iter()
                .map(|t| t.names.histograms.iter().cloned().collect()),
        )
    }
    fn counter_labels(&self, name: &str) -> Vec<BTreeMap<String, String>> {
        self.tables
            .iter()
            .filter(|t| t.names.counters.contains(name))
            .filter_map(|t| t.reader())
            .flat_map(|r| r.as_dyn().counter_labels(name))
            .collect()
    }
    fn gauge_labels(&self, name: &str) -> Vec<BTreeMap<String, String>> {
        self.tables
            .iter()
            .filter(|t| t.names.gauges.contains(name))
            .filter_map(|t| t.reader())
            .flat_map(|r| r.as_dyn().gauge_labels(name))
            .collect()
    }
    fn histogram_labels(&self, name: &str) -> Vec<BTreeMap<String, String>> {
        self.tables
            .iter()
            .filter(|t| t.names.histograms.contains(name))
            .filter_map(|t| t.reader())
            .flat_map(|r| r.as_dyn().histogram_labels(name))
            .collect()
    }

    // ── Time / interval: union extent, finest interval ──
    fn time_range(&self) -> Option<(f64, f64)> {
        // Seconds view of the same probed spans — see `time_range_ns`.
        self.time_range_ns()
            .map(|(b, e)| (b as f64 / 1e9, e as f64 / 1e9))
    }
    fn time_range_ns(&self) -> Option<(u64, u64)> {
        // From the probed spans, not the readers: this is asked before any
        // query runs, and answering it through `reader()` would open every
        // table and undo the lazy build.
        self.tables
            .iter()
            .filter_map(|t| t.span)
            .reduce(|(a0, a1), (b0, b1)| (a0.min(b0), a1.max(b1)))
    }
    fn interval(&self) -> f64 {
        // Probed per table, not read through `reader()` — see `span`. The
        // finest cadence still wins; only where the number comes from changed.
        let finest = self
            .tables
            .iter()
            .map(|t| t.interval)
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
        segments.push(tail.bytes);
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
        use crate::recorder::rez_stream::{ManifestSeed, RezWriterHandle, StreamRecorder};
        use crate::recorder::seal_policy::SealPolicy;

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
        use crate::recorder::rez_stream::{ManifestSeed, RezWriterHandle, StreamRecorder};
        use crate::recorder::seal_policy::SealPolicy;
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
        use crate::recorder::rez_sqlite::WalRow;
        use crate::recorder::rez_v3_writer::{
            encode_wal_row, ManifestSeed, RezArchive, StreamRecorderV3, WalCell, WalValue,
        };
        use crate::recorder::seal_policy::SealPolicy;
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
                        window: w,
                        counters: vec![Some(i * 1_000)],
                        gauges: Vec::new(),
                        histograms: Vec::new(),
                    };
                    let softirq = GroupSnapshot {
                        name: "cpu_usage/softirq".to_string(),
                        schema_hash: softirq_schema.hash(),
                        schema: Some(std::sync::Arc::clone(&softirq_schema)),
                        window: w,
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
        #[test]
        fn cross_group_query_spans_counter_gauge_and_histogram() {
            let percpu_schema = std::sync::Arc::new(group_schema(&["cpu_cycles"], "cpu_usage"));
            let freq_schema = std::sync::Arc::new(gauge_group_schema(&["frequency"], "cpu_usage"));
            let sched_schema =
                std::sync::Arc::new(histogram_group_schema(&["latency"], "cpu_usage"));
            let n = 6u64;
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("mixed_kinds.rez");
            let (_archive, mut rec) = recorder(&path, 2); // force multiple segments per table
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
                    window: w,
                    counters: vec![Some(i * 1_000)],
                    gauges: Vec::new(),
                    histograms: Vec::new(),
                };
                let freq = GroupSnapshot {
                    name: "cpu_usage/freq".to_string(),
                    schema_hash: freq_schema.hash(),
                    schema: Some(std::sync::Arc::clone(&freq_schema)),
                    window: w,
                    counters: Vec::new(),
                    gauges: vec![Some(2_000 + i as i64)],
                    histograms: Vec::new(),
                };
                let sched = GroupSnapshot {
                    name: "cpu_usage/sched".to_string(),
                    schema_hash: sched_schema.hash(),
                    schema: Some(std::sync::Arc::clone(&sched_schema)),
                    window: w,
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
                        window: w_fast,
                        counters: vec![Some(i * 1_000)],
                        gauges: Vec::new(),
                        histograms: Vec::new(),
                    };
                    let softirq = GroupSnapshot {
                        name: "cpu_usage/softirq".to_string(),
                        schema_hash: softirq_schema.hash(),
                        schema: Some(std::sync::Arc::clone(&softirq_schema)),
                        window: w_slow,
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
                        window: w,
                        counters: vec![Some(i * 1_000)],
                        gauges: Vec::new(),
                        histograms: Vec::new(),
                    };
                    let softirq = GroupSnapshot {
                        name: "cpu_usage/softirq".to_string(),
                        schema_hash: softirq_schema.hash(),
                        schema: Some(std::sync::Arc::clone(&softirq_schema)),
                        window: slow_w,
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
                        window: w,
                        counters: vec![Some(i * 1_000)],
                        gauges: Vec::new(),
                        histograms: Vec::new(),
                    },
                    GroupSnapshot {
                        name: "cpu_usage/softirq".to_string(),
                        schema_hash: softirq_schema.hash(),
                        schema: Some(std::sync::Arc::clone(&softirq_schema)),
                        window: w,
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

        // The slow sampler's three rows, as the query path indexes them (its
        // raw 1.5/4.5/10.5 s snap to the nominal grid). N rows span N-1 gaps,
        // so the first yields no rate — nothing precedes it to measure across.
        assert_eq!(
            times,
            vec![5.0, 11.0],
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
                .cross_cadence_eval_timestamps(
                    "rate(cpu_cycles[2s]) + rate(reads[4s])",
                    1.0,
                    RateMode::Grid,
                )
                .is_some(),
            "the fixture must be one the policy fires on, or this proves nothing"
        );
        assert!(
            reader
                .cross_cadence_eval_timestamps(
                    "rate(cpu_cycles[2s]) + rate(reads[4s])",
                    1.0,
                    RateMode::Raw,
                )
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

    /// The typical gap is the MEDIAN, so one long hole — a restart, a missed
    /// poll — does not masquerade as the table's cadence.
    #[test]
    fn typical_gap_is_robust_to_a_single_long_hole() {
        const S: u64 = 1_000_000_000;
        let steady: Vec<u64> = (0..10).map(|i| i * S).collect();
        assert_eq!(typical_gap_ns(&steady), Some(S));

        // Same table, with a 100 s hole in the middle.
        let mut holed = steady.clone();
        holed.extend((0..10).map(|i| 110 * S + i * S));
        assert_eq!(
            typical_gap_ns(&holed),
            Some(S),
            "a mean would be dragged upward by the hole; the median must not be"
        );

        // Fewer than two rows spans no gap at all.
        assert_eq!(typical_gap_ns(&[]), None);
        assert_eq!(typical_gap_ns(&[42]), None);
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
}
