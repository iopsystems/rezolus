//! The `.rez` per-sampler archive: an uncompressed tar of `manifest.json` plus
//! one parquet table per sampler. See the Stage-3 plan header for the format
//! decisions.
//!
//! A table is one or more parquet *segments* (schema v2), so an archive is
//! either written whole (`manifest.json` first, then `<dir>/<sampler>.parquet`)
//! or streamed (segments interleaved with checkpoint manifests, the last of
//! which may be missing entirely on an unclean kill). Reading tolerates a
//! truncated tail; see `read_archive_reader`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// `.rez` manifest schema version written by this build.
pub const REZ_SCHEMA_VERSION: u32 = 2;
/// Highest manifest schema version this build can read. v1 shipped without a
/// forward gate (which is what makes a downgraded/compacted v1-shaped manifest
/// readable by old binaries); v2 adds one, because gates cannot be retrofitted.
pub const REZ_MAX_SUPPORTED_VERSION: u32 = 2;
/// Manifest filename inside the tar.
pub const REZ_MANIFEST_NAME: &str = "manifest.json";
/// Table-level wall-clock sidecar column. Reserved: the query engine skips a
/// column with exactly this name rather than surfacing it as a metric.
pub const WALL_OFFSET_COLUMN: &str = ":wall_offset";

/// Top-level `.rez` manifest (`manifest.json`): a bag of label-tagged recordings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RezManifest {
    pub version: u32,
    pub recordings: Vec<RezRecording>,
}

/// One recording = one endpoint on one host = a label set + its per-sampler tables.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RezRecording {
    /// Filesystem-safe directory holding this recording's parquet tables in the tar.
    pub dir: String,
    /// Arbitrary label set: `source`, `host` (from systeminfo), user `--label k=v`.
    pub labels: BTreeMap<String, String>,
    /// Per-recording metadata: the existing `parquet_metadata` keys
    /// (`systeminfo`, `descriptions`, `sampling_interval_ms`, ...).
    pub metadata: BTreeMap<String, String>,
    /// True iff this recording was cleanly finalized. Checkpoint manifests
    /// never set it, so an archive recovered from one presents as incomplete.
    /// It lives per-recording, not on the manifest: `parquet combine` merges
    /// recordings from different archives, where one recovered + one clean
    /// recording has no truthful top-level value. Absent (v1) means false.
    #[serde(default)]
    pub complete: bool,
    /// Wall-clock reading (ns since epoch) at recording start. Row timestamps
    /// are `anchor + monotonic elapsed`, so this pins the timeline to wall time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clock_anchor_wall_ns: Option<u64>,
    /// `(anchored_ts, wall_minus_anchored_ns)` observations, one per checkpoint:
    /// an at-a-glance clock-drift summary that needs no table decode. Each is a
    /// projection of some row's `:wall_offset` — the newest row in that
    /// checkpoint's seal batch — so the series never contradicts the tables.
    ///
    /// Not guaranteed sorted: the observation is a per-batch maximum, so an
    /// age-sealed slow sampler can append a timestamp older than one a
    /// fast-sampler batch already contributed. Consumers that need order should
    /// sort; consumers that need per-row precision should read the column.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub clock_offsets: Vec<(u64, i64)>,
    pub tables: Vec<RezTableIndex>,
}

/// One entry in the manifest's table index. A table is one or more parquet
/// segments; `rows`/`cadence_ns` are totals across them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RezTableIndex {
    pub sampler: String,
    /// v1 single-file name. Set only when the table is a single segment (so v1
    /// readers can still open compacted/atomically-written archives); never
    /// serialized as `null`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// Segment file names in segment order. Absent on v1 manifests.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    /// Column names. Defaulted because checkpoint manifests omit it: it is
    /// O(total columns) and reaches 100 KB+ on cgroup-heavy tables, and
    /// recovery does not need it to load segments.
    #[serde(default)]
    pub columns: Vec<String>,
    pub rows: u64,
    /// Observed mean row interval (ns); `None` when fewer than 2 rows.
    pub cadence_ns: Option<u64>,
}

impl RezTableIndex {
    /// The table's segment files in order: `files` when present, else the v1
    /// single `file`, else empty (a malformed index naming no data).
    pub fn segment_files(&self) -> Vec<&str> {
        if !self.files.is_empty() {
            self.files.iter().map(String::as_str).collect()
        } else {
            self.file.as_deref().into_iter().collect()
        }
    }
}

use std::collections::HashMap;
use std::sync::Arc;

use arrow::array::{Array, ArrayRef, Int64Array, ListBuilder, UInt64Array, UInt64Builder};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use metriken::Window;
use parquet::arrow::ArrowWriter;
// Only the test-only eager reader (read_table_parquet) needs these.
#[cfg(test)]
use arrow::array::ListArray;
#[cfg(test)]
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

/// Per-metric column values for a table (row-aligned with the table's timestamps).
#[derive(Debug, Clone, PartialEq)]
pub enum RezValues {
    Counter(Vec<Option<u64>>),
    Gauge(Vec<Option<i64>>),
    Histogram(Vec<Option<histogram::Histogram>>),
}

/// One metric column plus its per-row acquisition windows.
#[derive(Debug, Clone)]
pub struct RezColumn {
    /// Column key (the snapshot entry's numeric-id name, e.g. `"5"` / `"5x3"`).
    pub name: String,
    /// Metric identity + annotations (`metric`, `sampler`, labels, `metric_type`).
    pub metadata: HashMap<String, String>,
    pub values: RezValues,
    pub windows: Vec<Option<Window>>,
}

/// One sampler's table: a timestamp column plus its metric/window columns.
#[derive(Debug, Clone)]
pub struct RezTable {
    /// Read by the atomic writer only — the streaming writer names segments
    /// from `SealJob::sampler`. See the note on `RezRecorder`.
    #[allow(dead_code)]
    pub sampler: String,
    pub timestamps: Vec<u64>,
    /// Per-row wall-clock observation: the raw `SystemTime` reading minus the
    /// row's (monotonically anchored) timestamp, in nanoseconds. Row-aligned
    /// with `timestamps`, or empty when the table carries no observations (a
    /// table decoded from an archive written before the sidecar existed).
    /// Serialized as the table-level `:wall_offset` column, which the query
    /// engine skips the same way it skips the `:window_*` sidecars.
    pub wall_offsets: Vec<i64>,
    pub columns: Vec<RezColumn>,
}

type RezError = Box<dyn std::error::Error>;

/// Mean row interval hint; `None` when fewer than 2 rows.
///
/// Atomic-writer only: the streaming writer keeps the equivalent running totals
/// per table because its segments are long gone by manifest time. See the note
/// on `RezRecorder`.
#[allow(dead_code)]
pub fn cadence_hint(timestamps: &[u64]) -> Option<u64> {
    if timestamps.len() < 2 {
        return None;
    }
    let span = timestamps.last().unwrap().saturating_sub(timestamps[0]);
    Some(span / (timestamps.len() as u64 - 1))
}

fn window_offset_columns(
    windows: &[Option<Window>],
    ts: &[u64],
) -> (Vec<Option<i64>>, Vec<Option<u64>>) {
    let mut begin = Vec::with_capacity(windows.len());
    let mut width = Vec::with_capacity(windows.len());
    for (w, &t) in windows.iter().zip(ts.iter()) {
        match w {
            Some(win) => {
                begin.push(Some(win.begin_ns as i64 - t as i64));
                width.push(Some(win.width_ns()));
            }
            None => {
                begin.push(None);
                width.push(None);
            }
        }
    }
    (begin, width)
}

fn build_histogram_list(values: &[Option<histogram::Histogram>]) -> ArrayRef {
    let mut b = ListBuilder::new(UInt64Builder::new());
    for v in values {
        match v {
            Some(h) => {
                for &c in h.as_slice() {
                    b.values().append_value(c);
                }
                b.append(true);
            }
            None => b.append(false),
        }
    }
    Arc::new(b.finish())
}

fn table_to_batch(table: &RezTable) -> Result<(Arc<Schema>, RecordBatch), RezError> {
    let mut fields: Vec<Field> = Vec::new();
    let mut arrays: Vec<ArrayRef> = Vec::new();

    fields.push(
        Field::new("timestamp", DataType::UInt64, false).with_metadata(HashMap::from([
            ("metric_type".to_string(), "timestamp".to_string()),
            ("unit".to_string(), "nanoseconds".to_string()),
        ])),
    );
    arrays.push(Arc::new(UInt64Array::from(table.timestamps.clone())));

    // Table-level (not per-metric) sidecar: one wall-clock observation per row.
    // Null where the table carries no observation for that row; a length
    // mismatch against `timestamps` surfaces as a `RecordBatch` error.
    fields.push(Field::new(WALL_OFFSET_COLUMN, DataType::Int64, true));
    arrays.push(Arc::new(if table.wall_offsets.is_empty() {
        Int64Array::from(vec![None; table.timestamps.len()])
    } else {
        Int64Array::from(table.wall_offsets.clone())
    }));

    for col in &table.columns {
        match &col.values {
            RezValues::Counter(v) => {
                fields.push(
                    Field::new(&col.name, DataType::UInt64, true)
                        .with_metadata(col.metadata.clone()),
                );
                arrays.push(Arc::new(UInt64Array::from(v.clone())));
            }
            RezValues::Gauge(v) => {
                fields.push(
                    Field::new(&col.name, DataType::Int64, true)
                        .with_metadata(col.metadata.clone()),
                );
                arrays.push(Arc::new(Int64Array::from(v.clone())));
            }
            RezValues::Histogram(v) => {
                let arr = build_histogram_list(v);
                fields.push(
                    Field::new(
                        format!("{}:buckets", col.name),
                        arr.data_type().clone(),
                        true,
                    )
                    .with_metadata(col.metadata.clone()),
                );
                arrays.push(arr);
            }
        }

        let (begin, width) = window_offset_columns(&col.windows, &table.timestamps);
        fields.push(Field::new(
            format!("{}:window_begin", col.name),
            DataType::Int64,
            true,
        ));
        arrays.push(Arc::new(Int64Array::from(begin)));
        fields.push(Field::new(
            format!("{}:window_width", col.name),
            DataType::UInt64,
            true,
        ));
        arrays.push(Arc::new(UInt64Array::from(width)));
    }

    let schema = Arc::new(Schema::new(fields));
    let batch = RecordBatch::try_new(schema.clone(), arrays)?;
    Ok((schema, batch))
}

/// Serialize one table to parquet bytes.
pub fn write_table_parquet(table: &RezTable) -> Result<Vec<u8>, RezError> {
    let (schema, batch) = table_to_batch(table)?;
    let mut buf: Vec<u8> = Vec::new();
    let mut writer = ArrowWriter::try_new(&mut buf, schema, None)?;
    writer.write(&batch)?;
    writer.close()?;
    Ok(buf)
}

#[cfg(test)]
fn u64_col(a: &ArrayRef) -> &UInt64Array {
    a.as_any()
        .downcast_ref::<UInt64Array>()
        .expect("UInt64 column")
}

