//! Save-as-Report machinery shared between the server viewer
//! (`rezolus view`) and the static-site WASM viewer.
//!
//! A report is a `.dendro` archive: the recordings the page shows, trimmed
//! to the columns its saved selection queries (the "trim") and to its time
//! range, with the selection, the report marker and the events embedded on
//! the baseline's recording. A dendro source is copied; a tar `.rez` and
//! standalone parquet files are converted into the report in memory
//! (metriken-storage's `to_dendro`), so the browser builds the same report
//! the server does. A v3 `.rez` is converted by path, which only the server
//! has (`legacy::convert` there), and handed in as a dendro archive. Both
//! consumers work on bytes; the server reads its file first.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use bytes::Bytes;
use dashboard::Event;
use metriken_query::MetricsSource;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::arrow::ArrowWriter;
use parquet::file::metadata::KeyValue;
use parquet::file::properties::WriterProperties;
use parquet::file::reader::FileReader;
use parquet::file::serialized_reader::SerializedFileReader;
use serde::Deserialize;
use tracing::warn;

// ── Constants ────────────────────────────────────────────────────────

/// Recording-level marker: the report was column-trimmed by Save as Report.
pub const KEY_REPORT: &str = "report";
pub const REPORT_VALUE_TRIMMED: &str = "trimmed";
pub const KEY_SELECTION: &str = "selection";
pub use dashboard::keys::{KEY_DESCRIPTIONS, KEY_EVENTS};

/// Row group size of the in-memory parquet a time range cuts a parquet
/// source to before it is converted
/// (`rezolus::parquet_metadata::MAX_ROW_GROUP_SIZE`).
pub const MAX_ROW_GROUP_SIZE: usize = 1800;

// ── Payload types ────────────────────────────────────────────────────

/// Save-relevant subset of `/api/v1/save_with_selection`'s POST body.
/// Other fields on the wire (tagline, anchors, chartToggles, …) are
/// ignored — only entries' queries, events, the `trim_columns` flag and
/// `trim_range_ms` shape the output.
#[derive(Debug, Clone, Deserialize)]
pub struct ReportPayload {
    #[serde(default)]
    pub entries: Vec<ReportEntry>,
    #[serde(default = "default_trim_columns")]
    pub trim_columns: bool,
    #[serde(default)]
    pub events: Vec<Event>,
    /// The time range to keep, in milliseconds since the epoch. `None`
    /// keeps every row. See [`ReportPayload::time_range`].
    #[serde(default)]
    pub trim_range_ms: Option<TrimRangeMs>,
}

/// `trim_range_ms` as it arrives: milliseconds since the epoch, possibly
/// fractional.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct TrimRangeMs {
    pub start: f64,
    pub end: f64,
}

/// A report's row-timestamp bound, inclusive, in nanoseconds since the epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeRange {
    pub start_ns: u64,
    pub end_ns: u64,
}

impl TimeRange {
    fn contains(&self, ts: u64) -> bool {
        self.start_ns <= ts && ts <= self.end_ns
    }
}

impl ReportPayload {
    /// `trim_range_ms` in nanoseconds, widened outward by
    /// [`RANGE_SLACK_NS`] at each end. An error when either end is not finite
    /// or the start is after the end.
    pub fn time_range(&self) -> Result<Option<TimeRange>, String> {
        let Some(r) = self.trim_range_ms else {
            return Ok(None);
        };
        if !r.start.is_finite() || !r.end.is_finite() || r.start > r.end {
            return Err(format!("invalid trim_range_ms: {} to {}", r.start, r.end));
        }
        // `as` saturates: a negative start becomes 0, a huge end u64::MAX.
        Ok(Some(TimeRange {
            start_ns: ((r.start * 1e6).floor() as u64).saturating_sub(RANGE_SLACK_NS),
            end_ns: ((r.end * 1e6).ceil() as u64).saturating_add(RANGE_SLACK_NS),
        }))
    }
}

/// How far [`ReportPayload::time_range`] widens each end. A browser holds a
/// row's time as f64 milliseconds or seconds, about 240 ns apart near the
/// present, so a window that starts or ends on a row can land just inside
/// it; 1 µs keeps that row.
pub const RANGE_SLACK_NS: u64 = 1_000;

#[derive(Debug, Clone, Deserialize)]
pub struct ReportEntry {
    pub promql_query: String,
    #[serde(default)]
    pub promql_query_experiment: Option<String>,
}

fn default_trim_columns() -> bool {
    true
}

/// Per-entry: pick `promql_query` (Baseline) or `promql_query_experiment`
/// with fallback to `promql_query` (Experiment).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Side {
    Baseline,
    Experiment,
}

// ── Column resolution ────────────────────────────────────────────────

