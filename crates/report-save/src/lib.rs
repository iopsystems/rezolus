//! Save-as-Report machinery shared between the server viewer
//! (`rezolus view`) and the static-site WASM viewer.
//!
//! Both consumers project a source parquet onto the columns referenced
//! by a saved selection's queries (the "trim"), stamp the selection
//! JSON in the footer, and optionally repack a combined-A/B tarball.
//! The only API difference between the two is path-in vs bytes-in;
//! this crate operates uniformly on [`bytes::Bytes`] (the type
//! `metriken-query`'s `Tsdb::load_from_bytes` already uses), so the
//! server reads its parquet from disk into bytes before calling
//! through, and the WASM viewer reuses the bytes it already holds.

use std::collections::{BTreeSet, HashSet};

use arrow::datatypes::Field;
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

/// File-level marker: parquet was column-trimmed by Save as Report.
pub const KEY_REPORT: &str = "report";
pub const REPORT_VALUE_TRIMMED: &str = "trimmed";
pub const KEY_SELECTION: &str = "selection";
pub const KEY_DESCRIPTIONS: &str = "descriptions";
pub const KEY_EVENTS: &str = "events";

/// Row group size for saved reports; stays consistent with what the recorder
/// produces (`rezolus::parquet_metadata::MAX_ROW_GROUP_SIZE`). Kept smaller
/// than metriken-exposition's 50k default so the viewer's windowed (zoom)
/// reads decode far fewer row groups — see that constant for the measured
/// drill-down/size tradeoff.
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