/// Deserialize one table from parquet bytes. Test-only: the production read
/// path decodes tables lazily via metriken-query's `ParquetReader`
/// (`read_archive_bytes` → `RezReader`); this eager decoder exists to verify
/// the write path independently.
#[cfg(test)]
pub fn read_table_parquet(sampler: String, bytes: Vec<u8>) -> Result<RezTable, RezError> {
    let reader = ParquetRecordBatchReaderBuilder::try_new(bytes::Bytes::from(bytes))?.build()?;

    let mut timestamps: Vec<u64> = Vec::new();
    let mut wall_offsets: Vec<i64> = Vec::new();
    let mut order: Vec<String> = Vec::new();
    let mut values: HashMap<String, RezValues> = HashMap::new();
    let mut metas: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut begins: HashMap<String, Vec<Option<i64>>> = HashMap::new();
    let mut widths: HashMap<String, Vec<Option<u64>>> = HashMap::new();

    for batch in reader {
        let batch = batch?;
        let schema = batch.schema();
        for i in 0..batch.num_columns() {
            let field = schema.field(i);
            let name = field.name();
            let col = batch.column(i);
            if name == "timestamp" {
                let a = u64_col(col);
                timestamps.extend((0..a.len()).map(|r| a.value(r)));
            } else if name == WALL_OFFSET_COLUMN {
                let a = col
                    .as_any()
                    .downcast_ref::<Int64Array>()
                    .expect("i64 wall_offset");
                // An all-null column means the table carried no observations;
                // leave `wall_offsets` empty so a write→read→write round trip
                // does not fabricate zeros.
                if a.null_count() < a.len() {
                    wall_offsets.extend((0..a.len()).map(|r| a.value(r)));
                }
            } else if let Some(base) = name.strip_suffix(":window_begin") {
                let a = col
                    .as_any()
                    .downcast_ref::<Int64Array>()
                    .expect("i64 window_begin");
                begins
                    .entry(base.to_string())
                    .or_default()
                    .extend((0..a.len()).map(|r| (!a.is_null(r)).then(|| a.value(r))));
            } else if let Some(base) = name.strip_suffix(":window_width") {
                let a = u64_col(col);
                widths
                    .entry(base.to_string())
                    .or_default()
                    .extend((0..a.len()).map(|r| (!a.is_null(r)).then(|| a.value(r))));
            } else if let Some(base) = name.strip_suffix(":buckets") {
                let meta = field.metadata().clone();
                let gp: u8 = meta
                    .get("grouping_power")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                let mvp: u8 = meta
                    .get("max_value_power")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                let list = col
                    .as_any()
                    .downcast_ref::<ListArray>()
                    .expect("list histogram");
                let entry = match values.entry(base.to_string()) {
                    std::collections::hash_map::Entry::Vacant(v) => {
                        order.push(base.to_string());
                        metas.insert(base.to_string(), meta);
                        v.insert(RezValues::Histogram(Vec::new()))
                    }
                    std::collections::hash_map::Entry::Occupied(o) => o.into_mut(),
                };
                if let RezValues::Histogram(hs) = entry {
                    for r in 0..list.len() {
                        if list.is_null(r) {
                            hs.push(None);
                        } else {
                            let vals = list.value(r);
                            let a = u64_col(&vals);
                            let buckets: Vec<u64> = (0..a.len()).map(|k| a.value(k)).collect();
                            hs.push(Some(histogram::Histogram::from_buckets(gp, mvp, buckets)?));
                        }
                    }
                }
            } else {
                // A metric value column: counter (UInt64) or gauge (Int64).
                let meta = field.metadata().clone();
                let is_gauge = meta.get("metric_type").map(String::as_str) == Some("gauge");
                let entry = match values.entry(name.to_string()) {
                    std::collections::hash_map::Entry::Vacant(v) => {
                        order.push(name.to_string());
                        metas.insert(name.to_string(), meta);
                        v.insert(if is_gauge {
                            RezValues::Gauge(Vec::new())
                        } else {
                            RezValues::Counter(Vec::new())
                        })
                    }
                    std::collections::hash_map::Entry::Occupied(o) => o.into_mut(),
                };
                match entry {
                    RezValues::Counter(vs) => {
                        let a = u64_col(col);
                        vs.extend((0..a.len()).map(|r| (!a.is_null(r)).then(|| a.value(r))));
                    }
                    RezValues::Gauge(vs) => {
                        let a = col
                            .as_any()
                            .downcast_ref::<Int64Array>()
                            .expect("i64 gauge");
                        vs.extend((0..a.len()).map(|r| (!a.is_null(r)).then(|| a.value(r))));
                    }
                    RezValues::Histogram(_) => {}
                }
            }
        }
    }

    let columns = order
        .into_iter()
        .map(|base| {
            let begin = begins.remove(&base).unwrap_or_default();
            let width = widths.remove(&base).unwrap_or_default();
            let windows = (0..timestamps.len())
                .map(|r| {
                    match (
                        begin.get(r).copied().flatten(),
                        width.get(r).copied().flatten(),
                    ) {
                        (Some(b), Some(w)) => {
                            let begin_ns = (timestamps[r] as i64 + b) as u64;
                            Some(Window::new(begin_ns, begin_ns + w))
                        }
                        _ => None,
                    }
                })
                .collect();
            RezColumn {
                metadata: metas.remove(&base).unwrap_or_default(),
                values: values.remove(&base).unwrap(),
                windows,
                name: base,
            }
        })
        .collect();

    Ok(RezTable {
        sampler,
        timestamps,
        wall_offsets,
        columns,
    })
}

use std::io::Read;
use std::path::Path;

/// One recording's data to serialize (borrowed tables).
#[allow(dead_code)]
pub struct RecordingData<'a> {
    pub dir: String,
    pub labels: BTreeMap<String, String>,
    pub metadata: BTreeMap<String, String>,
    pub tables: &'a [RezTable],
}

/// A decoded `.rez` archive (round-trip / test surface).
#[cfg(test)]
pub struct RezArchive {
    pub manifest: RezManifest,
    /// Decoded tables, one inner `Vec` per `manifest.recordings` entry (parallel order).
    pub tables: Vec<Vec<RezTable>>,
}

pub(crate) fn append_tar_entry<W: std::io::Write>(
    builder: &mut tar::Builder<W>,
    name: &str,
    bytes: &[u8],
) -> Result<(), RezError> {
    let mut header = tar::Header::new_gnu();
    header.set_path(name)?;
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    builder.append(&header, bytes)?;
    Ok(())
}

/// Write `recordings` to an uncompressed `.rez` tar at `path`. Each recording's
/// tables are nested under `<dir>/<sampler>.parquet`.
///
/// Atomic (whole-archive) writer — see the note on `RezRecorder`.
#[allow(dead_code)]
pub fn write_archive(path: &Path, recordings: &[RecordingData]) -> Result<(), RezError> {
    let mut manifest_recordings = Vec::with_capacity(recordings.len());
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    for rec in recordings {
        let mut index = Vec::with_capacity(rec.tables.len());
        for table in rec.tables {
            let file = format!("{}.parquet", table.sampler);
            let bytes = write_table_parquet(table)?;
            index.push(RezTableIndex {
                sampler: table.sampler.clone(),
                file: Some(file.clone()),
                files: vec![file.clone()],
                columns: table.columns.iter().map(|c| c.name.clone()).collect(),
                rows: table.timestamps.len() as u64,
                cadence_ns: cadence_hint(&table.timestamps),
            });
            files.push((format!("{}/{}", rec.dir, file), bytes));
        }
        manifest_recordings.push(RezRecording {
            dir: rec.dir.clone(),
            labels: rec.labels.clone(),
            metadata: rec.metadata.clone(),
            // This writer emits the whole archive at once, so its recordings
            // are complete by construction.
            complete: true,
            clock_anchor_wall_ns: None,
            clock_offsets: Vec::new(),
            tables: index,
        });
    }
    let manifest = RezManifest {
        version: REZ_SCHEMA_VERSION,
        recordings: manifest_recordings,
    };
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)?;

    let out = std::fs::File::create(path)?;
    let mut builder = tar::Builder::new(out);
    builder.mode(tar::HeaderMode::Deterministic);
    append_tar_entry(&mut builder, REZ_MANIFEST_NAME, &manifest_bytes)?;
    for (name, bytes) in &files {
        append_tar_entry(&mut builder, name, bytes)?;
    }
    builder.into_inner()?.sync_all()?;
    Ok(())
}

/// One recording's manifest entry paired with its table bytes: the outer `Vec`
/// is parallel to `RezRecording::tables`, the inner one holds that table's
/// segments in order.
pub type RecordingSegments = (RezRecording, Vec<Vec<Vec<u8>>>);

/// The names a table's `segments` get inside its recording directory. A single
/// segment keeps the v1 `<sampler>.parquet` shape so v1 binaries can still open
/// tool output; multiple segments live under `<sampler>/`, zero-padded in
/// segment order.
fn canonical_segment_names(sampler: &str, segments: usize) -> Vec<String> {
    if segments == 1 {
        vec![format!("{sampler}.parquet")]
    } else {
        (0..segments)
            .map(|i| format!("{sampler}/{i:04}.parquet"))
            .collect()
    }
}

/// Write a multi-recording `.rez` from already-encoded per-table parquet bytes
/// (no re-encode). `recordings` pairs each recording's manifest entry with its
/// table bytes (parallel to `recording.tables`), each table's segments in order.
///
/// The writer **owns the file naming**: every table's index entry is rewritten
/// to name exactly the files emitted for it (`canonical_segment_names`), and the
/// incoming `file`/`files` are discarded. This is load-bearing, not cosmetic —
/// `combine`/`filter`/`annotate` carry the input's `RezTableIndex` verbatim, so
/// on segmented input a stale entry would name segments the output never writes
/// and break every downstream reader. Everything else on the entry (`columns`,
/// `rows`, `cadence_ns`) and on the recording (`complete`, clock fields) copies
/// through untouched, and segment bytes pass through byte-identical — only the
/// compactor ever merges segments.
///
/// Errors on a duplicate `dir` or duplicate sampler within a recording (the
/// reader keys tables by `<dir>/<file>`, so either collision would clobber) and
/// on a table-count/bytes mismatch. All validation runs before the output file
/// is created. The caller assigns unique dirs.
pub fn write_archive_bytes(path: &Path, recordings: &[RecordingSegments]) -> Result<(), RezError> {
    let mut seen_dirs = std::collections::HashSet::new();
    // The manifest entries as they will be written, with canonical file names.
    let mut canonical: Vec<RezRecording> = Vec::with_capacity(recordings.len());
    for (rec, table_bytes) in recordings {
        if !seen_dirs.insert(rec.dir.clone()) {
            return Err(format!("duplicate recording dir {:?}", rec.dir).into());
        }
        if table_bytes.len() != rec.tables.len() {
            return Err(format!(
                "recording {} has {} table index entries but {} byte blobs",
                rec.dir,
                rec.tables.len(),
                table_bytes.len()
            )
            .into());
        }
        let mut seen_samplers = std::collections::HashSet::new();
        let mut rec = rec.clone();
        for (idx, segments) in rec.tables.iter_mut().zip(table_bytes) {
            if !seen_samplers.insert(idx.sampler.clone()) {
                return Err(format!(
                    "recording {} has two tables named {:?}",
                    rec.dir, idx.sampler
                )
                .into());
            }
            let names = canonical_segment_names(&idx.sampler, segments.len());
            // v1-shaped iff single-segment; `files` is always the full list.
            idx.file = match names.as_slice() {
                [one] => Some(one.clone()),
                _ => None,
            };
            idx.files = names;
        }
        canonical.push(rec);
    }
    let manifest = RezManifest {
        version: REZ_SCHEMA_VERSION,
        recordings: canonical,
    };
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)?;

    let out = std::fs::File::create(path)?;
    let mut builder = tar::Builder::new(out);
    builder.mode(tar::HeaderMode::Deterministic);
    append_tar_entry(&mut builder, REZ_MANIFEST_NAME, &manifest_bytes)?;
    // Names come from the manifest just built, so entries and index agree by
    // construction — no per-table parity check is possible here.
    for (rec, (_, table_bytes)) in manifest.recordings.iter().zip(recordings) {
        for (idx, segments) in rec.tables.iter().zip(table_bytes) {
            for (name, bytes) in idx.files.iter().zip(segments) {
                append_tar_entry(&mut builder, &format!("{}/{}", rec.dir, name), bytes)?;
            }
        }
    }
    builder.into_inner()?.sync_all()?;
    Ok(())
}

/// Read a `.rez` archive back into its manifest + decoded tables (per recording).
/// Test-only eager reader; production uses `read_archive_bytes` → `RezReader`.
#[cfg(test)]
pub fn read_archive(path: &Path) -> Result<RezArchive, RezError> {
    let (manifest, recordings) = read_archive_bytes(path)?;
    let mut all = Vec::with_capacity(recordings.len());
    for rec in recordings {
        let mut tables = Vec::with_capacity(rec.tables.len());
        for (sampler, segments) in rec.tables {
            // The eager decoder produces one `RezTable` per blob; segmented
            // tables are the segment-aware source's job, not this test surface.
            let bytes = match <[Vec<u8>; 1]>::try_from(segments) {
                Ok([bytes]) => bytes,
                Err(segs) => {
                    return Err(format!(
                        "table {sampler} has {} segments; read_archive decodes \
                         single-segment tables only",
                        segs.len()
                    )
                    .into());
                }
            };
            tables.push(read_table_parquet(sampler, bytes)?);
        }
        all.push(tables);
    }
    Ok(RezArchive {
        manifest,
        tables: all,
    })
}

/// One recording's raw per-sampler parquet bytes (for building readers).
pub struct RecordingBytes {
    pub dir: String,
    pub labels: BTreeMap<String, String>,
    pub metadata: BTreeMap<String, String>,
    /// False when the recording was recovered from a checkpoint rather than
    /// cleanly finalized — data after its last row may be missing. Always true
    /// for v1 archives, which predate unclean-kill recovery.
    pub complete: bool,
    /// `(sampler, segment_bytes)` in manifest order; segments in segment order.
    pub tables: Vec<(String, Vec<Vec<u8>>)>,
}