/// Resolve every entry's query against `source`, union the returned
/// columns, and always keep `timestamp` + `duration`.
///
/// Queries that fail to PARSE are logged and skipped — one malformed
/// chart shouldn't abort the whole save. Queries that parse but match
/// no series contribute nothing.
pub fn resolve_kept_columns(
    payload: &ReportPayload,
    source: &dyn MetricsSource,
    side: Side,
) -> HashSet<String> {
    let mut out: HashSet<String> = ["timestamp", "duration"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    for entry in &payload.entries {
        let query = match side {
            Side::Baseline => entry.promql_query.as_str(),
            Side::Experiment => entry
                .promql_query_experiment
                .as_deref()
                .unwrap_or(entry.promql_query.as_str()),
        };
        match source.columns(query) {
            Ok(cols) => out.extend(cols),
            Err(e) => warn!("report-save: skipped malformed query {query:?}: {e}"),
        }
    }
    out
}

// ── Building a report ────────────────────────────────────────────────

/// What a report is built from.
pub enum ReportSource<'a> {
    /// A dendro archive's bytes.
    Dendro(&'a [u8]),
    /// A v1 or v2 (tar) `.rez`'s bytes.
    RezTar(&'a [u8]),
    /// A v3 (SQLite) `.rez`'s bytes, its own WAL table included. The server
    /// converts a v3 `.rez` by path instead, which also reads SQLite's
    /// `-wal` sidecar beside the file.
    RezV3(&'a [u8]),
    /// Standalone parquet files' bytes, a recording each, baseline first.
    /// Two files are a compare: the report labels them `arm=baseline` and
    /// `arm=experiment`, so it reopens as the comparison it was saved from.
    Parquet(Vec<&'a [u8]>),
}

/// Build a dendro report of `source`: every recording copied (trimmed to
/// `keep_metrics` when `Some`), the selection, the report marker (when
/// trimmed) and the events embedded on the recording labelled `anchor` (the
/// baseline capture's; else the first), and only what `range` holds.
/// Returns the archive's bytes.
///
/// `range` keeps the segments overlapping it from an archive, a segment
/// copied whole, so the report holds rows up to a segment's span beyond
/// each end; a parquet source is cut to the rows inside it before it is
/// converted. An error when `range` keeps nothing.
pub fn build_report(
    source: ReportSource<'_>,
    anchor: Option<&BTreeMap<String, String>>,
    keep_metrics: Option<&BTreeSet<String>>,
    range: Option<TimeRange>,
    selection_json: &str,
    events_json: Option<&str>,
) -> Result<Vec<u8>, String> {
    use dendro::archive::{Archive, ArchiveMut};
    let err = |e: Box<dyn std::error::Error + Send + Sync>| e.to_string();
    match source {
        ReportSource::Dendro(bytes) => {
            let src = Archive::open_bytes(bytes.to_vec()).map_err(|e| e.to_string())?;
            build_dendro_report(
                &src,
                anchor,
                keep_metrics,
                range,
                selection_json,
                events_json,
            )
        }
        ReportSource::RezTar(bytes) => {
            let mut converted = ArchiveMut::create_in_memory().map_err(|e| e.to_string())?;
            metriken_storage::rez::to_dendro::convert_tar_into(
                std::io::Cursor::new(bytes),
                &mut converted,
                &metriken_storage::WriterConfig::default(),
            )
            .map_err(err)?;
            build_dendro_report(
                &converted,
                anchor,
                keep_metrics,
                range,
                selection_json,
                events_json,
            )
        }
        ReportSource::RezV3(bytes) => {
            let db = metriken_storage::rez::rez_sqlite::RezDb::open_bytes(bytes.to_vec())?;
            let mut converted = ArchiveMut::create_in_memory().map_err(|e| e.to_string())?;
            metriken_storage::rez::to_dendro::convert_v3_into(&db, &mut converted).map_err(err)?;
            build_dendro_report(
                &converted,
                anchor,
                keep_metrics,
                range,
                selection_json,
                events_json,
            )
        }
        ReportSource::Parquet(files) => {
            let mut converted = ArchiveMut::create_in_memory().map_err(|e| e.to_string())?;
            let arms: &[&str] = if files.len() == 2 {
                &["baseline", "experiment"]
            } else {
                &[]
            };
            for (i, bytes) in files.iter().enumerate() {
                let cut = match range {
                    Some(r) => Bytes::from(parquet_rows_in(bytes, r)?),
                    None => Bytes::copy_from_slice(bytes),
                };
                let mut converter = parquet_converter(&cut);
                if let Some(arm) = arms.get(i) {
                    converter = converter.label("arm", *arm);
                }
                converter.convert_into(cut, &mut converted).map_err(err)?;
            }
            // The rows are already cut, so the copy keeps every segment.
            build_dendro_report(
                &converted,
                anchor,
                keep_metrics,
                None,
                selection_json,
                events_json,
            )
        }
    }
}

/// The converter for the parquet file `bytes`: its source labelled as the
/// viewer labels the file when it reads it
/// (`dashboard::combined::parquet_source_label`), so the report presents as
/// the file did.
fn parquet_converter(bytes: &Bytes) -> metriken_storage::to_dendro::ParquetToArchive {
    let converter = metriken_storage::to_dendro::ParquetToArchive::new();
    match metriken_query::ParquetReader::open_bytes(bytes.clone())
        .ok()
        .and_then(|r| dashboard::combined::parquet_source_label(&r))
    {
        Some(source) => converter.label("source", source),
        None => converter,
    }
}

// ── Internals ────────────────────────────────────────────────────────

fn read_file_metadata(bytes: Bytes) -> Result<Vec<KeyValue>, String> {
    let reader = SerializedFileReader::new(bytes).map_err(|e| e.to_string())?;
    Ok(reader
        .metadata()
        .file_metadata()
        .key_value_metadata()
        .cloned()
        .unwrap_or_default())
}

/// Rewrite a parquet with `kv_meta` as its footer, keeping the columns in
/// `projection` (all when `None`) and the rows whose `timestamp` is inside
/// `rows` (all when `None`). An error when `rows` keeps no row.
fn rewrite_parquet_bytes(
    source: Bytes,
    kv_meta: Vec<KeyValue>,
    projection: Option<&[usize]>,
    rows: Option<TimeRange>,
) -> Result<Vec<u8>, String> {
    let builder = ParquetRecordBatchReaderBuilder::try_new(source).map_err(|e| e.to_string())?;
    let schema = builder.schema().clone();
    let reader = builder.build().map_err(|e| e.to_string())?;
    let timestamp = match rows {
        Some(_) => Some(
            schema
                .index_of("timestamp")
                .map_err(|_| "a time range needs a timestamp column".to_string())?,
        ),
        None => None,
    };
    let mut kept_rows = 0usize;

    let output_schema = match projection {
        Some(indices) => std::sync::Arc::new(schema.project(indices).map_err(|e| e.to_string())?),
        None => schema,
    };

    let props = WriterProperties::builder()
        .set_key_value_metadata(Some(kv_meta))
        .set_max_row_group_row_count(Some(MAX_ROW_GROUP_SIZE))
        .set_compression(parquet::basic::Compression::ZSTD(Default::default()))
        .build();

    let mut buf = Vec::new();
    {
        let mut writer = ArrowWriter::try_new(
            std::io::Cursor::new(&mut buf),
            output_schema.clone(),
            Some(props),
        )
        .map_err(|e| e.to_string())?;
        for batch in reader {
            let batch = batch.map_err(|e| e.to_string())?;
            let batch = match (rows, timestamp) {
                (Some(range), Some(col)) => filter_rows(&batch, col, range)?,
                _ => batch,
            };
            kept_rows += batch.num_rows();
            let batch = match projection {
                Some(indices) => batch.project(indices).map_err(|e| e.to_string())?,
                None => batch,
            };
            writer.write(&batch).map_err(|e| e.to_string())?;
        }
        writer.close().map_err(|e| e.to_string())?;
    }
    if rows.is_some() && kept_rows == 0 {
        return Err("the time range holds no rows".to_string());
    }
    Ok(buf)
}

/// The rows of `batch` whose `UInt64` column `col` (nanoseconds) is inside
/// `range`.
fn filter_rows(
    batch: &arrow::record_batch::RecordBatch,
    col: usize,
    range: TimeRange,
) -> Result<arrow::record_batch::RecordBatch, String> {
    use arrow::array::{Array, BooleanArray, UInt64Array};
    let ts = batch
        .column(col)
        .as_any()
        .downcast_ref::<UInt64Array>()
        .ok_or_else(|| "the timestamp column is not UInt64".to_string())?;
    let mask: BooleanArray = (0..ts.len())
        .map(|i| Some(!ts.is_null(i) && range.contains(ts.value(i))))
        .collect();
    arrow::compute::filter_record_batch(batch, &mask).map_err(|e| e.to_string())
}

/// `bytes` with only the rows inside `range`, footer unchanged: an
/// in-memory parquet, converted into the report.
fn parquet_rows_in(bytes: &[u8], range: TimeRange) -> Result<Vec<u8>, String> {
    let bytes = Bytes::copy_from_slice(bytes);
    let kv_meta = read_file_metadata(bytes.clone())?;
    rewrite_parquet_bytes(bytes, kv_meta, None, Some(range))
}

/// Remove an earlier report's keys from `metadata`. A recording without a
/// selection keeps its events: they are its own (`recording annotate
/// --event`, `record -- cmd`'s run_start/run_end), not a report's.
fn strip_report_keys(metadata: &mut BTreeMap<String, String>) -> bool {
    if metadata.remove(KEY_SELECTION).is_none() {
        return false;
    }
    metadata.remove(KEY_REPORT);
    metadata.remove(KEY_EVENTS);
    true
}

/// The item of `items` whose labels are `anchor`, else the first.
fn anchored<'a, T>(
    items: &'a [T],
    anchor: Option<&BTreeMap<String, String>>,
    labels: impl Fn(&T) -> &BTreeMap<String, String>,
) -> Option<&'a T> {
    anchor
        .and_then(|a| items.iter().find(|i| labels(i) == a))
        .or_else(|| items.first())
}

/// A dendro report of `src`, in memory: every source copied through dendro's
/// `copy_sources_into` (trimmed with metriken-storage's `KeepMetrics` when
/// `keep_metrics` is `Some`, which keeps a long table long and its occupant
/// stream whole), each stream's live tail sealed with metriken-storage's
/// `Encoder`, and segments re-encoded with the writer's properties. An
/// occupant stream whose table the trim dropped is removed, as `recording
/// filter` removes it. The markers go on the first source.
///
/// With `range`, the copy starts [`OCCUPANT_RESTATE_NS`] before it when the
/// source has long tables, so an occupant first seen before the range keeps
/// the restatement that names it. That is the bound hindsight's ranged dump
/// uses. The lead applies when the source has a long table, whether or not
/// `keep_metrics` keeps one.
fn build_dendro_report(
    src: &dendro::archive::Archive,
    anchor: Option<&BTreeMap<String, String>>,
    keep_metrics: Option<&BTreeSet<String>>,
    range: Option<TimeRange>,
    selection_json: &str,
    events_json: Option<&str>,
) -> Result<Vec<u8>, String> {
    use dendro::archive::ArchiveMut;
    use dendro::rewrite::{copy_sources_into, ColumnFilter, CopySpec};
    use rez::occupants::table_of;
    let err = |e: dendro::Error| e.to_string();

    let mut names = Vec::new();
    for s in src.read_sources().map_err(err)? {
        names.extend(src.all_streams(s.id).map_err(err)?);
    }
    let keep = keep_metrics.map(metriken_storage::KeepMetrics::new);
    let (start, end) = match range {
        Some(r) => {
            let long = names.iter().any(|s| table_of(s).is_some());
            let lead = if long { OCCUPANT_RESTATE_NS } else { 0 };
            let ns = |t: u64| i64::try_from(t).unwrap_or(i64::MAX);
            (ns(r.start_ns.saturating_sub(lead)), ns(r.end_ns))
        }
        None => (i64::MIN, i64::MAX),
    };
    let spec = CopySpec {
        start,
        end,
        keep_columns: keep.as_ref().map(|k| k as &dyn ColumnFilter),
        writer_props: Some(metriken_storage::segment_props(
            metriken_storage::default_compression(),
        )),
        ..CopySpec::everything()
    };
    let mut dst = ArchiveMut::create_in_memory().map_err(err)?;
    // The source is bytes in memory, so nothing can write it during the
    // copy; the encoder is built in the copy's snapshot all the same, as
    // metriken-storage documents for a source that could be live, and the
    // destination commits outside it, the order rezolus's `dendro_copy` uses.
    dst.transaction(|tx| {
        src.read_snapshot(|src| {
            let encoder = metriken_storage::Encoder::for_archive(src)?;
            copy_sources_into(src, tx, &spec, &encoder)
        })
    })
    .map_err(err)?;

    let sources = dst.read_sources().map_err(err)?;
    if range.is_some() {
        let mut rows = 0u64;
        for source in &sources {
            for stream in dst.all_streams(source.id).map_err(err)? {
                if table_of(&stream).is_some() {
                    continue;
                }
                rows += dst
                    .segments_overlapping(source.id, &stream, i64::MIN, i64::MAX)
                    .map_err(err)?
                    .iter()
                    .map(|seg| seg.meta.rows)
                    .sum::<u64>();
                rows += dst.live_wal_span(source.id, &stream).map_err(err)?.rows;
            }
        }
        if rows == 0 {
            return Err("the time range holds no rows".to_string());
        }
    }
    for source in &sources {
        let streams = dst.all_streams(source.id).map_err(err)?;
        let tables: BTreeSet<&str> = streams
            .iter()
            .map(String::as_str)
            .filter(|s| table_of(s).is_none())
            .collect();
        let orphan = |stream: &str| table_of(stream).is_some_and(|t| !tables.contains(t));
        if streams.iter().any(|s| orphan(s)) {
            dst.evict_streams_before(source.id, i64::MAX, &orphan)
                .map_err(err)?;
        }
    }

    let anchor = anchored(&sources, anchor, |s| &s.meta.labels).ok_or_else(|| {
        "report has no recordings (source was empty or fully trimmed)".to_string()
    })?;
    let mut metadata = anchor.meta.metadata.clone();
    metadata.insert(KEY_SELECTION.to_string(), selection_json.to_string());
    if keep_metrics.is_some() {
        metadata.insert(KEY_REPORT.to_string(), REPORT_VALUE_TRIMMED.to_string());
    }
    match events_json {
        Some(events) => {
            metadata.insert(KEY_EVENTS.to_string(), events.to_string());
        }
        None => {
            metadata.remove(KEY_EVENTS);
        }
    }
    dst.update_source_metadata(anchor.id, &metadata)
        .map_err(err)?;
    // Only the anchor carries a report: one copied from an earlier report
    // onto another recording would be read instead on reload. A recording's
    // own events stay ([`strip_report_keys`]).
    for other in sources.iter().filter(|s| s.id != anchor.id) {
        let mut metadata = other.meta.metadata.clone();
        if strip_report_keys(&mut metadata) {
            dst.update_source_metadata(other.id, &metadata)
                .map_err(err)?;
        }
    }
    dst.serialize().map_err(err)
}

/// The row-time interval at which metriken-storage's writer restates a long
/// table's live occupants, in nanoseconds: `WriterConfig::default()`'s
/// `restate_every_ns`, which needs the writer and so is not reachable from
/// here. The binary's tests pin the two together.
pub const OCCUPANT_RESTATE_NS: u64 = 300_000_000_000;

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::{Int64Array, UInt64Array};
    use arrow::datatypes::{DataType, Field, Schema};
    use arrow::record_batch::RecordBatch;
    use metriken_query::ParquetReader;
    use std::sync::Arc;

    /// Build a tiny single-source parquet (timestamp, duration, m_a,
    /// m_b) entirely in memory and return both its bytes and the loaded
    /// ParquetReader.
    fn build_test(_parquet: bool) -> (Bytes, ParquetReader) {
        let sec = 1_000_000_000u64;
        let mut meta = std::collections::HashMap::new();
        meta.insert("metric_type".to_string(), "gauge".to_string());

        let schema = Arc::new(Schema::new(vec![
            Field::new("timestamp", DataType::UInt64, false),
            Field::new("duration", DataType::UInt64, false),
            Field::new("m_a", DataType::Int64, false).with_metadata(meta.clone()),
            Field::new("m_b", DataType::Int64, false).with_metadata(meta),
        ]));
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(UInt64Array::from(vec![sec, 2 * sec, 3 * sec])),
                Arc::new(UInt64Array::from(vec![sec; 3])),
                Arc::new(Int64Array::from(vec![1, 2, 3])),
                Arc::new(Int64Array::from(vec![10, 20, 30])),
            ],
        )
        .unwrap();
        let kv = vec![
            KeyValue {
                key: "source".into(),
                value: Some("svc".into()),
            },
            KeyValue {
                key: "sampling_interval_ms".into(),
                value: Some("1000".into()),
            },
        ];
        let props = WriterProperties::builder()
            .set_key_value_metadata(Some(kv))
            .build();
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut writer =
                ArrowWriter::try_new(std::io::Cursor::new(&mut buf), schema, Some(props)).unwrap();
            writer.write(&batch).unwrap();
            writer.close().unwrap();
        }
        let bytes = Bytes::from(buf);
        let reader = ParquetReader::open_bytes(bytes.clone()).expect("reader loads");
        (bytes, reader)
    }

    /// Each source of a dendro report: its labels, its metadata, and its
    /// streams' column names.
    type SourceShape = (
        BTreeMap<String, String>,
        BTreeMap<String, String>,
        BTreeSet<String>,
    );

    /// What each source of the dendro archive `bytes` holds.
    fn sources_of(bytes: Vec<u8>) -> Vec<SourceShape> {
        use metriken_storage::Catalog;
        let archive = dendro::archive::Archive::open_bytes(bytes.clone()).unwrap();
        let catalog = metriken_storage::DendroCatalog::open_bytes(bytes).unwrap();
        archive
            .read_sources()
            .unwrap()
            .into_iter()
            .map(|s| {
                let mut columns = BTreeSet::new();
                for table in catalog.tables(s.id).unwrap() {
                    for (seq, _) in catalog.segment_meta(s.id, &table).unwrap() {
                        let bytes = catalog.segment_bytes(s.id, &table, seq).unwrap().unwrap();
                        let b =
                            ParquetRecordBatchReaderBuilder::try_new(Bytes::from(bytes)).unwrap();
                        columns.extend(b.schema().fields().iter().map(|f| f.name().clone()));
                    }
                }
                (s.meta.labels, s.meta.metadata, columns)
            })
            .collect()
    }

    /// The row timestamps of every segment of the dendro archive `bytes`.
    fn report_timestamps(bytes: Vec<u8>) -> Vec<u64> {
        use metriken_storage::Catalog;
        let catalog = metriken_storage::DendroCatalog::open_bytes(bytes).unwrap();
        let mut out = Vec::new();
        for source in catalog.sources().unwrap() {
            for table in catalog.tables(source.id).unwrap() {
                for (seq, _) in catalog.segment_meta(source.id, &table).unwrap() {
                    let bytes = catalog
                        .segment_bytes(source.id, &table, seq)
                        .unwrap()
                        .unwrap();
                    let reader = ParquetRecordBatchReaderBuilder::try_new(Bytes::from(bytes))
                        .unwrap()
                        .build()
                        .unwrap();
                    for batch in reader {
                        let batch = batch.unwrap();
                        let col = batch.schema().index_of("timestamp").unwrap();
                        let ts = batch
                            .column(col)
                            .as_any()
                            .downcast_ref::<UInt64Array>()
                            .unwrap();
                        out.extend(ts.values().iter().copied());
                    }
                }
            }
        }
        out.sort_unstable();
        out
    }

    #[test]
    fn baseline_side_kept_set_includes_timestamp_and_duration() {
        let (_bytes, reader) = build_test(true);

        let payload = ReportPayload {
            entries: vec![ReportEntry {
                promql_query: "m_a".into(),
                promql_query_experiment: None,
            }],
            trim_columns: true,
            events: vec![],
            trim_range_ms: None,
        };
        let kept = resolve_kept_columns(&payload, &reader, Side::Baseline);
        assert!(kept.contains("timestamp"));
        assert!(kept.contains("duration"));
        assert!(kept.contains("m_a"));
        assert!(!kept.contains("m_b"));
    }

    #[test]
    fn experiment_side_falls_back_to_promql_query_when_experiment_unset() {
        let (_bytes, reader) = build_test(true);

        let payload = ReportPayload {
            entries: vec![ReportEntry {
                promql_query: "m_a".into(),
                promql_query_experiment: None,
            }],
            trim_columns: true,
            events: vec![],
            trim_range_ms: None,
        };
        let kept = resolve_kept_columns(&payload, &reader, Side::Experiment);
        assert!(kept.contains("m_a"));
        assert!(!kept.contains("m_b"));
    }

    #[test]
    fn experiment_side_uses_promql_query_experiment_when_set() {
        let (_bytes, reader) = build_test(true);

        let payload = ReportPayload {
            entries: vec![ReportEntry {
                promql_query: "m_a".into(),
                promql_query_experiment: Some("m_b".into()),
            }],
            trim_columns: true,
            events: vec![],
            trim_range_ms: None,
        };
        let kept_b = resolve_kept_columns(&payload, &reader, Side::Baseline);
        let kept_e = resolve_kept_columns(&payload, &reader, Side::Experiment);
        assert!(kept_b.contains("m_a") && !kept_b.contains("m_b"));
        assert!(kept_e.contains("m_b") && !kept_e.contains("m_a"));
    }

    #[test]
    fn parses_minimal_payload() {
        let json = r#"{
            "version": 1,
            "entries": [
                {"chartId": "c1", "promql_query": "cpu_cores"},
                {"chartId": "c2", "promql_query": "cpu_usage",
                 "promql_query_experiment": "cpu_usage{state=\"user\"}"}
            ]
        }"#;
        let payload: ReportPayload = serde_json::from_str(json).unwrap();
        assert_eq!(payload.entries.len(), 2);
        assert_eq!(payload.entries[0].promql_query, "cpu_cores");
        assert_eq!(
            payload.entries[1].promql_query_experiment.as_deref(),
            Some("cpu_usage{state=\"user\"}")
        );
    }

    #[test]
    fn experiment_query_optional() {
        let json = r#"{ "entries": [{"chartId": "c", "promql_query": "m"}] }"#;
        let payload: ReportPayload = serde_json::from_str(json).unwrap();
        assert!(payload.entries[0].promql_query_experiment.is_none());
    }

    #[test]
    fn trim_columns_defaults_true_when_omitted() {
        let json = r#"{ "entries": [] }"#;
        let payload: ReportPayload = serde_json::from_str(json).unwrap();
        assert!(payload.trim_columns);
    }

    #[test]
    fn trim_columns_false_when_explicit() {
        let json = r#"{ "entries": [], "trim_columns": false }"#;
        let payload: ReportPayload = serde_json::from_str(json).unwrap();
        assert!(!payload.trim_columns);
    }

    /// A single parquet saves a dendro report of one source, labelled with
    /// the file's `source`, trimmed to the columns kept, with the selection
    /// and the report marker on it.
    #[test]
    fn a_parquet_saves_a_trimmed_dendro_report() {
        let (bytes, reader) = build_test(true);
        let payload = ReportPayload {
            entries: vec![ReportEntry {
                promql_query: "m_a".into(),
                promql_query_experiment: None,
            }],
            trim_columns: true,
            events: vec![],
            trim_range_ms: None,
        };
        let keep: BTreeSet<String> = resolve_kept_columns(&payload, &reader, Side::Baseline)
            .into_iter()
            .collect();
        let body = r#"{"version":1,"entries":[{"chartId":"c","promql_query":"m_a"}]}"#;
        let out = build_report(
            ReportSource::Parquet(vec![&bytes]),
            None,
            Some(&keep),
            None,
            body,
            None,
        )
        .unwrap();
        let sources = sources_of(out);
        assert_eq!(sources.len(), 1);
        let (labels, metadata, columns) = &sources[0];
        assert_eq!(labels.get("source").map(String::as_str), Some("svc"));
        assert_eq!(metadata.get(KEY_SELECTION).map(String::as_str), Some(body));
        assert_eq!(
            metadata.get(KEY_REPORT).map(String::as_str),
            Some(REPORT_VALUE_TRIMMED)
        );
        assert!(columns.contains("m_a"), "{columns:?}");
        assert!(!columns.contains("m_b"), "{columns:?}");
    }

    /// An untrimmed save keeps every column and stamps no report marker,
    /// and its events go on the anchor.
    #[test]
    fn an_untrimmed_save_keeps_every_column_and_no_marker() {
        let (bytes, _reader) = build_test(true);
        let events = r#"{"events":[{"timestamp":1000000000,"description":"x"}]}"#;
        let out = build_report(
            ReportSource::Parquet(vec![&bytes]),
            None,
            None,
            None,
            "{}",
            Some(events),
        )
        .unwrap();
        let (_, metadata, columns) = &sources_of(out)[0];
        assert!(
            columns.contains("m_a") && columns.contains("m_b"),
            "{columns:?}"
        );
        assert!(!metadata.contains_key(KEY_REPORT));
        assert_eq!(metadata.get(KEY_EVENTS).map(String::as_str), Some(events));
    }

    #[test]
    fn events_default_to_empty() {
        let json = r#"{"entries":[]}"#;
        let payload: ReportPayload = serde_json::from_str(json).unwrap();
        assert!(payload.events.is_empty());
    }

    #[test]
    fn events_round_trip_through_payload() {
        let json = r#"{
            "entries": [],
            "events": [
                {"timestamp": 1715625600000000000, "description": "deploy", "chart_id": "queue_depth"},
                {"timestamp": 1715625900000000000, "description": "restart"}
            ]
        }"#;
        let payload: ReportPayload = serde_json::from_str(json).unwrap();
        assert_eq!(payload.events.len(), 2);
        assert_eq!(payload.events[0].description, "deploy");
        assert_eq!(payload.events[0].chart_id.as_deref(), Some("queue_depth"));
        assert_eq!(payload.events[1].chart_id, None);
    }

    fn rez_test_parquet(source: &str, sampler: &str, metric: &str) -> Vec<u8> {
        use arrow::array::UInt64Array;
        use arrow::datatypes::{DataType, Field, Schema};
        use arrow::record_batch::RecordBatch;
        use std::collections::HashMap;
        use std::sync::Arc;

        let tsf =
            Field::new("timestamp", DataType::UInt64, false).with_metadata(HashMap::from([(
                "metric_type".to_string(),
                "timestamp".to_string(),
            )]));
        let durf = Field::new("duration", DataType::UInt64, true).with_metadata(HashMap::from([(
            "metric_type".to_string(),
            "duration".to_string(),
        )]));
        let mf = Field::new(metric, DataType::UInt64, true).with_metadata(HashMap::from([
            ("metric_type".to_string(), "counter".to_string()),
            ("sampler".to_string(), sampler.to_string()),
        ]));
        let schema = Arc::new(Schema::new(vec![tsf, durf, mf]));
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(UInt64Array::from(vec![1000u64, 2000, 3000])),
                Arc::new(UInt64Array::from(vec![Some(5u64), Some(5), Some(5)])),
                Arc::new(UInt64Array::from(vec![1u64, 2, 3])),
            ],
        )
        .unwrap();
        let props = WriterProperties::builder()
            .set_key_value_metadata(Some(vec![KeyValue {
                key: "source".to_string(),
                value: Some(source.to_string()),
            }]))
            .build();
        let mut buf = Vec::new();
        {
            let mut w = ArrowWriter::try_new(&mut buf, schema, Some(props)).unwrap();
            w.write(&batch).unwrap();
            w.close().unwrap();
        }
        buf
    }

    /// A parquet compare saves a 2-source dendro report, one source per
    /// side labelled with its `source` and its `arm`, the markers on the
    /// first.
    #[test]
    fn a_parquet_compare_saves_a_two_source_report() {
        let a = rez_test_parquet("redis", "cpu_usage", "cpu_cycles");
        let b = rez_test_parquet("valkey", "cpu_usage", "cpu_cycles");
        let keep: BTreeSet<String> = ["cpu_cycles".to_string()].into_iter().collect();
        let out = build_report(
            ReportSource::Parquet(vec![&a, &b]),
            None,
            Some(&keep),
            None,
            r#"{"entries":[]}"#,
            None,
        )
        .unwrap();
        let sources = sources_of(out);
        let names: Vec<&str> = sources.iter().map(|s| s.0["source"].as_str()).collect();
        assert_eq!(names, ["redis", "valkey"]);
        let arms: Vec<&str> = sources.iter().map(|s| s.0["arm"].as_str()).collect();
        assert_eq!(arms, ["baseline", "experiment"], "it reopens as a compare");
        assert_eq!(
            sources[0].1.get(KEY_SELECTION).map(String::as_str),
            Some(r#"{"entries":[]}"#)
        );
        assert!(!sources[1].1.contains_key(KEY_SELECTION));
    }

    /// A dendro source is copied, and saving again from a report moves the
    /// markers to the new anchor: an earlier report's keys leave the others.
    #[test]
    fn a_report_saved_again_keeps_one_anchor() {
        let a = rez_test_parquet("redis", "cpu_usage", "cpu_cycles");
        let b = rez_test_parquet("valkey", "cpu_usage", "cpu_cycles");
        let first = build_report(
            ReportSource::Parquet(vec![&a, &b]),
            None,
            None,
            None,
            "{}",
            None,
        )
        .unwrap();
        let anchor: BTreeMap<String, String> = [
            ("arm".to_string(), "experiment".to_string()),
            ("source".to_string(), "valkey".to_string()),
        ]
        .into();
        let again = build_report(
            ReportSource::Dendro(&first),
            Some(&anchor),
            None,
            None,
            r#"{"entries":[1]}"#,
            None,
        )
        .unwrap();
        let sources = sources_of(again);
        let marked: Vec<&str> = sources
            .iter()
            .filter(|s| s.1.contains_key(KEY_SELECTION))
            .map(|s| s.0["source"].as_str())
            .collect();
        assert_eq!(marked, ["valkey"]);
    }

    /// A v3 `.rez` converts into the report in memory, as the browser saves
    /// one: the committed `v3-multi.rez`'s two recordings, with their labels,
    /// and `v3-live.rez`'s rows still in its WAL table.
    #[test]
    fn a_v3_rez_saves_a_dendro_report() {
        let fixture = |name: &str| {
            std::fs::read(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../rez/tests/fixtures/rez")
                    .join(name),
            )
            .unwrap()
        };
        let out = build_report(
            ReportSource::RezV3(&fixture("v3-multi.rez")),
            None,
            None,
            None,
            "{}",
            None,
        )
        .unwrap();
        let sources = sources_of(out);
        assert_eq!(sources.len(), 2);
        assert_eq!(
            sources[0].1.get(KEY_SELECTION).map(String::as_str),
            Some("{}")
        );
        let live = build_report(
            ReportSource::RezV3(&fixture("v3-live.rez")),
            None,
            None,
            None,
            "{}",
            None,
        )
        .unwrap();
        assert_eq!(sources_of(live).len(), 1);
    }

    /// A tar `.rez` source converts into the report in memory, as the
    /// browser saves one: the committed `v2.rez`'s one recording, with its
    /// labels.
    #[test]
    fn a_tar_rez_saves_a_dendro_report() {
        let tar = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../rez/tests/fixtures/rez/v2.rez"),
        )
        .unwrap();
        let out = build_report(ReportSource::RezTar(&tar), None, None, None, "{}", None).unwrap();
        let sources = sources_of(out);
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].0.get("host").map(String::as_str), Some("web-01"));
        assert_eq!(
            sources[0].1.get(KEY_SELECTION).map(String::as_str),
            Some("{}")
        );
    }

    fn ranged_payload(start: f64, end: f64, trim_columns: bool) -> ReportPayload {
        ReportPayload {
            entries: vec![ReportEntry {
                promql_query: "m_a".into(),
                promql_query_experiment: None,
            }],
            trim_columns,
            events: vec![],
            trim_range_ms: Some(TrimRangeMs { start, end }),
        }
    }

    #[test]
    fn time_range_converts_ms_to_ns_and_rejects_bad_ranges() {
        let p = ranged_payload(1500.0, 2500.5, true);
        assert_eq!(
            p.time_range().unwrap(),
            Some(TimeRange {
                start_ns: 1_500_000_000 - RANGE_SLACK_NS,
                end_ns: 2_500_500_000 + RANGE_SLACK_NS,
            })
        );
        assert!(ranged_payload(3.0, 2.0, true).time_range().is_err());
        assert!(ranged_payload(f64::NAN, 2.0, true).time_range().is_err());
        let body: ReportPayload =
            serde_json::from_str(r#"{"trim_range_ms":{"start":1000,"end":2000}}"#).unwrap();
        assert_eq!(
            body.time_range().unwrap(),
            Some(TimeRange {
                start_ns: 1_000_000_000 - RANGE_SLACK_NS,
                end_ns: 2_000_000_000 + RANGE_SLACK_NS,
            })
        );
    }

    /// A range keeps only a parquet's rows inside it.
    #[test]
    fn a_parquet_report_keeps_only_rows_in_the_range() {
        let sec = 1_000_000_000u64;
        let (bytes, _reader) = build_test(true);
        let range = ranged_payload(1500.0, 3000.0, false).time_range().unwrap();
        let out = build_report(
            ReportSource::Parquet(vec![&bytes]),
            None,
            None,
            range,
            "{}",
            None,
        )
        .unwrap();
        assert_eq!(report_timestamps(out), vec![2 * sec, 3 * sec]);
    }

    #[test]
    fn a_range_holding_no_rows_is_an_error() {
        let (bytes, _reader) = build_test(true);
        let range = ranged_payload(10_000.0, 20_000.0, true)
            .time_range()
            .unwrap();
        let err = build_report(
            ReportSource::Parquet(vec![&bytes]),
            None,
            None,
            range,
            "{}",
            None,
        )
        .unwrap_err();
        assert!(err.contains("no rows"), "{err}");
    }

    /// A window whose ends came through browser f64 seconds keeps the rows
    /// it starts and ends on.
    #[test]
    fn a_window_on_row_times_keeps_its_edge_rows() {
        let first: u64 = 1_790_144_092_001_835_800;
        let last: u64 = first + 34_677_000_000_000;
        // As the page computes them: seconds as f64, times 1000.
        let ms = |ns: u64| (ns as f64 / 1e9) * 1000.0;
        let p = ranged_payload(ms(first), ms(last), false);
        let r = p.time_range().unwrap().unwrap();
        assert!(r.contains(first) && r.contains(last), "{r:?}");
    }
}