/// Serializes `events` into the wire shape `{"events":[...]}` and
/// returns the JSON string. Returns `None` for empty input so callers
/// can skip the footer key entirely (matches the spec's "byte-identical
/// output for empty events" guarantee).
fn events_payload_json(events: &[Event]) -> Option<String> {
    if events.is_empty() {
        return None;
    }
    Some(
        serde_json::to_string(&serde_json::json!({ "events": events }))
            .expect("Event serializes deterministically"),
    )
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

// ── Top-level save entry points ──────────────────────────────────────

/// Project the source parquet down to the saved selection's columns
/// (when `trim_columns` is true), or just embed the selection JSON in
/// the footer (when false), keeping only the rows inside the payload's
/// [`time_range`](ReportPayload::time_range). Returns the new parquet bytes
/// ready to stream / download.
pub fn save_single_parquet(
    source_bytes: Bytes,
    payload: &ReportPayload,
    selection_json: &str,
    source: &dyn MetricsSource,
    trim_columns: bool,
) -> Result<Vec<u8>, String> {
    let events_json = events_payload_json(&payload.events);
    let rows = payload.time_range()?;
    if trim_columns {
        let kept = resolve_kept_columns(payload, source, Side::Baseline);
        trim_parquet_to_columns(
            source_bytes,
            &kept,
            selection_json,
            events_json.as_deref(),
            rows,
        )
    } else {
        embed_selection_in_parquet(source_bytes, selection_json, events_json.as_deref(), rows)
    }
}

/// Trim each per-side parquet independently (or embed-only when
/// `trim_columns` is false) and repack into a `*.parquet.ab.tar`. The
/// caller-supplied `manifest_bytes` (typically `serde_json::to_vec_pretty`
/// of an `AbContainers`) is written into the tar verbatim; this crate
/// doesn't need to know the manifest's shape.
#[allow(clippy::too_many_arguments)]
pub fn save_combined_ab_tarball(
    baseline_bytes: Bytes,
    experiment_bytes: Bytes,
    payload: &ReportPayload,
    selection_json: &str,
    baseline_source: &dyn MetricsSource,
    experiment_source: &dyn MetricsSource,
    manifest_bytes: &[u8],
    trim_columns: bool,
) -> Result<Vec<u8>, String> {
    let events_json = events_payload_json(&payload.events);
    let rows = payload.time_range()?;
    let (baseline_out, experiment_out) = if trim_columns {
        let baseline_kept = resolve_kept_columns(payload, baseline_source, Side::Baseline);
        let experiment_kept = resolve_kept_columns(payload, experiment_source, Side::Experiment);
        (
            trim_parquet_to_columns(
                baseline_bytes,
                &baseline_kept,
                selection_json,
                events_json.as_deref(),
                rows,
            )?,
            trim_parquet_to_columns(
                experiment_bytes,
                &experiment_kept,
                selection_json,
                events_json.as_deref(),
                rows,
            )?,
        )
    } else {
        (
            embed_selection_in_parquet(
                baseline_bytes,
                selection_json,
                events_json.as_deref(),
                rows,
            )?,
            embed_selection_in_parquet(
                experiment_bytes,
                selection_json,
                events_json.as_deref(),
                rows,
            )?,
        )
    };

    let mut buf: Vec<u8> = Vec::new();
    let mut builder = tar::Builder::new(&mut buf);
    builder.mode(tar::HeaderMode::Deterministic);
    append_tar_entry(&mut builder, "baseline.parquet", &baseline_out)?;
    append_tar_entry(&mut builder, "experiment.parquet", &experiment_out)?;
    append_tar_entry(&mut builder, "ab.json", manifest_bytes)?;
    builder.into_inner().map_err(|e| e.to_string())?;
    Ok(buf)
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

fn embed_selection_in_parquet(
    source_bytes: Bytes,
    selection_json: &str,
    events_json: Option<&str>,
    rows: Option<TimeRange>,
) -> Result<Vec<u8>, String> {
    let mut kv_meta = read_file_metadata(source_bytes.clone())?;
    kv_meta.retain(|kv| kv.key != KEY_SELECTION && kv.key != KEY_EVENTS);
    kv_meta.push(KeyValue {
        key: KEY_SELECTION.to_string(),
        value: Some(selection_json.to_string()),
    });
    if let Some(events) = events_json {
        kv_meta.push(KeyValue {
            key: KEY_EVENTS.to_string(),
            value: Some(events.to_string()),
        });
    }
    rewrite_parquet_bytes(source_bytes, kv_meta, None, rows)
}

fn trim_parquet_to_columns(
    source_bytes: Bytes,
    kept: &HashSet<String>,
    selection_json: &str,
    events_json: Option<&str>,
    rows: Option<TimeRange>,
) -> Result<Vec<u8>, String> {
    let builder = ParquetRecordBatchReaderBuilder::try_new(source_bytes.clone())
        .map_err(|e| e.to_string())?;
    let schema = builder.schema().clone();
    drop(builder);

    let indices: Vec<usize> = schema
        .fields()
        .iter()
        .enumerate()
        .filter(|(_, f)| keep_field(f, kept))
        .map(|(i, _)| i)
        .collect();
    if indices.is_empty() {
        return Err("trim produced an empty column set (source missing timestamp?)".to_string());
    }

    let mut kv_meta = read_file_metadata(source_bytes.clone())?;
    kv_meta.retain(|kv| kv.key != KEY_SELECTION && kv.key != KEY_REPORT && kv.key != KEY_EVENTS);
    kv_meta.push(KeyValue {
        key: KEY_REPORT.to_string(),
        value: Some(REPORT_VALUE_TRIMMED.to_string()),
    });
    kv_meta.push(KeyValue {
        key: KEY_SELECTION.to_string(),
        value: Some(selection_json.to_string()),
    });
    if let Some(events) = events_json {
        kv_meta.push(KeyValue {
            key: KEY_EVENTS.to_string(),
            value: Some(events.to_string()),
        });
    }

    let kept_names: BTreeSet<&str> = indices
        .iter()
        .flat_map(|&i| {
            let f = schema.field(i);
            std::iter::once(f.name().as_str()).chain(f.metadata().get("metric").map(String::as_str))
        })
        .collect();
    filter_descriptions(&mut kv_meta, &kept_names);

    rewrite_parquet_bytes(source_bytes, kv_meta, Some(&indices), rows)
}

/// Mirror of `parquet filter`'s field-keep predicate: exact name, base
/// before `:` (e.g. `foo` for `foo:buckets`), or the `metric` metadata
/// fallback for Prometheus-sourced columns whose physical name is a
/// numeric ID.
fn keep_field(f: &Field, kept: &HashSet<String>) -> bool {
    let name = f.name();
    kept.contains(name)
        || name
            .split_once(':')
            .is_some_and(|(base, _)| kept.contains(base))
        || f.metadata().get("metric").is_some_and(|m| kept.contains(m))
}

fn filter_descriptions(kv_meta: &mut [KeyValue], kept_names: &BTreeSet<&str>) {
    let Some(entry) = kv_meta.iter_mut().find(|kv| kv.key == KEY_DESCRIPTIONS) else {
        return;
    };
    let Some(value) = entry.value.as_deref() else {
        return;
    };
    let Ok(mut map) = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(value)
    else {
        return;
    };
    map.retain(|k, _| kept_names.contains(k.as_str()));
    if let Ok(filtered) = serde_json::to_string(&map) {
        entry.value = Some(filtered);
    }
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

/// `bytes` with only the rows inside `range`, footer unchanged.
fn parquet_rows_in(bytes: &[u8], range: TimeRange) -> Result<Vec<u8>, String> {
    let bytes = Bytes::copy_from_slice(bytes);
    let kv_meta = read_file_metadata(bytes.clone())?;
    rewrite_parquet_bytes(bytes, kv_meta, None, Some(range))
}

fn append_tar_entry<W: std::io::Write>(
    builder: &mut tar::Builder<W>,
    name: &str,
    data: &[u8],
) -> Result<(), String> {
    let mut header = tar::Header::new_gnu();
    header.set_path(name).map_err(|e| e.to_string())?;
    header.set_size(data.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    builder.append(&header, data).map_err(|e| e.to_string())?;
    Ok(())
}

// ── `.rez` reports ───────────────────────────────────────────────────
//
// A `.rez` source (or a parquet compare) saves to a `.rez` report rather than
// a trimmed parquet or a `.parquet.ab.tar`. Everything here is built IN MEMORY
// (`RezDb::create_in_memory` → `serialize`) and metriken-free, so the browser
// viewer runs it too. `keep_metrics` is the resolved column set the caller
// derived from the selection (via [`resolve_kept_columns`]); the caller owns
// column resolution because only it has the `MetricsSource`.

/// Embed the selection / events / report marker onto the anchor recording (the
/// first in catalog order — the baseline slot). `trimmed` stamps `KEY_REPORT`
/// so a reloaded archive opens straight to the Report view; an untrimmed save
/// carries the selection but no marker, matching the parquet footer path.
fn embed_rez_report_markers(
    db: &rez::rez_sqlite::RezDb,
    trimmed: bool,
    selection_json: &str,
    events_json: Option<&str>,
) -> Result<(), String> {
    let recordings = db.read_recordings()?;
    let anchor = recordings.first().ok_or_else(|| {
        "report has no recordings (source was empty or fully trimmed)".to_string()
    })?;
    let mut metadata = anchor.meta.metadata.clone();
    metadata.insert(KEY_SELECTION.to_string(), selection_json.to_string());
    if trimmed {
        metadata.insert(KEY_REPORT.to_string(), REPORT_VALUE_TRIMMED.to_string());
    }
    match events_json {
        Some(events) => {
            metadata.insert(KEY_EVENTS.to_string(), events.to_string());
        }
        // A save with no events must not leave a stale payload behind.
        None => {
            metadata.remove(KEY_EVENTS);
        }
    }
    db.update_recording_metadata(anchor.id, &metadata)
}

/// Build a `.rez` report from a `.rez` source's bytes: copy every recording
/// (trimming to `keep_metrics` when `Some`) and embed the selection. Returns
/// the new archive's bytes.
///
/// `range` keeps the segments overlapping it. A segment is copied whole, so
/// the report holds rows up to a segment's span beyond each end of `range`.
/// A dendro source with long tables is copied from [`OCCUPANT_RESTATE_NS`]
/// before `range`. An error when `range` keeps no segment.
pub fn build_rez_report_from_rez(
    source_bytes: &[u8],
    keep_metrics: Option<&BTreeSet<String>>,
    range: Option<TimeRange>,
    selection_json: &str,
    events_json: Option<&str>,
) -> Result<Vec<u8>, String> {
    use rez::rez_sqlite::RezDb;
    use rez::rez_v3_rewrite::{copy_recordings_into, CopySpec};

    if metriken_archive::DendroCatalog::is_archive_bytes(source_bytes) {
        return build_dendro_report(
            source_bytes,
            keep_metrics,
            range,
            selection_json,
            events_json,
        );
    }
    let src = RezDb::open_bytes(source_bytes.to_vec())?;
    let mut dst = RezDb::create_in_memory()?;
    dst.transaction(|tx| {
        src.read_snapshot(|src| {
            copy_recordings_into(
                src,
                tx,
                &CopySpec {
                    keep_metrics,
                    start: range.map_or(0, |r| r.start_ns),
                    end: range.map_or(u64::MAX, |r| r.end_ns),
                    ..CopySpec::everything()
                },
            )
            .map(|_| ())
        })
    })?;
    if range.is_some() {
        let mut rows = 0u64;
        for rec in dst.read_recordings()? {
            for table in dst.all_samplers(rec.id)? {
                rows += dst
                    .segments_overlapping(rec.id, &table, 0, u64::MAX)?
                    .iter()
                    .map(|seg| seg.meta.rows)
                    .sum::<u64>();
                rows += dst.live_wal_span(rec.id, &table)?.rows;
            }
        }
        if rows == 0 {
            return Err("the time range holds no rows".to_string());
        }
    }
    embed_rez_report_markers(&dst, keep_metrics.is_some(), selection_json, events_json)?;
    dst.serialize()
}

/// [`build_rez_report_from_rez`] for a dendro archive: a dendro report.
///
/// The same assembly, in memory: every source copied through dendro's
/// `copy_sources_into` (trimmed with metriken-archive's `KeepMetrics` when
/// `keep_metrics` is `Some`, which keeps a long table long and its occupant
/// stream whole), each stream's live tail sealed with metriken-archive's
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
    source_bytes: &[u8],
    keep_metrics: Option<&BTreeSet<String>>,
    range: Option<TimeRange>,
    selection_json: &str,
    events_json: Option<&str>,
) -> Result<Vec<u8>, String> {
    use dendro::archive::{Archive, ArchiveMut};
    use dendro::rewrite::{copy_sources_into, ColumnFilter, CopySpec};
    use rez::occupants::table_of;
    let err = |e: dendro::Error| e.to_string();

    let src = Archive::open_bytes(source_bytes.to_vec()).map_err(err)?;
    let mut names = Vec::new();
    for s in src.read_sources().map_err(err)? {
        names.extend(src.all_streams(s.id).map_err(err)?);
    }
    let encoder = metriken_archive::Encoder::for_streams(names.iter().map(String::as_str));
    let keep = keep_metrics.map(metriken_archive::KeepMetrics::new);
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
        writer_props: Some(metriken_archive::segment_props(
            metriken_archive::default_compression(),
        )),
        ..CopySpec::everything()
    };
    let mut dst = ArchiveMut::create_in_memory().map_err(err)?;
    dst.transaction(|tx| copy_sources_into(&src, tx, &spec, &encoder))
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

    let anchor = sources.first().ok_or_else(|| {
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
    dst.serialize().map_err(err)
}

/// The row-time interval at which metriken-archive's writer restates a long
/// table's live occupants, in nanoseconds: `WriterConfig::default()`'s
/// `restate_every_ns`, which needs the writer and so is not reachable from
/// here. The binary's tests pin the two together.
pub const OCCUPANT_RESTATE_NS: u64 = 300_000_000_000;

/// One side of a parquet compare: its bytes and the columns to keep (`None`
/// keeps all — an untrimmed save).
pub struct ParquetReportSide<'a> {
    pub bytes: &'a [u8],
    pub keep_metrics: Option<&'a BTreeSet<String>>,
}

/// Build a `.rez` report by ingesting parquet sides, each as one windowless
/// recording — the `.rez` replacement for a `.parquet.ab.tar`. `trimmed`
/// stamps the report marker (true when the sides were column-projected).
/// `range` keeps only the rows inside it, on every side.
pub fn build_rez_report_from_parquets(
    sides: &[ParquetReportSide<'_>],
    trimmed: bool,
    range: Option<TimeRange>,
    selection_json: &str,
    events_json: Option<&str>,
) -> Result<Vec<u8>, String> {
    use rez::parquet_ingest::ingest_parquet_bytes;
    use rez::rez_sqlite::RezDb;

    let ranged: Vec<Vec<u8>> = match range {
        Some(r) => sides
            .iter()
            .map(|side| parquet_rows_in(side.bytes, r))
            .collect::<Result<_, _>>()?,
        None => Vec::new(),
    };
    let mut dst = RezDb::create_in_memory()?;
    dst.transaction(|tx| {
        for (i, side) in sides.iter().enumerate() {
            let bytes = ranged.get(i).map_or(side.bytes, Vec::as_slice);
            ingest_parquet_bytes(bytes, tx, side.keep_metrics)?;
        }
        Ok(())
    })?;
    embed_rez_report_markers(&dst, trimmed, selection_json, events_json)?;
    dst.serialize()
}

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

    fn schema_names(bytes: &[u8]) -> Vec<String> {
        let b = ParquetRecordBatchReaderBuilder::try_new(Bytes::from(bytes.to_vec())).unwrap();
        b.schema()
            .fields()
            .iter()
            .map(|f| f.name().clone())
            .collect()
    }

    fn footer_kv(bytes: &[u8]) -> Vec<KeyValue> {
        let reader = SerializedFileReader::new(Bytes::from(bytes.to_vec())).unwrap();
        reader
            .metadata()
            .file_metadata()
            .key_value_metadata()
            .cloned()
            .unwrap_or_default()
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

    #[test]
    fn single_parquet_round_trip_trims_to_one_column() {
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
        let body = r#"{"version":1,"entries":[{"chartId":"c","promql_query":"m_a"}]}"#;
        let out = save_single_parquet(bytes, &payload, body, &reader, true).unwrap();
        assert_eq!(schema_names(&out), vec!["timestamp", "duration", "m_a"]);
        let kv = footer_kv(&out);
        assert_eq!(
            kv.iter()
                .find(|kv| kv.key == KEY_REPORT)
                .and_then(|kv| kv.value.as_deref()),
            Some(REPORT_VALUE_TRIMMED)
        );
        assert_eq!(
            kv.iter()
                .find(|kv| kv.key == KEY_SELECTION)
                .and_then(|kv| kv.value.as_deref()),
            Some(body)
        );
    }

    #[test]
    fn save_with_trim_columns_false_preserves_all_columns_and_skips_marker() {
        let (bytes, reader) = build_test(true);
        let payload = ReportPayload {
            entries: vec![ReportEntry {
                promql_query: "m_a".into(),
                promql_query_experiment: None,
            }],
            trim_columns: false,
            events: vec![],
            trim_range_ms: None,
        };
        let selection = r#"{"version":1,"entries":[{"chartId":"c","promql_query":"m_a"}]}"#;
        let out = save_single_parquet(bytes, &payload, selection, &reader, false).unwrap();
        assert_eq!(
            schema_names(&out),
            vec!["timestamp", "duration", "m_a", "m_b"]
        );
        let kv = footer_kv(&out);
        assert!(
            !kv.iter().any(|kv| kv.key == KEY_REPORT),
            "untrimmed save must not stamp KEY_REPORT"
        );
        assert_eq!(
            kv.iter()
                .find(|kv| kv.key == KEY_SELECTION)
                .and_then(|kv| kv.value.as_deref()),
            Some(selection)
        );
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

    #[test]
    fn combined_ab_round_trip_trims_each_side_and_repacks() {
        let (bytes_a, reader_a) = build_test(true);
        let (bytes_b, reader_b) = build_test(true);
        let payload = ReportPayload {
            entries: vec![ReportEntry {
                promql_query: "m_a".into(),
                promql_query_experiment: Some("m_b".into()),
            }],
            trim_columns: true,
            events: vec![],
            trim_range_ms: None,
        };
        let body = r#"{"version":1,"entries":[]}"#;
        // Manifest bytes are opaque to this crate; just hand it a valid
        // JSON blob and verify it lands in the tar.
        let manifest_bytes = br#"{"version":1,"baseline":{"alias":"a","sources":["svc"]},"experiment":{"alias":"b","sources":["svc"]}}"#;

        let out = save_combined_ab_tarball(
            bytes_a,
            bytes_b,
            &payload,
            body,
            &reader_a,
            &reader_b,
            manifest_bytes,
            true,
        )
        .unwrap();

        let mut archive = tar::Archive::new(std::io::Cursor::new(&out));
        let mut baseline_bytes: Vec<u8> = Vec::new();
        let mut experiment_bytes: Vec<u8> = Vec::new();
        let mut ab_json: Vec<u8> = Vec::new();
        let mut names = Vec::new();
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            let p = entry.path().unwrap().to_path_buf();
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            names.push(name.clone());
            match name.as_str() {
                "baseline.parquet" => std::io::copy(&mut entry, &mut baseline_bytes)
                    .map(|_| ())
                    .unwrap(),
                "experiment.parquet" => std::io::copy(&mut entry, &mut experiment_bytes)
                    .map(|_| ())
                    .unwrap(),
                "ab.json" => std::io::copy(&mut entry, &mut ab_json).map(|_| ()).unwrap(),
                _ => {}
            }
        }
        assert!(names.iter().any(|n| n == "baseline.parquet"));
        assert!(names.iter().any(|n| n == "experiment.parquet"));
        assert!(names.iter().any(|n| n == "ab.json"));
        assert_eq!(ab_json.as_slice(), manifest_bytes);
        assert_eq!(
            schema_names(&baseline_bytes),
            vec!["timestamp", "duration", "m_a"]
        );
        assert_eq!(
            schema_names(&experiment_bytes),
            vec!["timestamp", "duration", "m_b"]
        );
    }

    #[test]
    fn save_single_parquet_writes_key_events_when_payload_has_events() {
        let (bytes, reader) = build_test(true);
        let payload = ReportPayload {
            entries: vec![ReportEntry {
                promql_query: "m_a".into(),
                promql_query_experiment: None,
            }],
            trim_columns: true,
            events: vec![Event {
                timestamp: 1_715_625_600_000_000_000,
                description: "deploy".into(),
                kind: Some("deploy".into()),
                details: None,
                source: Some("svc".into()),
                node: None,
                instance: None,
                labels: Default::default(),
                duration_ns: None,
                id: None,
                chart_id: Some("c1".into()),
            }],
            trim_range_ms: None,
        };
        let body = r#"{"entries":[{"chartId":"c","promql_query":"m_a"}]}"#;
        let out = save_single_parquet(bytes, &payload, body, &reader, true).unwrap();
        let kv = footer_kv(&out);
        let events_value = kv
            .iter()
            .find(|kv| kv.key == "events")
            .and_then(|kv| kv.value.as_deref())
            .expect("KEY_EVENTS must be written when payload carries events");
        let parsed: serde_json::Value = serde_json::from_str(events_value).unwrap();
        let arr = parsed["events"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["description"], "deploy");
        assert_eq!(arr[0]["chart_id"], "c1");
    }

    #[test]
    fn save_single_parquet_skips_key_events_when_no_events() {
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
        let body = r#"{"entries":[{"chartId":"c","promql_query":"m_a"}]}"#;
        let out = save_single_parquet(bytes, &payload, body, &reader, true).unwrap();
        let kv = footer_kv(&out);
        assert!(
            !kv.iter().any(|kv| kv.key == "events"),
            "KEY_EVENTS must not be written when payload has no events"
        );
    }

    #[test]
    fn combined_ab_writes_events_to_both_sides() {
        let (bytes_a, reader_a) = build_test(true);
        let (bytes_b, reader_b) = build_test(true);
        let payload = ReportPayload {
            entries: vec![ReportEntry {
                promql_query: "m_a".into(),
                promql_query_experiment: Some("m_b".into()),
            }],
            trim_columns: true,
            events: vec![Event {
                timestamp: 1,
                description: "deploy".into(),
                kind: None,
                details: None,
                source: None,
                node: None,
                instance: None,
                labels: Default::default(),
                duration_ns: None,
                id: None,
                chart_id: None,
            }],
            trim_range_ms: None,
        };
        let body = r#"{"entries":[]}"#;
        let manifest_bytes = br#"{"version":1,"baseline":{"alias":"a","sources":["svc"]},"experiment":{"alias":"b","sources":["svc"]}}"#;
        let out = save_combined_ab_tarball(
            bytes_a,
            bytes_b,
            &payload,
            body,
            &reader_a,
            &reader_b,
            manifest_bytes,
            true,
        )
        .unwrap();

        let mut archive = tar::Archive::new(std::io::Cursor::new(&out));
        let mut baseline_bytes: Vec<u8> = Vec::new();
        let mut experiment_bytes: Vec<u8> = Vec::new();
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            let p = entry.path().unwrap().to_path_buf();
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            match name.as_str() {
                "baseline.parquet" => {
                    std::io::copy(&mut entry, &mut baseline_bytes).unwrap();
                }
                "experiment.parquet" => {
                    std::io::copy(&mut entry, &mut experiment_bytes).unwrap();
                }
                _ => {}
            }
        }
        for (label, bytes) in [
            ("baseline", &baseline_bytes),
            ("experiment", &experiment_bytes),
        ] {
            let kv = footer_kv(bytes);
            let val = kv
                .iter()
                .find(|kv| kv.key == "events")
                .and_then(|kv| kv.value.as_deref())
                .unwrap_or_else(|| panic!("{label} side missing KEY_EVENTS"));
            let parsed: serde_json::Value = serde_json::from_str(val).unwrap();
            assert_eq!(parsed["events"].as_array().unwrap().len(), 1);
        }
    }

    // ── `.rez` report builders ──

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

    /// A parquet compare assembles a 2-recording `.rez` report; when trimmed it
    /// stamps the report marker and embeds the selection on the anchor.
    #[test]
    fn parquet_compare_builds_a_two_recording_rez_report() {
        use rez::rez_sqlite::RezDb;
        let a = rez_test_parquet("redis", "cpu_usage", "cpu_cycles");
        let b = rez_test_parquet("valkey", "cpu_usage", "cpu_cycles");
        let sides = [
            ParquetReportSide {
                bytes: &a,
                keep_metrics: None,
            },
            ParquetReportSide {
                bytes: &b,
                keep_metrics: None,
            },
        ];
        let out =
            build_rez_report_from_parquets(&sides, true, None, r#"{"entries":[]}"#, None).unwrap();

        let db = RezDb::open_bytes(out).unwrap();
        let recs = db.read_recordings().unwrap();
        assert_eq!(recs.len(), 2, "one recording per parquet side");
        let anchor = &recs[0].meta.metadata;
        assert_eq!(
            anchor.get(KEY_SELECTION).map(String::as_str),
            Some(r#"{"entries":[]}"#)
        );
        assert_eq!(
            anchor.get(KEY_REPORT).map(String::as_str),
            Some(REPORT_VALUE_TRIMMED)
        );
    }

    /// The from-rez builder trims a `.rez` source and re-embeds a new
    /// selection; here the source is itself built by the parquet path, so the
    /// two shared builders round-trip together.
    #[test]
    fn from_rez_trims_and_reembeds_selection() {
        use rez::rez_sqlite::RezDb;
        let a = rez_test_parquet("redis", "cpu_usage", "cpu_cycles");
        let source = build_rez_report_from_parquets(
            &[ParquetReportSide {
                bytes: &a,
                keep_metrics: None,
            }],
            false,
            None,
            "{}",
            None,
        )
        .unwrap();
        // Untrimmed parquet build carries no report marker.
        {
            let db = RezDb::open_bytes(source.clone()).unwrap();
            assert!(!db.read_recordings().unwrap()[0]
                .meta
                .metadata
                .contains_key(KEY_REPORT));
        }

        let keep: BTreeSet<String> = ["cpu_cycles".to_string()].into_iter().collect();
        let out = build_rez_report_from_rez(&source, Some(&keep), None, r#"{"entries":[1]}"#, None)
            .unwrap();
        let db = RezDb::open_bytes(out).unwrap();
        let md = &db.read_recordings().unwrap()[0].meta.metadata;
        assert_eq!(
            md.get(KEY_SELECTION).map(String::as_str),
            Some(r#"{"entries":[1]}"#)
        );
        assert_eq!(
            md.get(KEY_REPORT).map(String::as_str),
            Some(REPORT_VALUE_TRIMMED)
        );
    }

    /// The timestamps (ns) of a parquet's rows.
    fn row_timestamps(bytes: &[u8]) -> Vec<u64> {
        let reader = ParquetRecordBatchReaderBuilder::try_new(Bytes::copy_from_slice(bytes))
            .unwrap()
            .build()
            .unwrap();
        let mut out = Vec::new();
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
        out
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

    /// A range keeps only the rows inside it, with or without a column trim.
    #[test]
    fn single_parquet_keeps_only_rows_in_the_range() {
        let sec = 1_000_000_000u64;
        for trim_columns in [true, false] {
            let (bytes, reader) = build_test(true);
            let payload = ranged_payload(1500.0, 3000.0, trim_columns);
            let out = save_single_parquet(bytes, &payload, "{}", &reader, trim_columns).unwrap();
            assert_eq!(
                row_timestamps(&out),
                vec![2 * sec, 3 * sec],
                "trim_columns={trim_columns}"
            );
        }
    }

    #[test]
    fn a_range_holding_no_rows_is_an_error() {
        let (bytes, reader) = build_test(true);
        let payload = ranged_payload(10_000.0, 20_000.0, true);
        let err = save_single_parquet(bytes, &payload, "{}", &reader, true).unwrap_err();
        assert!(err.contains("no rows"), "{err}");
    }

    /// A parquet compare keeps only each side's rows inside the range.
    #[test]
    fn parquet_compare_keeps_only_rows_in_the_range() {
        use rez::rez_sqlite::RezDb;
        let a = rez_test_parquet("redis", "cpu_usage", "cpu_cycles");
        let b = rez_test_parquet("valkey", "cpu_usage", "cpu_cycles");
        let all = row_timestamps(&a);
        assert!(all.len() >= 3, "fixture has rows to cut: {all:?}");
        let (lo, hi) = (all[1], all[all.len() - 2]);
        let sides = [
            ParquetReportSide {
                bytes: &a,
                keep_metrics: None,
            },
            ParquetReportSide {
                bytes: &b,
                keep_metrics: None,
            },
        ];
        let range = TimeRange {
            start_ns: lo,
            end_ns: hi,
        };
        let out = build_rez_report_from_parquets(&sides, false, Some(range), "{}", None).unwrap();
        let db = RezDb::open_bytes(out).unwrap();
        let recs = db.read_recordings().unwrap();
        assert_eq!(recs.len(), 2);
        for rec in &recs {
            let tables = db.all_samplers(rec.id).unwrap();
            assert!(!tables.is_empty());
            for table in tables {
                for seg in db
                    .segments_overlapping(rec.id, &table, 0, u64::MAX)
                    .unwrap()
                {
                    let m = &seg.meta;
                    assert!(
                        m.first_ts >= lo && m.last_ts <= hi,
                        "{table}: {}..{} outside {lo}..{hi}",
                        m.first_ts,
                        m.last_ts
                    );
                }
            }
        }
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