/// Take the parquet blobs `manifest` references out of `blobs`.
///
/// Two phases on purpose. The presence check runs first and mutates nothing,
/// so a manifest that does not resolve leaves `blobs` intact for the next
/// (older) manifest to try. Only then are the bytes *moved* out rather than
/// cloned — a `.rez` is read whole into memory, so copying would double peak
/// RSS on a large archive.
fn resolve(
    manifest: &RezManifest,
    blobs: &mut HashMap<String, Vec<u8>>,
) -> Result<Vec<RecordingBytes>, RezError> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for rec in &manifest.recordings {
        for idx in &rec.tables {
            for seg in idx.segment_files() {
                let path = format!("{}/{}", rec.dir, seg);
                if !blobs.contains_key(&path) {
                    return Err(format!("missing table file {path}").into());
                }
                // Two index entries naming one file would make the move below
                // ill-defined; reject before touching anything.
                if !seen.insert(path.clone()) {
                    return Err(format!("table file {path} is referenced twice").into());
                }
            }
        }
    }

    // v1 predates unclean-kill recovery: every v1 writer emitted the whole
    // archive at once, so a v1 recording is complete and the flag is not read.
    let interpret_complete = manifest.version >= 2;
    let mut recordings = Vec::with_capacity(manifest.recordings.len());
    for rec in &manifest.recordings {
        let mut tables = Vec::with_capacity(rec.tables.len());
        for idx in &rec.tables {
            let mut segments = Vec::new();
            for seg in idx.segment_files() {
                let path = format!("{}/{}", rec.dir, seg);
                segments.push(
                    blobs
                        .remove(&path)
                        .ok_or_else(|| format!("missing table file {path}"))?,
                );
            }
            tables.push((idx.sampler.clone(), segments));
        }
        recordings.push(RecordingBytes {
            dir: rec.dir.clone(),
            labels: rec.labels.clone(),
            metadata: rec.metadata.clone(),
            complete: !interpret_complete || rec.complete,
            tables,
        });
    }
    Ok(recordings)
}

/// Read a `.rez` from any reader into its manifest + per-recording raw parquet
/// bytes (unlike `read_archive`, which decodes tables into `RezTable`s).
///
/// Tar iteration is truncation-tolerant, because an unclean kill (SIGKILL,
/// power loss) is a supported way for a streaming recording to end. The rules,
/// stated precisely because the `tar` crate does not error where one would
/// expect: an entry counts only if the bytes read equal the header's declared
/// size; any tar error, short read, or unparseable manifest ends iteration.
/// Recovery then uses the newest manifest whose files are all present — the
/// newest one written may reference segments the truncated tail ate, since a
/// checkpoint manifest is always followed by more segments.
pub fn read_archive_reader<R: std::io::Read>(
    reader: R,
) -> Result<(RezManifest, Vec<RecordingBytes>), RezError> {
    let mut archive = tar::Archive::new(reader);
    // Every manifest in the archive, oldest first: the streaming writer appends
    // a checkpoint manifest after each seal batch (duplicate tar names are
    // legal and this reader is order-agnostic).
    let mut manifests: Vec<RezManifest> = Vec::new();
    let mut blobs: HashMap<String, Vec<u8>> = HashMap::new();

    for entry in archive.entries()? {
        // A truncated header errors here; everything before it is still good.
        let Ok(mut entry) = entry else { break };
        let size = entry.size();
        let name = match entry.path() {
            Ok(p) => p.to_string_lossy().into_owned(),
            Err(_) => break,
        };
        let mut buf = Vec::new();
        // A mid-data truncation yields the entry `Ok` and reads a *silently*
        // short buffer (the body is a `Take` over a reader already at EOF), so
        // the length check — not the `Result` — is what catches it.
        match entry.read_to_end(&mut buf) {
            Ok(read) if read as u64 == size => {}
            _ => break,
        }
        if name == REZ_MANIFEST_NAME {
            match serde_json::from_slice::<RezManifest>(&buf) {
                Ok(m) => {
                    if m.version > REZ_MAX_SUPPORTED_VERSION {
                        return Err(format!(
                            "unsupported .rez manifest version {} (this build reads up to \
                             version {}); upgrade rezolus to read this archive",
                            m.version, REZ_MAX_SUPPORTED_VERSION
                        )
                        .into());
                    }
                    manifests.push(m);
                }
                // A half-persisted checkpoint manifest ends iteration; recovery
                // falls back to the previous one.
                Err(_) => break,
            }
        } else if name.ends_with(".parquet") {
            blobs.insert(name, buf);
        }
    }

    if manifests.is_empty() {
        return Err("missing manifest.json".into());
    }
    // Newest resolvable manifest wins; report the newest failure if none does.
    let mut newest_err: Option<RezError> = None;
    while let Some(mut manifest) = manifests.pop() {
        match resolve(&manifest, &mut blobs) {
            Ok(recordings) => {
                // Normalize `complete` on the manifest too, by the same rule
                // `resolve` applies to `RecordingBytes` (v1 recordings are
                // complete by construction; the absent flag is not read). The
                // tools copy `RezRecording` verbatim into an output stamped
                // `version: 2`, so without this a clean v1 archive would come
                // out of `combine`/`filter`/`annotate` as `complete: false` and
                // read back as "not cleanly finalized".
                if manifest.version < 2 {
                    for rec in &mut manifest.recordings {
                        rec.complete = true;
                    }
                }
                return Ok((manifest, recordings));
            }
            Err(e) => newest_err = newest_err.or(Some(e)),
        }
    }
    Err(newest_err.unwrap_or_else(|| "missing manifest.json".into()))
}

/// Read a `.rez` archive at `path` into manifest + per-recording raw bytes.
pub fn read_archive_bytes(path: &Path) -> Result<(RezManifest, Vec<RecordingBytes>), RezError> {
    read_archive_reader(std::fs::File::open(path)?)
}

/// True if `reader` yields a `.rez` archive: an uncompressed tar containing a
/// top-level `manifest.json` member. Distinguishes `.rez` from the A/B tarball
/// (which has `ab.json` + root-level parquets, no `manifest.json`) and from a
/// bare parquet (not a tar). Consumes the reader.
pub fn is_rez_reader<R: std::io::Read>(reader: R) -> Result<bool, RezError> {
    let mut archive = tar::Archive::new(reader);
    let entries = match archive.entries() {
        Ok(e) => e,
        Err(_) => return Ok(false),
    };
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => return Ok(false),
        };
        if entry.path()?.to_string_lossy() == REZ_MANIFEST_NAME {
            return Ok(true);
        }
    }
    Ok(false)
}

/// True if the file at `path` is a `.rez` archive (by content, not extension).
pub fn is_rez_path(path: &Path) -> Result<bool, RezError> {
    is_rez_reader(std::fs::File::open(path)?)
}

/// Which container a `.rez` path holds. v1/v2 are tar archives; v3 is SQLite.
///
/// Not yet consumed by any caller — `is_rez_path` still gates every existing
/// `.rez` consumer unchanged. Tasks C1/C2 migrate callers over to this.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RezFormat {
    V3Sqlite,
    V2Tar,
    NotRez,
}

/// SQLite's file header: bytes 0..16 of every database it creates.
const SQLITE_MAGIC: &[u8; 16] = b"SQLite format 3\0";

/// Detect a `.rez` container by content, not extension.
///
/// The SQLite check goes first because it is a 16-byte read, while the tar
/// sniff walks entries looking for `manifest.json`. A file shorter than the
/// magic simply falls through. This sits in front of `is_rez_path` without
/// changing it: existing callers keep calling `is_rez_path`/`is_rez_reader`
/// exactly as before, and only see v2 tar archives. A v3 SQLite file is not a
/// tar, so `is_rez_path` correctly (and unchanged) reports `false` for it;
/// callers must move to `detect_rez_format` to recognize v3.
#[allow(dead_code)]
pub fn detect_rez_format(path: &Path) -> Result<RezFormat, RezError> {
    let mut file = std::fs::File::open(path)?;
    let mut header = [0u8; 16];
    let is_sqlite = match file.read_exact(&mut header) {
        Ok(()) => &header == SQLITE_MAGIC,
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => false,
        Err(e) => return Err(e.into()),
    };
    if is_sqlite {
        return Ok(RezFormat::V3Sqlite);
    }
    if is_rez_path(path)? {
        Ok(RezFormat::V2Tar)
    } else {
        Ok(RezFormat::NotRez)
    }
}

use metriken_exposition::{Counter, Gauge, Histogram, Snapshot};

/// A borrowed snapshot entry, tagged by shape.
pub(crate) enum Entry<'a> {
    Counter(&'a Counter),
    Gauge(&'a Gauge),
    Histogram(&'a Histogram),
}

impl Entry<'_> {
    fn name(&self) -> &str {
        match self {
            Entry::Counter(c) => &c.name,
            Entry::Gauge(g) => &g.name,
            Entry::Histogram(h) => &h.name,
        }
    }
    fn metadata(&self) -> &HashMap<String, String> {
        match self {
            Entry::Counter(c) => &c.metadata,
            Entry::Gauge(g) => &g.metadata,
            Entry::Histogram(h) => &h.metadata,
        }
    }
    fn window(&self) -> Option<Window> {
        match self {
            Entry::Counter(c) => c.window,
            Entry::Gauge(g) => g.window,
            Entry::Histogram(h) => h.window,
        }
    }
    /// The `metric_type` string the parquet reader keys on to reconstruct the
    /// column's value shape (counter vs gauge; histograms carry a `:buckets`
    /// suffix, so their `metric_type` is informational).
    fn metric_type(&self) -> &'static str {
        match self {
            Entry::Counter(_) => "counter",
            Entry::Gauge(_) => "gauge",
            Entry::Histogram(_) => "histogram",
        }
    }
}

/// In-memory cost of a cell's value slot: `Option<u64>` / `Option<i64>` /
/// `Option<Box<[u64]>>` are all 16 B (the histogram's buckets are counted
/// separately below).
const VALUE_SLOT_BYTES: usize = 16;
/// In-memory cost of the `Option<Window>` that `push_row` pushes alongside
/// every counted cell. Measured, not assumed: 24 B. `Window` is two `u64`s
/// with no niche, so the option tag costs a whole word of padding.
const WINDOW_SLOT_BYTES: usize = 24;
/// Per-cell overhead: value slot + window slot.
///
/// This is a *memory bound*, and until now it was 2.5x optimistic: the window
/// slot was documented as included but never counted, so a scalar cell was
/// charged 16 B against a true 40 B and an 8 MiB-capped scalar table really
/// held 20.0 MiB of builder RSS. Counting it means the effective cap is now
/// reached ~2.5x sooner for scalar-heavy tables (worst measured segment drops
/// 6.23 MiB -> 2.49 MiB encoded). That is intended: the cap is what bounds
/// resident memory, so it has to be honest about what a cell costs.
const CELL_OVERHEAD_BYTES: usize = VALUE_SLOT_BYTES + WINDOW_SLOT_BYTES;
/// Bytes per histogram bucket: `push_row` clones the histogram's bucket
/// `Box<[u64]>` into the column.
const HISTOGRAM_BUCKET_BYTES: usize = 8;

/// A growing per-sampler table. Columns are sparse: shorter than the row count
/// until padded (a metric absent in some rows gets `None` there).
pub(crate) struct TableBuilder {
    sampler: String,
    timestamps: Vec<u64>,
    wall_offsets: Vec<i64>,
    order: Vec<String>,
    columns: HashMap<String, RezColumn>,
    /// Atomic-writer dedup state. `StreamRecorder` keeps its keys outside the
    /// builder instead, so they survive a segment rotation.
    #[allow(dead_code)]
    last_key: Option<u64>,
    approx_bytes: usize,
}

impl TableBuilder {
    pub(crate) fn new(sampler: String) -> Self {
        Self {
            sampler,
            timestamps: Vec::new(),
            wall_offsets: Vec::new(),
            order: Vec::new(),
            columns: HashMap::new(),
            last_key: None,
            approx_bytes: 0,
        }
    }

    /// Approximate in-memory bytes accumulated by the cells pushed so far.
    ///
    /// This is what the streaming writer's seal threshold is measured in.
    /// Serialized parquet size cannot be estimated cheaply — a dry-run encode
    /// is exactly the cost that gets moved off the scrape thread, and static
    /// per-row guesses are off by orders of magnitude for histogram tables — so
    /// the cap bounds the two things it can measure exactly and in O(1) per
    /// entry: the builder's memory footprint and the encoder's input size.
    /// Counts only pushed cells — null back-padding of a late-appearing column
    /// is not accounted, so the number slightly under-reports a sparse table.
    /// Resets with the builder: a fresh builder (a post-rotation segment)
    /// starts at zero.
    pub(crate) fn approx_bytes(&self) -> usize {
        self.approx_bytes
    }

    /// Rows appended so far (the row-count seal threshold, and the
    /// "never seal an empty builder" test).
    pub(crate) fn rows(&self) -> usize {
        self.timestamps.len()
    }

    fn col_len(col: &RezColumn) -> usize {
        match &col.values {
            RezValues::Counter(v) => v.len(),
            RezValues::Gauge(v) => v.len(),
            RezValues::Histogram(v) => v.len(),
        }
    }

    fn pad(col: &mut RezColumn, to: usize) {
        while Self::col_len(col) < to {
            match &mut col.values {
                RezValues::Counter(v) => v.push(None),
                RezValues::Gauge(v) => v.push(None),
                RezValues::Histogram(v) => v.push(None),
            }
            col.windows.push(None);
        }
    }

    /// Append one row: `snapshot_ts` is the row's timestamp and
    /// `wall_offset_ns` the wall-clock observation for that tick (raw
    /// `SystemTime` reading minus `snapshot_ts`), stored once per row in the
    /// table-level `:wall_offset` sidecar.
    pub(crate) fn push_row(
        &mut self,
        snapshot_ts: u64,
        wall_offset_ns: i64,
        entries: &[Entry<'_>],
    ) {
        let row = self.timestamps.len();
        self.timestamps.push(snapshot_ts);
        self.wall_offsets.push(wall_offset_ns);
        let mut added_bytes = 0usize;
        for e in entries {
            let name = e.name().to_string();
            let order = &mut self.order;
            let col = self.columns.entry(name.clone()).or_insert_with(|| {
                order.push(name.clone());
                let values = match e {
                    Entry::Counter(_) => RezValues::Counter(Vec::new()),
                    Entry::Gauge(_) => RezValues::Gauge(Vec::new()),
                    Entry::Histogram(_) => RezValues::Histogram(Vec::new()),
                };
                let mut metadata = e.metadata().clone();
                metadata
                    .entry("metric_type".to_string())
                    .or_insert_with(|| e.metric_type().to_string());
                RezColumn {
                    name,
                    metadata,
                    values,
                    windows: Vec::new(),
                }
            });
            Self::pad(col, row);
            // The window is pushed only where the value was: an entry whose
            // shape does not match the column's established type is skipped
            // entirely (an agent restart can remap a numeric id and flip a
            // column from counter to gauge mid-recording). Pushing the window
            // regardless would leave `windows` one longer than `values` and
            // shift every later row's window onto the wrong value.
            let cell_bytes = match (e, &mut col.values) {
                (Entry::Counter(c), RezValues::Counter(v)) => {
                    v.push(Some(c.value));
                    Some(CELL_OVERHEAD_BYTES)
                }
                (Entry::Gauge(g), RezValues::Gauge(v)) => {
                    v.push(Some(g.value));
                    Some(CELL_OVERHEAD_BYTES)
                }
                (Entry::Histogram(h), RezValues::Histogram(v)) => {
                    // The clone below copies the whole bucket `Box<[u64]>`
                    // (7,424 buckets ≈ 58 KB at gp=7/mvp=64), which is why the
                    // cap counts bytes rather than rows.
                    let buckets = h.value.as_slice().len();
                    v.push(Some(h.value.clone()));
                    Some(CELL_OVERHEAD_BYTES + buckets * HISTOGRAM_BUCKET_BYTES)
                }
                _ => None,
            };
            if let Some(bytes) = cell_bytes {
                col.windows.push(e.window());
                added_bytes += bytes;
            }
        }
        self.approx_bytes += added_bytes;
    }

    pub(crate) fn finish(mut self) -> RezTable {
        let rows = self.timestamps.len();
        let columns = self
            .order
            .iter()
            .map(|name| {
                let mut col = self.columns.remove(name).unwrap();
                Self::pad(&mut col, rows);
                col
            })
            .collect();
        RezTable {
            sampler: self.sampler,
            timestamps: self.timestamps,
            wall_offsets: self.wall_offsets,
            columns,
        }
    }
}

/// Partition a snapshot's metrics by their `sampler` label (`"unattributed"`
/// when the label is absent). Shared by the in-memory `RezRecorder` and the
/// streaming `StreamRecorder` so the two ingest paths cannot drift apart.
pub(crate) fn group_by_sampler(snapshot: &Snapshot) -> BTreeMap<&str, Vec<Entry<'_>>> {
    let (counters, gauges, histograms) = match snapshot {
        Snapshot::V1(s) => (&s.counters, &s.gauges, &s.histograms),
        Snapshot::V2(s) => (&s.counters, &s.gauges, &s.histograms),
    };

    fn sampler_of(metadata: &HashMap<String, String>) -> &str {
        metadata
            .get("sampler")
            .map(String::as_str)
            .unwrap_or("unattributed")
    }

    let mut groups: BTreeMap<&str, Vec<Entry<'_>>> = BTreeMap::new();
    for c in counters {
        groups
            .entry(sampler_of(&c.metadata))
            .or_default()
            .push(Entry::Counter(c));
    }
    for g in gauges {
        groups
            .entry(sampler_of(&g.metadata))
            .or_default()
            .push(Entry::Gauge(g));
    }
    for h in histograms {
        groups
            .entry(sampler_of(&h.metadata))
            .or_default()
            .push(Entry::Histogram(h));
    }
    groups
}

/// One sampler group's dedup key: the representative acquisition window (max
/// `end_ns` among windowed metrics), or `snapshot_ts` when nothing in the group
/// carries a window (windowless → one row per poll).
pub(crate) fn dedup_key(entries: &[Entry<'_>], snapshot_ts: u64) -> u64 {
    entries
        .iter()
        .filter_map(|e| e.window())
        .map(|w| w.end_ns)
        .max()
        .unwrap_or(snapshot_ts)
}

/// Accumulates scraped snapshots into per-sampler tables, deduping by each
/// sampler's representative acquisition window.
///
/// The in-memory, atomic (whole-archive-at-finalize) writer. The recorder loop
/// no longer uses it — it streams sealed segments through
/// [`rez_stream::StreamRecorder`](super::rez_stream::StreamRecorder) so
/// finalization is bounded — but it remains the fixture builder for every
/// `.rez` test in the tree and the natural base for the deferred offline
/// compactor. Kept whole (not `#[cfg(test)]`-forked) so the archives tests
/// exercise stay the archives this crate knows how to write.
#[allow(dead_code)]
pub struct RezRecorder {
    tables: BTreeMap<String, TableBuilder>,
    metadata: BTreeMap<String, String>,
    labels: BTreeMap<String, String>,
    dir: String,
}

#[allow(dead_code)]
impl RezRecorder {
    pub fn new(
        metadata: BTreeMap<String, String>,
        labels: BTreeMap<String, String>,
        dir: String,
    ) -> Self {
        Self {
            tables: BTreeMap::new(),
            metadata,
            labels,
            dir,
        }
    }

    /// Partition `snapshot`'s metrics by their `sampler` label; for each sampler
    /// append a row iff its representative window (max `end_ns` among windowed
    /// metrics) advanced, else key on `snapshot_ts` (windowless → per-poll row).
    pub fn ingest(&mut self, snapshot: &Snapshot, snapshot_ts: u64) {
        for (sampler, entries) in group_by_sampler(snapshot) {
            let key = dedup_key(&entries, snapshot_ts);
            let table = self
                .tables
                .entry(sampler.to_string())
                .or_insert_with(|| TableBuilder::new(sampler.to_string()));
            if let Some(last) = table.last_key {
                if key <= last {
                    continue; // window unchanged → same observation → skip
                }
            }
            table.last_key = Some(key);
            // `snapshot_ts` is today's raw `SystemTime` reading, so the wall
            // observation is exactly zero. It becomes meaningful when the
            // recorder loop switches to monotonic-anchored row stamps and
            // passes its own per-tick wall reading through.
            table.push_row(snapshot_ts, 0, &entries);
        }
    }

    /// Test/inspection helper: the current (unpadded) table builder view.
    #[cfg(test)]
    pub(crate) fn table(&self, sampler: &str) -> Option<&TableBuilder> {
        self.tables.get(sampler)
    }

    /// Consume into finalized per-sampler tables.
    pub fn finalize_tables(self) -> Vec<RezTable> {
        self.tables
            .into_values()
            .map(TableBuilder::finish)
            .collect()
    }

    /// Finalize and write the single-recording `.rez` archive at `path`.
    pub fn finalize(self, path: &Path) -> Result<(), RezError> {
        let dir = self.dir.clone();
        let labels = self.labels.clone();
        let metadata = self.metadata.clone();
        let tables = self.finalize_tables();
        write_archive(
            path,
            &[RecordingData {
                dir,
                labels,
                metadata,
                tables: &tables,
            }],
        )
    }
}

/// Extract the `hostname` string from a systeminfo JSON blob, if present.
pub fn host_from_systeminfo(systeminfo_json: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(systeminfo_json)
        .ok()?
        .get("hostname")?
        .as_str()
        .map(|s| s.to_string())
}

/// The recording's label set: `source`, `host` (from systeminfo hostname, when
/// available), then user `--label k=v` applied last (last-wins, so a user
/// `--label host=...` overrides the auto value).
pub fn build_labels(
    source: &str,
    systeminfo_json: Option<&str>,
    user_labels: &[(String, String)],
) -> BTreeMap<String, String> {
    let mut labels = BTreeMap::new();
    labels.insert("source".to_string(), source.to_string());
    if let Some(host) = systeminfo_json.and_then(host_from_systeminfo) {
        labels.insert("host".to_string(), host);
    }
    for (k, v) in user_labels {
        labels.insert(k.clone(), v.clone());
    }
    labels
}

/// Filesystem-safe directory name for a single recording, derived from its
/// `source` label (falls back to `"recording"`). The manifest — not the dir —
/// is authoritative for labels; this is only a human-readable tar path.
pub fn recording_dir_slug(labels: &BTreeMap<String, String>) -> String {
    let base = labels
        .get("source")
        .map(String::as_str)
        .unwrap_or("recording");
    let slug: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if slug.is_empty() {
        "recording".to_string()
    } else {
        slug
    }
}

/// True when the recording should be written as a `.rez` archive: either the
/// output path ends in `.rez` or `--format rez` was given.
pub fn wants_rez(output: &Path, format: crate::Format) -> bool {
    format == crate::Format::Rez || output.extension().and_then(|e| e.to_str()) == Some("rez")
}

#[cfg(test)]
mod builder_tests {
    use super::*;
    use metriken_exposition::{Counter, Gauge, Histogram};
    use std::collections::HashMap;

    fn cmeta(sampler: &str) -> HashMap<String, String> {
        [("sampler".to_string(), sampler.to_string())]
            .into_iter()
            .collect()
    }

    fn hist(gp: u8, mvp: u8) -> histogram::Histogram {
        let mut h = histogram::Histogram::new(gp, mvp).unwrap();
        h.increment(1_000).unwrap();
        h
    }

    // The seal threshold is byte-first, so `push_row` must maintain the byte
    // count itself: a row's cost is dominated by histogram buckets, which row
    // counts do not see at all (one gp=7/mvp=64 cell is ~58 KB).
    #[test]
    fn push_row_accumulates_approx_bytes() {
        let mut b = TableBuilder::new("s".to_string());
        assert_eq!(b.approx_bytes(), 0, "a fresh builder accounts nothing");

        let c = Counter::new("0".to_string(), 1, cmeta("s"));
        b.push_row(1_000, 0, &[Entry::Counter(&c)]);
        let after_scalar = b.approx_bytes();
        assert_eq!(
            after_scalar, CELL_OVERHEAD_BYTES,
            "one scalar cell is its value slot plus its window slot"
        );

        let h = Histogram::new("1".to_string(), hist(7, 64), cmeta("s"));
        let buckets = h.value.as_slice().len();
        assert_eq!(buckets, h.value.config().total_buckets());
        b.push_row(2_000, 0, &[Entry::Histogram(&h)]);
        assert_eq!(
            b.approx_bytes() - after_scalar,
            CELL_OVERHEAD_BYTES + buckets * HISTOGRAM_BUCKET_BYTES,
            "one histogram cell is the per-cell overhead plus its buckets"
        );
    }

    // Regression: `approx_bytes` bounds resident memory, and it used to charge
    // a scalar cell only its value slot (16 B) while `push_row` also pushed an
    // `Option<Window>` (24 B) — a 2.5x optimistic bound. This pins the window
    // slot as counted, and fails if the per-cell charge is reverted to 16 B.
    #[test]
    fn approx_bytes_counts_the_window_slot() {
        use std::mem::size_of;
        // The constants are claims about layout; check them against the layout.
        assert_eq!(size_of::<Option<u64>>(), VALUE_SLOT_BYTES);
        assert_eq!(size_of::<Option<i64>>(), VALUE_SLOT_BYTES);
        assert_eq!(size_of::<Option<Box<[u64]>>>(), VALUE_SLOT_BYTES);
        // `Window` is two `u64`s with no niche, so the option tag costs a word.
        assert_eq!(size_of::<Option<Window>>(), WINDOW_SLOT_BYTES);

        let mut b = TableBuilder::new("s".to_string());
        let c =
            Counter::new("0".to_string(), 1, cmeta("s")).with_window(Some(Window::new(900, 1_000)));
        b.push_row(1_000, 0, &[Entry::Counter(&c)]);

        // One value slot and one window slot were pushed, so both must be paid
        // for. The strict inequality is the part that catches a revert.
        let charged = b.approx_bytes();
        assert_eq!(
            charged,
            VALUE_SLOT_BYTES + WINDOW_SLOT_BYTES,
            "a scalar cell costs 40 B, not 16 B"
        );
        assert!(
            charged > VALUE_SLOT_BYTES,
            "the window slot must be accounted, not just the value slot"
        );
        let table = b.finish();
        assert_eq!(
            table.columns[0].windows.len(),
            1,
            "push_row pushed the window slot that was charged for"
        );
    }

    // Regression: the type-mismatch arm used to skip the value but still push
    // the window, desyncing every later row's window from its value. An agent
    // restart can remap a numeric id and flip a column's shape mid-recording,
    // and segmentation would bake the skew into immutable data.
    #[test]
    fn mismatch_arm_does_not_desync_values_and_windows() {
        let mut b = TableBuilder::new("s".to_string());
        let w = |n: u64| Some(Window::new(n, n + 100));

        let c0 = Counter::new("0".to_string(), 1, cmeta("s")).with_window(w(900));
        b.push_row(1_000, 0, &[Entry::Counter(&c0)]);
        // Same column name, now a gauge: shape mismatch against the established
        // counter column.
        let g = Gauge::new("0".to_string(), -5, cmeta("s")).with_window(w(1_900));
        b.push_row(2_000, 0, &[Entry::Gauge(&g)]);
        let c1 = Counter::new("0".to_string(), 3, cmeta("s")).with_window(w(2_900));
        b.push_row(3_000, 0, &[Entry::Counter(&c1)]);

        let table = b.finish();
        assert_eq!(table.timestamps.len(), 3);
        for col in &table.columns {
            assert_eq!(
                col.windows.len(),
                TableBuilder::col_len(col),
                "column {} desynced: {} windows vs {} values",
                col.name,
                col.windows.len(),
                TableBuilder::col_len(col)
            );
        }
        // The mismatched row is a null, and the rows around it keep their own
        // windows (row 2's window is not shifted onto row 3).
        match &table.columns[0].values {
            RezValues::Counter(v) => assert_eq!(v, &vec![Some(1), None, Some(3)]),
            other => panic!("expected counter column, got {other:?}"),
        }
        assert_eq!(
            table.columns[0].windows,
            vec![
                Some(Window::new(900, 1_000)),
                None,
                Some(Window::new(2_900, 3_000))
            ]
        );
    }

    // The raw wall-clock reading rides along as a table-level sidecar column so
    // an NTP step locates to the exact tick; the query engine skips it.
    #[test]
    fn wall_offsets_roundtrip_through_parquet() {
        let mut b = TableBuilder::new("cpu_usage".to_string());
        let c = Counter::new("0".to_string(), 7, cmeta("cpu_usage"));
        b.push_row(1_000, -123_456, &[Entry::Counter(&c)]);
        let table = b.finish();
        assert_eq!(table.wall_offsets, vec![-123_456]);

        let (schema, _) = table_to_batch(&table).unwrap();
        let field = schema
            .field_with_name(":wall_offset")
            .expect(":wall_offset field present");
        assert_eq!(field.data_type(), &DataType::Int64);

        let bytes = write_table_parquet(&table).unwrap();
        let back = read_table_parquet("cpu_usage".to_string(), bytes).unwrap();
        assert_eq!(back.wall_offsets, vec![-123_456]);
        assert_eq!(back.timestamps, vec![1_000]);
    }
}

#[cfg(test)]
pub(crate) mod recorder_tests_support {
    pub use super::recorder_tests::{counter, snap};
}

#[cfg(test)]
mod recorder_tests {
    use super::*;
    use metriken::Window;
    use metriken_exposition::{Counter, Snapshot, SnapshotV2};
    use std::collections::HashMap;
    use std::time::SystemTime;

    fn cmeta(metric: &str, sampler: &str) -> HashMap<String, String> {
        [
            ("metric".to_string(), metric.to_string()),
            ("sampler".to_string(), sampler.to_string()),
        ]
        .into_iter()
        .collect()
    }

    pub fn snap(ts: u64, counters: Vec<Counter>) -> Snapshot {
        Snapshot::V2(SnapshotV2 {
            systemtime: SystemTime::UNIX_EPOCH + std::time::Duration::from_nanos(ts),
            duration: std::time::Duration::ZERO,
            metadata: HashMap::new(),
            counters,
            gauges: Vec::new(),
            histograms: Vec::new(),
        })
    }

    pub fn counter(name: &str, sampler: &str, value: u64, window: Option<Window>) -> Counter {
        Counter::new(name.to_string(), value, cmeta(name, sampler)).with_window(window)
    }

    #[test]
    fn windowless_sampler_writes_one_row_per_poll() {
        let mut r = RezRecorder::new(BTreeMap::new(), BTreeMap::new(), "test".to_string());
        for i in 0..3u64 {
            let ts = 1_000 + i;
            r.ingest(&snap(ts, vec![counter("0", "cpu_perf", i, None)]), ts);
        }
        let t = r.table("cpu_perf").unwrap();
        assert_eq!(t.timestamps.len(), 3);
    }

    #[test]
    fn unchanged_window_dedups_to_one_row() {
        let mut r = RezRecorder::new(BTreeMap::new(), BTreeMap::new(), "test".to_string());
        let w = Window::new(900, 1_000);
        for i in 0..3u64 {
            r.ingest(
                &snap(1_000 + i, vec![counter("0", "drivehealth", 5, Some(w))]),
                1_000 + i,
            );
        }
        assert_eq!(r.table("drivehealth").unwrap().timestamps.len(), 1);
    }

    #[test]
    fn advancing_window_writes_one_row_per_advance() {
        let mut r = RezRecorder::new(BTreeMap::new(), BTreeMap::new(), "test".to_string());
        for i in 0..3u64 {
            let end = 1_000 + i * 100;
            r.ingest(
                &snap(
                    2_000 + i,
                    vec![counter(
                        "0",
                        "cpu_usage",
                        i,
                        Some(Window::new(end - 50, end)),
                    )],
                ),
                2_000 + i,
            );
        }
        assert_eq!(r.table("cpu_usage").unwrap().timestamps.len(), 3);
    }

    #[test]
    fn mixed_sampler_advances_on_windowed_and_carries_packed_column() {
        let mut r = RezRecorder::new(BTreeMap::new(), BTreeMap::new(), "test".to_string());
        // metric "0" windowed (advances), metric "1" packed/windowless, same sampler.
        for i in 0..2u64 {
            let end = 1_000 + i * 100;
            r.ingest(
                &snap(
                    3_000 + i,
                    vec![
                        counter("0", "cpu_usage", i, Some(Window::new(end - 50, end))),
                        counter("1", "cpu_usage", 42 + i, None),
                    ],
                ),
                3_000 + i,
            );
        }
        let t = r.table("cpu_usage").unwrap();
        assert_eq!(t.timestamps.len(), 2);
        let packed = t
            .columns
            .values()
            .find(|c| c.name == "1")
            .expect("packed column present");
        match &packed.values {
            RezValues::Counter(v) => assert_eq!(v, &vec![Some(42), Some(43)]),
            _ => panic!("expected counter"),
        }
    }

    #[test]
    fn two_samplers_split_into_two_tables() {
        let mut r = RezRecorder::new(BTreeMap::new(), BTreeMap::new(), "test".to_string());
        r.ingest(
            &snap(
                1_000,
                vec![
                    counter("0", "cpu_usage", 1, Some(Window::new(900, 1_000))),
                    counter("9", "blockio_latency", 2, Some(Window::new(900, 1_000))),
                ],
            ),
            1_000,
        );
        let tables = r.finalize_tables();
        assert_eq!(tables.len(), 2);
        assert!(tables.iter().any(|t| t.sampler == "cpu_usage"));
        assert!(tables.iter().any(|t| t.sampler == "blockio_latency"));
    }
}

#[cfg(test)]
mod manifest_tests {
    use super::*;

    // A v1 manifest (single `file` per table, no `complete`/clock fields) must
    // still parse: v2 only adds fields, and `file` stays readable as a
    // one-element segment list.
    #[test]
    fn v1_manifest_json_still_parses() {
        let v1 = r#"{"version":1,"recordings":[{"dir":"rezolus","labels":{},
        "metadata":{},"tables":[{"sampler":"cpu_usage","file":"cpu_usage.parquet",
        "columns":["5"],"rows":3,"cadence_ns":1000000000}]}]}"#;
        let m: RezManifest = serde_json::from_str(v1).unwrap();
        let t = &m.recordings[0].tables[0];
        assert_eq!(t.segment_files(), vec!["cpu_usage.parquet"]);
        assert!(!m.recordings[0].complete); // absent -> false
    }

    #[test]
    fn v2_roundtrip_files_complete_clock() {
        let m = RezManifest {
            version: REZ_SCHEMA_VERSION,
            recordings: vec![RezRecording {
                dir: "rezolus".to_string(),
                labels: BTreeMap::new(),
                metadata: BTreeMap::new(),
                complete: true,
                clock_anchor_wall_ns: Some(1_700_000_000_000_000_000),
                clock_offsets: vec![(1_700_000_001_000_000_000, -2_500_000)],
                tables: vec![RezTableIndex {
                    sampler: "cpu_usage".to_string(),
                    file: None,
                    files: vec![
                        "cpu_usage/0.parquet".to_string(),
                        "cpu_usage/1.parquet".to_string(),
                    ],
                    columns: vec!["5".to_string()],
                    rows: 7,
                    cadence_ns: Some(1_000_000_000),
                }],
            }],
        };
        let json = serde_json::to_string(&m).unwrap();
        // A multi-segment table never serializes a null `file` (a v1 reader
        // would take `null` as a present-but-broken path rather than absent).
        assert!(!json.contains("\"file\":null"), "{json}");
        let back: RezManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
        assert_eq!(
            back.recordings[0].tables[0].segment_files(),
            vec!["cpu_usage/0.parquet", "cpu_usage/1.parquet"]
        );
    }

    #[test]
    fn manifest_json_round_trips() {
        let m = RezManifest {
            version: REZ_SCHEMA_VERSION,
            recordings: vec![RezRecording {
                dir: "rezolus".to_string(),
                labels: [
                    ("source".to_string(), "rezolus".to_string()),
                    ("host".to_string(), "node0".to_string()),
                ]
                .into_iter()
                .collect(),
                metadata: [("sampling_interval_ms".to_string(), "1000".to_string())]
                    .into_iter()
                    .collect(),
                complete: true,
                clock_anchor_wall_ns: None,
                clock_offsets: Vec::new(),
                tables: vec![RezTableIndex {
                    sampler: "cpu_usage".to_string(),
                    file: Some("cpu_usage.parquet".to_string()),
                    files: vec!["cpu_usage.parquet".to_string()],
                    columns: vec!["5".to_string()],
                    rows: 3,
                    cadence_ns: Some(1_000_000_000),
                }],
            }],
        };
        let bytes = serde_json::to_vec(&m).unwrap();
        let back: RezManifest = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(m, back);
        assert_eq!(REZ_MANIFEST_NAME, "manifest.json");
    }
}

#[cfg(test)]
mod table_tests {
    use super::*;
    use metriken::Window;
    use std::collections::HashMap;

    fn meta(metric: &str, mtype: &str) -> HashMap<String, String> {
        [
            ("metric".to_string(), metric.to_string()),
            ("sampler".to_string(), "s".to_string()),
            ("metric_type".to_string(), mtype.to_string()),
        ]
        .into_iter()
        .collect()
    }

    #[test]
    fn single_table_parquet_round_trips_values_and_windows() {
        let ts = vec![1_000u64, 2_000u64];
        let table = RezTable {
            sampler: "s".to_string(),
            timestamps: ts.clone(),
            wall_offsets: Vec::new(),
            columns: vec![
                RezColumn {
                    name: "0".to_string(),
                    metadata: meta("c", "counter"),
                    values: RezValues::Counter(vec![Some(10), Some(20)]),
                    windows: vec![
                        Some(Window::new(900, 1_000)),
                        Some(Window::new(1_900, 2_050)),
                    ],
                },
                RezColumn {
                    name: "1".to_string(),
                    metadata: meta("g", "gauge"),
                    values: RezValues::Gauge(vec![Some(-5), None]),
                    windows: vec![None, None],
                },
                RezColumn {
                    name: "2".to_string(),
                    metadata: {
                        let mut m = meta("h", "histogram");
                        m.insert("grouping_power".to_string(), "1".to_string());
                        m.insert("max_value_power".to_string(), "3".to_string());
                        m
                    },
                    values: RezValues::Histogram(vec![
                        Some(
                            histogram::Histogram::from_buckets(1, 3, vec![0, 1, 1, 0, 0, 0])
                                .unwrap(),
                        ),
                        None,
                    ]),
                    windows: vec![Some(Window::new(800, 1_000)), None],
                },
            ],
        };

        let bytes = write_table_parquet(&table).unwrap();
        let back = read_table_parquet("s".to_string(), bytes).unwrap();

        assert_eq!(back.timestamps, ts);
        assert_eq!(back.columns.len(), 3);
        // counter values + per-row windows preserved
        match &back.columns[0].values {
            RezValues::Counter(v) => assert_eq!(v, &vec![Some(10), Some(20)]),
            _ => panic!("expected counter"),
        }
        assert_eq!(
            back.columns[0].windows,
            vec![
                Some(Window::new(900, 1_000)),
                Some(Window::new(1_900, 2_050))
            ]
        );
        // gauge nulls + null windows preserved
        match &back.columns[1].values {
            RezValues::Gauge(v) => assert_eq!(v, &vec![Some(-5), None]),
            _ => panic!("expected gauge"),
        }
        assert_eq!(back.columns[1].windows, vec![None, None]);
        // histogram buckets preserved
        match &back.columns[2].values {
            RezValues::Histogram(v) => {
                assert_eq!(v[0].as_ref().unwrap().as_slice(), &[0, 1, 1, 0, 0, 0]);
                assert!(v[1].is_none());
            }
            _ => panic!("expected histogram"),
        }
    }
}

#[cfg(test)]
mod archive_tests {
    use super::*;
    use metriken::Window;
    use std::collections::HashMap;

    fn counter_col(name: &str, vals: Vec<Option<u64>>, wins: Vec<Option<Window>>) -> RezColumn {
        RezColumn {
            name: name.to_string(),
            metadata: [
                ("metric".to_string(), name.to_string()),
                ("metric_type".to_string(), "counter".to_string()),
            ]
            .into_iter()
            .collect::<HashMap<_, _>>(),
            values: RezValues::Counter(vals),
            windows: wins,
        }
    }

    #[test]
    fn read_archive_bytes_returns_manifest_and_per_table_bytes() {
        let t = RezTable {
            sampler: "cpu_usage".to_string(),
            timestamps: vec![1_000, 2_000],
            wall_offsets: Vec::new(),
            columns: vec![counter_col("0", vec![Some(1), Some(2)], vec![None, None])],
        };
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("r.rez");
        let tables = [t];
        write_archive(
            &out,
            &[RecordingData {
                dir: "rezolus".to_string(),
                labels: [("source".to_string(), "rezolus".to_string())]
                    .into_iter()
                    .collect(),
                metadata: BTreeMap::new(),
                tables: &tables,
            }],
        )
        .unwrap();

        let (manifest, recordings) = read_archive_bytes(&out).unwrap();
        assert_eq!(manifest.recordings.len(), 1);
        assert_eq!(recordings.len(), 1);
        assert_eq!(recordings[0].dir, "rezolus");
        assert_eq!(recordings[0].tables.len(), 1);
        let (sampler, segments) = &recordings[0].tables[0];
        assert_eq!(sampler, "cpu_usage");
        assert_eq!(segments.len(), 1);
        let bytes = &segments[0];
        assert_eq!(&bytes[..4], b"PAR1");
        assert_eq!(&bytes[bytes.len() - 4..], b"PAR1");
        // Written whole, so the recording is complete by construction.
        assert!(recordings[0].complete);
    }

    #[test]
    fn write_archive_bytes_round_trips_multiple_recordings() {
        let mk = |sampler: &str, arm: &str| -> (tempfile::TempDir, std::path::PathBuf) {
            let t = RezTable {
                sampler: sampler.to_string(),
                timestamps: vec![1_000, 2_000],
                wall_offsets: Vec::new(),
                columns: vec![counter_col("0", vec![Some(1), Some(2)], vec![None, None])],
            };
            let d = tempfile::tempdir().unwrap();
            let p = d.path().join("one.rez");
            let tables = [t];
            write_archive(
                &p,
                &[RecordingData {
                    dir: "rezolus".to_string(),
                    labels: [("arm".to_string(), arm.to_string())].into_iter().collect(),
                    metadata: BTreeMap::new(),
                    tables: &tables,
                }],
            )
            .unwrap();
            (d, p)
        };
        let (_da, a) = mk("cpu_usage", "redis");
        let (_db, b) = mk("cpu_usage", "valkey");

        let mut recs: Vec<RecordingSegments> = Vec::new();
        for (i, p) in [a, b].iter().enumerate() {
            let (m, rb) = read_archive_bytes(p).unwrap();
            let mut rec = m.recordings.into_iter().next().unwrap();
            rec.dir = format!("rec{i}");
            let bytes = rb
                .into_iter()
                .next()
                .unwrap()
                .tables
                .into_iter()
                .map(|(_, b)| b)
                .collect();
            recs.push((rec, bytes));
        }

        let outdir = tempfile::tempdir().unwrap();
        let out = outdir.path().join("ab.rez");
        write_archive_bytes(&out, &recs).unwrap();

        let (m, rb) = read_archive_bytes(&out).unwrap();
        assert_eq!(m.recordings.len(), 2);
        assert_eq!(m.recordings[0].dir, "rec0");
        assert_eq!(m.recordings[1].dir, "rec1");
        assert_eq!(
            m.recordings[0].labels.get("arm").map(String::as_str),
            Some("redis")
        );
        assert_eq!(
            m.recordings[1].labels.get("arm").map(String::as_str),
            Some("valkey")
        );
        assert_eq!(rb.len(), 2);
        assert_eq!(rb[0].tables[0].0, "cpu_usage");
    }

    /// The tools carry the input's `RezTableIndex` verbatim, so on segmented
    /// input a stale index would name files the output never emits. The writer
    /// owns the naming: it rewrites each entry from the blobs it is handed —
    /// without parsing them (the blobs here are not parquet at all).
    #[test]
    fn write_archive_bytes_canonicalizes_index() {
        let lying_index = |sampler: &str| RezTableIndex {
            sampler: sampler.to_string(),
            file: Some("old.parquet".to_string()),
            files: vec!["stale/0.parquet".to_string()],
            columns: vec!["0".to_string()],
            rows: 2,
            cadence_ns: Some(1_000),
        };
        let rec = RezRecording {
            dir: "rec0".to_string(),
            labels: [("arm".to_string(), "redis".to_string())]
                .into_iter()
                .collect(),
            metadata: BTreeMap::new(),
            complete: true,
            clock_anchor_wall_ns: Some(1_700_000_000_000_000_000),
            clock_offsets: vec![(1_000, -7)],
            tables: vec![lying_index("cpu_usage"), lying_index("scheduler")],
        };
        let seg = |s: &str| s.as_bytes().to_vec();
        let blobs = vec![
            vec![seg("cpu-segment-0"), seg("cpu-segment-1")],
            vec![seg("scheduler-only-segment")],
        ];

        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("canon.rez");
        write_archive_bytes(&out, &[(rec, blobs)]).unwrap();

        let (m, rb) = read_archive_bytes(&out).unwrap();
        let tables = &m.recordings[0].tables;
        // Multi-segment: no v1 `file`, segment list in emission order.
        assert_eq!(tables[0].file, None);
        assert_eq!(
            tables[0].files,
            vec!["cpu_usage/0000.parquet", "cpu_usage/0001.parquet"]
        );
        // Single-segment: v1-shaped, so v1 binaries can still open it.
        assert_eq!(tables[1].file.as_deref(), Some("scheduler.parquet"));
        assert_eq!(tables[1].files, vec!["scheduler.parquet"]);
        // Everything else on the entry and the recording copies through.
        assert_eq!(tables[0].columns, vec!["0".to_string()]);
        assert_eq!(tables[0].rows, 2);
        assert!(m.recordings[0].complete);
        assert_eq!(
            m.recordings[0].clock_anchor_wall_ns,
            Some(1_700_000_000_000_000_000)
        );
        assert_eq!(m.recordings[0].clock_offsets, vec![(1_000, -7)]);
        // Segments pass through byte-identical, in order.
        assert_eq!(rb[0].tables[0].0, "cpu_usage");
        assert_eq!(
            rb[0].tables[0].1,
            vec![seg("cpu-segment-0"), seg("cpu-segment-1")]
        );
        assert_eq!(rb[0].tables[1].0, "scheduler");
        assert_eq!(rb[0].tables[1].1, vec![seg("scheduler-only-segment")]);
    }

    #[test]
    fn is_rez_distinguishes_rez_from_bare_parquet() {
        let t = RezTable {
            sampler: "cpu_usage".to_string(),
            timestamps: vec![1_000],
            wall_offsets: Vec::new(),
            columns: vec![counter_col("0", vec![Some(1)], vec![None])],
        };
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("r.rez");
        let tables = [t.clone()];
        write_archive(
            &out,
            &[RecordingData {
                dir: "rezolus".to_string(),
                labels: BTreeMap::new(),
                metadata: BTreeMap::new(),
                tables: &tables,
            }],
        )
        .unwrap();
        assert!(is_rez_path(&out).unwrap());

        let bare = dir.path().join("plain.parquet");
        std::fs::write(&bare, write_table_parquet(&t).unwrap()).unwrap();
        assert!(!is_rez_path(&bare).unwrap());
    }

    #[test]
    fn archive_round_trips_multiple_tables() {
        let a = RezTable {
            sampler: "cpu_usage".to_string(),
            timestamps: vec![1_000, 2_000],
            wall_offsets: Vec::new(),
            columns: vec![counter_col(
                "0",
                vec![Some(1), Some(2)],
                vec![
                    Some(Window::new(500, 1_000)),
                    Some(Window::new(1_400, 2_000)),
                ],
            )],
        };
        let b = RezTable {
            sampler: "blockio_latency".to_string(),
            timestamps: vec![1_000],
            wall_offsets: Vec::new(),
            columns: vec![counter_col("9", vec![Some(7)], vec![None])],
        };
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("rec.rez");
        let labels: BTreeMap<String, String> = [
            ("source".to_string(), "rezolus".to_string()),
            ("host".to_string(), "node0".to_string()),
        ]
        .into_iter()
        .collect();
        let metadata: BTreeMap<String, String> =
            [("sampling_interval_ms".to_string(), "1000".to_string())]
                .into_iter()
                .collect();

        let tables = [a.clone(), b.clone()];
        write_archive(
            &out,
            &[RecordingData {
                dir: "rezolus".to_string(),
                labels: labels.clone(),
                metadata: metadata.clone(),
                tables: &tables,
            }],
        )
        .unwrap();
        let archive = read_archive(&out).unwrap();

        assert_eq!(archive.manifest.version, REZ_SCHEMA_VERSION);
        assert_eq!(archive.manifest.recordings.len(), 1);
        let rec = &archive.manifest.recordings[0];
        assert_eq!(rec.dir, "rezolus");
        assert_eq!(rec.labels, labels);
        assert_eq!(rec.metadata, metadata);
        assert_eq!(rec.tables.len(), 2);
        assert_eq!(archive.tables.len(), 1);
        assert_eq!(archive.tables[0].len(), 2);

        let cpu = archive.tables[0]
            .iter()
            .find(|t| t.sampler == "cpu_usage")
            .unwrap();
        assert_eq!(cpu.timestamps, vec![1_000, 2_000]);
        assert_eq!(
            cpu.columns[0].windows,
            vec![
                Some(Window::new(500, 1_000)),
                Some(Window::new(1_400, 2_000))
            ]
        );
        let bio = archive.tables[0]
            .iter()
            .find(|t| t.sampler == "blockio_latency")
            .unwrap();
        assert_eq!(bio.timestamps, vec![1_000]);
        assert_eq!(bio.columns[0].windows, vec![None]);

        let cpu_idx = rec
            .tables
            .iter()
            .find(|t| t.sampler == "cpu_usage")
            .unwrap();
        assert_eq!(cpu_idx.segment_files(), vec!["cpu_usage.parquet"]);
        assert_eq!(cpu_idx.rows, 2);
        assert_eq!(cpu_idx.cadence_ns, Some(1_000));
    }

    // Two recordings with distinct dirs must round-trip independently: the tar
    // nests each under its own <dir>/, and read_archive returns tables parallel
    // to manifest.recordings order. This is the multi-recording path Phase C
    // (`parquet combine`) will exercise; the writer/reader already support it.
    #[test]
    fn archive_round_trips_multiple_recordings() {
        let baseline = RezTable {
            sampler: "cpu_usage".to_string(),
            timestamps: vec![1_000, 2_000],
            wall_offsets: Vec::new(),
            columns: vec![counter_col("0", vec![Some(1), Some(2)], vec![None, None])],
        };
        let experiment = RezTable {
            sampler: "cpu_usage".to_string(),
            timestamps: vec![1_000, 2_000],
            wall_offsets: Vec::new(),
            columns: vec![counter_col("0", vec![Some(10), Some(20)], vec![None, None])],
        };
        let labels = |arm: &str| -> BTreeMap<String, String> {
            [
                ("source".to_string(), "rezolus".to_string()),
                ("arm".to_string(), arm.to_string()),
            ]
            .into_iter()
            .collect()
        };
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("ab.rez");
        let base_tables = [baseline];
        let exp_tables = [experiment];

        write_archive(
            &out,
            &[
                RecordingData {
                    dir: "redis".to_string(),
                    labels: labels("redis"),
                    metadata: BTreeMap::new(),
                    tables: &base_tables,
                },
                RecordingData {
                    dir: "valkey".to_string(),
                    labels: labels("valkey"),
                    metadata: BTreeMap::new(),
                    tables: &exp_tables,
                },
            ],
        )
        .unwrap();
        let archive = read_archive(&out).unwrap();

        // Two recordings, distinct dirs, tables parallel to recordings order.
        assert_eq!(archive.manifest.recordings.len(), 2);
        assert_eq!(archive.tables.len(), 2);
        assert_eq!(archive.manifest.recordings[0].dir, "redis");
        assert_eq!(archive.manifest.recordings[1].dir, "valkey");
        assert_eq!(
            archive.manifest.recordings[0]
                .labels
                .get("arm")
                .map(String::as_str),
            Some("redis")
        );
        assert_eq!(
            archive.manifest.recordings[1]
                .labels
                .get("arm")
                .map(String::as_str),
            Some("valkey")
        );
        // Same sampler name in both recordings resolves to each recording's own
        // values (no cross-recording clobber despite the shared file basename).
        match &archive.tables[0][0].columns[0].values {
            RezValues::Counter(v) => assert_eq!(v, &vec![Some(1), Some(2)]),
            _ => panic!("expected counter"),
        }
        match &archive.tables[1][0].columns[0].values {
            RezValues::Counter(v) => assert_eq!(v, &vec![Some(10), Some(20)]),
            _ => panic!("expected counter"),
        }
    }

    fn write_minimal_v2_archive(path: &Path) {
        let t = RezTable {
            sampler: "cpu_usage".to_string(),
            timestamps: vec![1_000, 2_000],
            wall_offsets: Vec::new(),
            columns: vec![counter_col("0", vec![Some(1), Some(2)], vec![None, None])],
        };
        let tables = [t];
        write_archive(
            path,
            &[RecordingData {
                dir: "rezolus".to_string(),
                labels: [("source".to_string(), "rezolus".to_string())]
                    .into_iter()
                    .collect(),
                metadata: BTreeMap::new(),
                tables: &tables,
            }],
        )
        .unwrap();
    }

    #[test]
    fn detects_v3_sqlite_v2_tar_and_neither() {
        let dir = tempfile::tempdir().unwrap();

        // v3: a SQLite database created by the v3 container module.
        let v3 = dir.path().join("v3.rez");
        drop(crate::recorder::rez_sqlite::RezDb::create(&v3).unwrap());
        assert_eq!(detect_rez_format(&v3).unwrap(), RezFormat::V3Sqlite);

        // v2: an actual tar archive, built the way the other tests here do.
        let v2 = dir.path().join("v2.rez");
        write_minimal_v2_archive(&v2);
        assert_eq!(detect_rez_format(&v2).unwrap(), RezFormat::V2Tar);

        // Neither: a bare parquet file.
        let pq = dir.path().join("x.parquet");
        std::fs::write(&pq, b"PAR1").unwrap();
        assert_eq!(detect_rez_format(&pq).unwrap(), RezFormat::NotRez);

        // Neither: a file too short to hold the SQLite magic. This must not panic
        // or error — it falls through to the tar sniff and comes back NotRez.
        let tiny = dir.path().join("tiny");
        std::fs::write(&tiny, b"hi").unwrap();
        assert_eq!(detect_rez_format(&tiny).unwrap(), RezFormat::NotRez);
    }

    #[test]
    fn detection_does_not_change_is_rez_path() {
        // Every existing consumer still calls is_rez_path; adding the v3 detector
        // must not alter what it reports for a v2 archive or a non-archive.
        let dir = tempfile::tempdir().unwrap();
        let v2 = dir.path().join("v2.rez");
        write_minimal_v2_archive(&v2);
        assert!(is_rez_path(&v2).unwrap());

        let v3 = dir.path().join("v3.rez");
        drop(crate::recorder::rez_sqlite::RezDb::create(&v3).unwrap());
        // A v3 file is NOT a tar, so the legacy sniff correctly says false.
        // This is why callers must migrate to detect_rez_format rather than
        // having is_rez_path silently start returning true for SQLite files.
        assert!(!is_rez_path(&v3).unwrap());
    }
}

/// Truncation tolerance and manifest recovery. These build tars by hand
/// because the geometry of the cut is the thing under test: the `tar` crate
/// reports each failure mode differently (mid-data → a silently short entry,
/// mid-header → an error, block boundary → clean EOF).
#[cfg(test)]
mod recovery_tests {
    use super::*;
    use std::io::Cursor;

    const SEG_A: &[u8] = b"segment-a-parquet-bytes";
    const SEG_B: &[u8] = b"segment-b-parquet-bytes-which-are-longer";

    fn tar_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        for (name, bytes) in entries {
            let mut h = tar::Header::new_gnu();
            h.set_path(name).unwrap();
            h.set_size(bytes.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            b.append(&h, *bytes).unwrap();
        }
        b.into_inner().unwrap()
    }

    /// Offset just past `entries`, i.e. where the next entry's header starts
    /// (the tar minus its 1024-byte footer).
    fn tar_prefix_len(entries: &[(&str, &[u8])]) -> usize {
        tar_bytes(entries).len() - 1024
    }

    /// A manifest naming `files` (relative to the recording dir) as segments of
    /// the one `cpu_usage` table. `complete` marks a finalize manifest.
    fn manifest(files: &[&str], complete: bool) -> Vec<u8> {
        let tables = if files.is_empty() {
            Vec::new()
        } else {
            vec![RezTableIndex {
                sampler: "cpu_usage".to_string(),
                file: None,
                files: files.iter().map(|f| f.to_string()).collect(),
                columns: Vec::new(),
                rows: files.len() as u64,
                cadence_ns: None,
            }]
        };
        serde_json::to_vec(&RezManifest {
            version: REZ_SCHEMA_VERSION,
            recordings: vec![RezRecording {
                dir: "rezolus".to_string(),
                labels: BTreeMap::new(),
                metadata: BTreeMap::new(),
                complete,
                clock_anchor_wall_ns: Some(1_700_000_000_000_000_000),
                clock_offsets: Vec::new(),
                tables,
            }],
        })
        .unwrap()
    }

    /// The shape a streaming writer produces: an initial empty manifest, then
    /// alternating segments and checkpoint manifests, then the final one.
    fn checkpointed_entries() -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        (
            manifest(&[], false),
            manifest(&["cpu_usage/0.parquet"], false),
            manifest(&["cpu_usage/0.parquet", "cpu_usage/1.parquet"], true),
        )
    }

    fn read(bytes: &[u8]) -> Result<(RezManifest, Vec<RecordingBytes>), RezError> {
        read_archive_reader(Cursor::new(bytes.to_vec()))
    }

    /// `read` expecting failure (`RecordingBytes` holds blobs, so it is
    /// deliberately not `Debug`/`unwrap_err`-able).
    fn read_err(bytes: &[u8]) -> String {
        match read(bytes) {
            Ok(_) => panic!("expected the archive to fail to open"),
            Err(e) => e.to_string(),
        }
    }

    /// Asserts the archive recovered to the checkpoint that knows only segment A.
    fn assert_recovered_to_first_segment(bytes: &[u8]) {
        let (manifest, recs) = read(bytes).expect("truncated archive recovers");
        assert!(
            !manifest.recordings[0].complete,
            "a checkpoint manifest is never complete"
        );
        assert_eq!(recs.len(), 1);
        assert!(!recs[0].complete);
        assert_eq!(recs[0].tables.len(), 1);
        assert_eq!(recs[0].tables[0].0, "cpu_usage");
        assert_eq!(recs[0].tables[0].1, vec![SEG_A.to_vec()]);
    }

    #[test]
    fn multi_segment_tables_resolve_in_order() {
        let (m0, m1, m2) = checkpointed_entries();
        let tar = tar_bytes(&[
            (REZ_MANIFEST_NAME, &m0),
            ("rezolus/cpu_usage/0.parquet", SEG_A),
            (REZ_MANIFEST_NAME, &m1),
            ("rezolus/cpu_usage/1.parquet", SEG_B),
            (REZ_MANIFEST_NAME, &m2),
        ]);
        let (manifest, recs) = read(&tar).unwrap();
        assert!(manifest.recordings[0].complete);
        assert_eq!(recs.len(), 1);
        assert!(recs[0].complete);
        assert_eq!(recs[0].tables.len(), 1);
        assert_eq!(recs[0].tables[0].0, "cpu_usage");
        // Segment order is manifest order, not tar order.
        assert_eq!(
            recs[0].tables[0].1,
            vec![SEG_A.to_vec(), SEG_B.to_vec()],
            "segments resolve in manifest order"
        );
    }

    /// A hand-built v1 manifest: `version: 1`, a single `file` per table, and
    /// no `complete` field at all (it did not exist yet).
    fn v1_manifest() -> Vec<u8> {
        br#"{
            "version": 1,
            "recordings": [{
                "dir": "rezolus",
                "labels": {},
                "metadata": {},
                "tables": [{
                    "sampler": "cpu_usage",
                    "file": "cpu_usage.parquet",
                    "columns": [],
                    "rows": 1,
                    "cadence_ns": null
                }]
            }]
        }"#
        .to_vec()
    }

    fn v1_archive() -> Vec<u8> {
        let m = v1_manifest();
        tar_bytes(&[
            (REZ_MANIFEST_NAME, &m),
            ("rezolus/cpu_usage.parquet", SEG_A),
        ])
    }

    /// v1 writers emitted the whole archive at once, so a v1 recording is
    /// complete by construction and the absent flag must not be interpreted.
    /// Normalizing at the read boundary — on the returned `RezManifest`, not
    /// just on `RecordingBytes` — is what keeps the tools honest: they copy
    /// `RezRecording.complete` verbatim into an output stamped `version: 2`.
    #[test]
    fn v1_manifest_reads_as_complete() {
        let (manifest, recs) = read(&v1_archive()).unwrap();
        assert_eq!(manifest.version, 1);
        assert!(
            manifest.recordings[0].complete,
            "a v1 recording is complete by construction"
        );
        assert!(recs[0].complete);
    }

    /// End to end: a v1 archive through the read boundary and back out of
    /// `write_archive_bytes` stays complete, rather than becoming a v2-stamped
    /// archive that reads as "not cleanly finalized".
    #[test]
    fn v1_archive_through_write_archive_bytes_stays_complete() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("v1.rez");
        std::fs::write(&src, v1_archive()).unwrap();

        let (manifest, recs) = read_archive_bytes(&src).unwrap();
        let out_recs: Vec<RecordingSegments> = manifest
            .recordings
            .into_iter()
            .zip(recs)
            .map(|(rec, rb)| (rec, rb.tables.into_iter().map(|(_, b)| b).collect()))
            .collect();
        let out = dir.path().join("v2.rez");
        write_archive_bytes(&out, &out_recs).unwrap();

        let (manifest, recs) = read_archive_bytes(&out).unwrap();
        assert_eq!(manifest.version, REZ_SCHEMA_VERSION);
        assert!(manifest.recordings[0].complete);
        assert!(recs[0].complete);
    }

    #[test]
    fn version_gate_rejects_newer() {
        let tar = tar_bytes(&[(REZ_MANIFEST_NAME, br#"{"version":3,"recordings":[]}"#)]);
        let err = read_err(&tar);
        assert!(err.contains('3') && err.contains('2'), "{err}");
    }

    // The subtle one, and the only geometry where the read-length check is what
    // decides the outcome: a manifest that *precedes* the data it names (how
    // `write_archive`/`write_archive_bytes` lay an archive out) can reference a
    // segment the truncation ate. `tar` yields that entry `Ok` with a silently
    // short buffer, so without the length check M2 would "resolve" against a
    // corrupt stub instead of falling back to M1.
    #[test]
    fn truncated_mid_data_falls_back_to_previous_manifest() {
        let m1 = manifest(&["cpu_usage/0.parquet"], false);
        let m2 = manifest(&["cpu_usage/0.parquet", "cpu_usage/1.parquet"], true);
        let entries: Vec<(&str, &[u8])> = vec![
            (REZ_MANIFEST_NAME, &m1),
            ("rezolus/cpu_usage/0.parquet", SEG_A),
            (REZ_MANIFEST_NAME, &m2),
            ("rezolus/cpu_usage/1.parquet", SEG_B),
        ];
        let full = tar_bytes(&entries);
        let seg_b_data = tar_prefix_len(&entries[..3]) + 512;
        assert_recovered_to_first_segment(&full[..seg_b_data + SEG_B.len() - 2]);
    }

    #[test]
    fn truncated_mid_header_recovers() {
        let (m0, m1, m2) = checkpointed_entries();
        let entries: Vec<(&str, &[u8])> = vec![
            (REZ_MANIFEST_NAME, &m0),
            ("rezolus/cpu_usage/0.parquet", SEG_A),
            (REZ_MANIFEST_NAME, &m1),
            ("rezolus/cpu_usage/1.parquet", SEG_B),
            (REZ_MANIFEST_NAME, &m2),
        ];
        let full = tar_bytes(&entries);
        assert_recovered_to_first_segment(&full[..tar_prefix_len(&entries[..3]) + 100]);
    }

    #[test]
    fn truncated_mid_manifest_falls_back() {
        let (m0, m1, m2) = checkpointed_entries();
        let entries: Vec<(&str, &[u8])> = vec![
            (REZ_MANIFEST_NAME, &m0),
            ("rezolus/cpu_usage/0.parquet", SEG_A),
            (REZ_MANIFEST_NAME, &m1),
            ("rezolus/cpu_usage/1.parquet", SEG_B),
            (REZ_MANIFEST_NAME, &m2),
        ];
        let full = tar_bytes(&entries);
        let m2_data = tar_prefix_len(&entries[..4]) + 512;
        // What this pins is the geometry: segment B is fully present, but the
        // manifest naming it is not readable, so recovery must fall back to the
        // previous checkpoint's view rather than to segment B. It does *not*
        // isolate the read-length check — ablating that check leaves this test
        // passing, because a truncated JSON tail is independently unparseable
        // and the parse-failure branch catches it. Only
        // `truncated_mid_data_falls_back_to_previous_manifest` fails without it.
        assert_recovered_to_first_segment(&full[..m2_data + m2.len() - 3]);
    }

    // A full-length manifest entry whose JSON is garbage (the parse-failure
    // branch, as opposed to the short-read branch above).
    #[test]
    fn unparseable_tail_manifest_falls_back() {
        let (m0, m1, _) = checkpointed_entries();
        let tar = tar_bytes(&[
            (REZ_MANIFEST_NAME, &m0),
            ("rezolus/cpu_usage/0.parquet", SEG_A),
            (REZ_MANIFEST_NAME, &m1),
            (REZ_MANIFEST_NAME, b"{\"version\":2,\"recor"),
        ]);
        assert_recovered_to_first_segment(&tar);
    }

    #[test]
    fn block_boundary_truncation_is_clean_eof() {
        let (m0, m1, m2) = checkpointed_entries();
        let entries: Vec<(&str, &[u8])> = vec![
            (REZ_MANIFEST_NAME, &m0),
            ("rezolus/cpu_usage/0.parquet", SEG_A),
            (REZ_MANIFEST_NAME, &m1),
            ("rezolus/cpu_usage/1.parquet", SEG_B),
            (REZ_MANIFEST_NAME, &m2),
        ];
        let full = tar_bytes(&entries);
        // Cut exactly after a complete entry, with no footer: iteration ends
        // with no error at all, and the last complete manifest wins.
        assert_recovered_to_first_segment(&full[..tar_prefix_len(&entries[..3])]);
    }

    // `resolve` *moves* blobs out rather than cloning them, so two index
    // entries naming one file would leave the second table empty. Reject
    // instead — and, per the two-phase contract, without having touched
    // `blobs`, so an older manifest can still be tried against them.
    #[test]
    fn resolve_rejects_a_file_referenced_twice() {
        let table = |sampler: &str| RezTableIndex {
            sampler: sampler.to_string(),
            file: Some("cpu_usage.parquet".to_string()),
            files: Vec::new(),
            columns: Vec::new(),
            rows: 1,
            cadence_ns: None,
        };
        let manifest = RezManifest {
            version: REZ_SCHEMA_VERSION,
            recordings: vec![RezRecording {
                dir: "rezolus".to_string(),
                labels: BTreeMap::new(),
                metadata: BTreeMap::new(),
                complete: true,
                clock_anchor_wall_ns: None,
                clock_offsets: Vec::new(),
                tables: vec![table("cpu_usage"), table("scheduler")],
            }],
        };
        let mut blobs: HashMap<String, Vec<u8>> =
            [("rezolus/cpu_usage.parquet".to_string(), SEG_A.to_vec())]
                .into_iter()
                .collect();

        let err = match resolve(&manifest, &mut blobs) {
            Ok(_) => panic!("expected the duplicate reference to be rejected"),
            Err(e) => e.to_string(),
        };
        assert!(err.contains("referenced twice"), "{err}");
        assert!(err.contains("rezolus/cpu_usage.parquet"), "{err}");
        assert_eq!(
            blobs.get("rezolus/cpu_usage.parquet").map(Vec::as_slice),
            Some(SEG_A),
            "a failed resolve leaves the blobs intact"
        );
    }

    // Tolerance is for truncation, not for a manifest that lies: with no older
    // manifest to fall back to, an absent table file is still an error.
    #[test]
    fn single_manifest_missing_file_errors() {
        let m = manifest(&["cpu_usage/0.parquet"], true);
        let tar = tar_bytes(&[(REZ_MANIFEST_NAME, &m)]);
        let err = read_err(&tar);
        assert!(err.contains("rezolus/cpu_usage/0.parquet"), "{err}");
    }
}

#[cfg(test)]
mod finalize_tests {
    use super::recorder_tests_support::*;
    use super::*;
    use metriken::Window;
    use metriken_exposition::{Gauge, Snapshot, SnapshotV2};
    use std::collections::HashMap;
    use std::time::SystemTime;

    #[test]
    fn recorder_finalize_writes_readable_archive() {
        let mut r = RezRecorder::new(
            [("source".to_string(), "rezolus".to_string())]
                .into_iter()
                .collect(),
            [("source".to_string(), "rezolus".to_string())]
                .into_iter()
                .collect(),
            "rezolus".to_string(),
        );
        // drivehealth: same window over 3 polls → 1 row.
        let w = Window::new(900, 1_000);
        for i in 0..3u64 {
            r.ingest(
                &snap(1_000 + i, vec![counter("0", "drivehealth", 5, Some(w))]),
                1_000 + i,
            );
        }
        // cpu_perf: windowless → 3 rows.
        for i in 0..3u64 {
            r.ingest(
                &snap(2_000 + i, vec![counter("1", "cpu_perf", i, None)]),
                2_000 + i,
            );
        }

        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("rec.rez");
        r.finalize(&out).unwrap();

        let archive = read_archive(&out).unwrap();
        let rec = &archive.manifest.recordings[0];
        assert_eq!(
            rec.labels.get("source").map(String::as_str),
            Some("rezolus")
        );
        assert_eq!(
            rec.metadata.get("source").map(String::as_str),
            Some("rezolus")
        );
        let dh = archive.tables[0]
            .iter()
            .find(|t| t.sampler == "drivehealth")
            .unwrap();
        assert_eq!(dh.timestamps.len(), 1);
        assert_eq!(dh.columns[0].windows, vec![Some(Window::new(900, 1_000))]);
        let perf = archive.tables[0]
            .iter()
            .find(|t| t.sampler == "cpu_perf")
            .unwrap();
        assert_eq!(perf.timestamps.len(), 3);
        assert_eq!(perf.columns[0].windows, vec![None, None, None]);
    }

    // Added coverage: a GAUGE metric must round-trip through ingest→finalize→read
    // (the recorder sets metric_type="gauge", which the parquet reader keys on).
    #[test]
    fn recorder_round_trips_a_gauge_column() {
        let mut r = RezRecorder::new(
            BTreeMap::new(),
            [("source".to_string(), "rezolus".to_string())]
                .into_iter()
                .collect(),
            "rezolus".to_string(),
        );
        let w = Window::new(1_900, 2_000);
        let g = Gauge::new(
            "0".to_string(),
            -7,
            [
                ("metric".to_string(), "mem_free".to_string()),
                ("sampler".to_string(), "memory_meminfo".to_string()),
            ]
            .into_iter()
            .collect::<HashMap<_, _>>(),
        )
        .with_window(Some(w));
        let s = Snapshot::V2(SnapshotV2 {
            systemtime: SystemTime::UNIX_EPOCH + std::time::Duration::from_nanos(2_000),
            duration: std::time::Duration::ZERO,
            metadata: HashMap::new(),
            counters: Vec::new(),
            gauges: vec![g],
            histograms: Vec::new(),
        });
        r.ingest(&s, 2_000);

        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("g.rez");
        r.finalize(&out).unwrap();
        let archive = read_archive(&out).unwrap();
        let t = archive.tables[0]
            .iter()
            .find(|t| t.sampler == "memory_meminfo")
            .unwrap();
        assert_eq!(t.columns.len(), 1);
        match &t.columns[0].values {
            RezValues::Gauge(v) => assert_eq!(v, &vec![Some(-7)]),
            other => panic!("expected gauge column, got {other:?}"),
        }
        assert_eq!(t.columns[0].windows, vec![Some(w)]);
    }
}

#[cfg(test)]
mod selection_tests {
    use super::*;
    use crate::Format;
    use std::path::Path;

    #[test]
    fn extension_or_format_selects_rez() {
        assert!(wants_rez(Path::new("out.rez"), Format::Parquet));
        assert!(wants_rez(Path::new("out.parquet"), Format::Rez));
        assert!(!wants_rez(Path::new("out.parquet"), Format::Parquet));
        assert!(!wants_rez(Path::new("out.raw"), Format::Raw));
    }
}

#[cfg(test)]
mod label_tests {
    use super::*;

    #[test]
    fn host_from_systeminfo_extracts_hostname() {
        let json = r#"{"hostname":"node7","cpus":64}"#;
        assert_eq!(host_from_systeminfo(json), Some("node7".to_string()));
    }

    #[test]
    fn host_from_systeminfo_missing_or_invalid_is_none() {
        assert_eq!(host_from_systeminfo(r#"{"cpus":64}"#), None);
        assert_eq!(host_from_systeminfo("not json"), None);
        assert_eq!(host_from_systeminfo(r#"{"hostname":null}"#), None);
    }

    #[test]
    fn build_labels_populates_source_and_host() {
        let labels = build_labels("rezolus", Some(r#"{"hostname":"node7"}"#), &[]);
        assert_eq!(labels.get("source").map(String::as_str), Some("rezolus"));
        assert_eq!(labels.get("host").map(String::as_str), Some("node7"));
    }

    #[test]
    fn build_labels_no_host_when_systeminfo_absent() {
        let labels = build_labels("rezolus", None, &[]);
        assert_eq!(labels.get("source").map(String::as_str), Some("rezolus"));
        assert!(!labels.contains_key("host"));
    }

    #[test]
    fn build_labels_user_labels_merge_and_override() {
        let user = vec![
            ("arm".to_string(), "redis".to_string()),
            ("host".to_string(), "friendly".to_string()), // user overrides auto host
        ];
        let labels = build_labels("rezolus", Some(r#"{"hostname":"node7"}"#), &user);
        assert_eq!(labels.get("arm").map(String::as_str), Some("redis"));
        assert_eq!(labels.get("host").map(String::as_str), Some("friendly"));
    }

    #[test]
    fn recording_dir_slug_sanitizes_source() {
        let labels: BTreeMap<String, String> = [("source".to_string(), "llm-perf".to_string())]
            .into_iter()
            .collect();
        assert_eq!(recording_dir_slug(&labels), "llm-perf");
    }

    #[test]
    fn recording_dir_slug_replaces_unsafe_chars_and_defaults() {
        let labels: BTreeMap<String, String> = [("source".to_string(), "a/b c".to_string())]
            .into_iter()
            .collect();
        assert_eq!(recording_dir_slug(&labels), "a-b-c");
        assert_eq!(recording_dir_slug(&BTreeMap::new()), "recording");
    }
}
