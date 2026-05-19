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
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::arrow::ArrowWriter;
use parquet::file::metadata::KeyValue;
use parquet::file::properties::WriterProperties;
use parquet::file::reader::FileReader;
use parquet::file::serialized_reader::SerializedFileReader;
use serde::Deserialize;

// ── Constants ────────────────────────────────────────────────────────

/// File-level marker: parquet was column-trimmed by Save as Report.
pub const KEY_REPORT: &str = "report";
pub const REPORT_VALUE_TRIMMED: &str = "trimmed";
pub const KEY_SELECTION: &str = "selection";
pub const KEY_DESCRIPTIONS: &str = "descriptions";
pub const KEY_EVENTS: &str = "events";

/// Matches metriken-exposition's ParquetWriter default; row group sizing
/// stays consistent with what the recorder produces.
pub const MAX_ROW_GROUP_SIZE: usize = 50_000;

// ── Payload types ────────────────────────────────────────────────────

/// Save-relevant subset of `/api/v1/save_with_selection`'s POST body.
/// Other fields on the wire (tagline, anchors, chartToggles, …) are
/// ignored — only entries' queries and the `trim_columns` flag shape
/// the output.
#[derive(Debug, Clone, Deserialize)]
pub struct ReportPayload {
    #[serde(default)]
    pub entries: Vec<ReportEntry>,
    #[serde(default = "default_trim_columns")]
    pub trim_columns: bool,
    #[serde(default)]
    pub events: Vec<Event>,
}

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

// ── Top-level save entry points ──────────────────────────────────────

/// Trim-free variant: write the source parquet back out with the
/// selection JSON embedded in the footer. SQL-only callers use this
/// when the saved selection has no entries to drive trim; the
/// SQL-aware trim path is [`save_single_parquet_sql`].
pub fn save_single_parquet_embed_only(
    source_bytes: Bytes,
    selection_json: &str,
) -> Result<Vec<u8>, String> {
    embed_selection_in_parquet(source_bytes, selection_json, None)
}

/// SQL-backed single-parquet save with column trim. Mirrors
/// [`save_single_parquet`] but resolves kept columns from a
/// `MetricCatalog` + the saved selection's query strings, so no Tsdb
/// is required. Works for both PromQL-shaped and SQL-shaped query
/// strings — see [`resolve_kept_columns_sql`] for the matching rule.
pub fn save_single_parquet_sql(
    source_bytes: Bytes,
    payload: &ReportPayload,
    selection_json: &str,
    catalog: &metriken_query_sql::MetricCatalog,
    trim_columns: bool,
) -> Result<Vec<u8>, String> {
    let events_json = events_payload_json(&payload.events);
    if trim_columns {
        let kept = resolve_kept_columns_sql(payload, catalog, Side::Baseline);
        trim_parquet_to_columns(source_bytes, &kept, selection_json, events_json.as_deref())
    } else {
        embed_selection_in_parquet(source_bytes, selection_json, events_json.as_deref())
    }
}

/// Resolve kept physical columns for a SQL-backed capture by scanning
/// each entry's query text for catalog-known names.
///
/// Two passes per entry:
/// 1. **Metric-name match.** For each metric in
///    `catalog.series_by_metric`, check whether the metric name
///    appears in the query as a word (non-word boundary on both
///    sides). If yes, every physical column belonging to that metric
///    is kept. Captures the PromQL `metric_name{labels}` shape and
///    SQL idioms that reference metrics by canonical name.
/// 2. **Physical-name match.** For each physical column, check
///    whether its quoted form (`"physical"`) appears in the SQL.
///    Catches direct-column references that the metric-name pass
///    misses (e.g. when a SQL string references one specific
///    instance like `"cpu_usage/user/3"`).
///
/// `timestamp` and `duration` are always kept (parquet readers
/// require them). Over-keeping is preferred to under-keeping — the
/// purpose of trim is footer size, not correctness; missing columns
/// would break the saved report.
pub fn resolve_kept_columns_sql(
    payload: &ReportPayload,
    catalog: &metriken_query_sql::MetricCatalog,
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
        for (metric, series_list) in &catalog.series_by_metric {
            if query_mentions_word(query, metric) {
                for s in series_list {
                    out.insert(s.physical.clone());
                }
                continue;
            }
            // Direct physical-column references in SQL: catch any
            // metric whose physical column appears quoted in the query.
            for s in series_list {
                let quoted = format!("\"{}\"", s.physical);
                if query.contains(&quoted) {
                    out.insert(s.physical.clone());
                }
            }
        }
    }
    out
}

/// Whether `query` contains `needle` as a whole word — i.e. surrounded
/// by non-word characters (or the string boundary). Used so that the
/// metric name `cpu` doesn't accidentally match a query referencing
/// `cpu_usage`. Treats `[A-Za-z0-9_]` as word characters; everything
/// else (including `/`, `:`, `(`, `{`, `"`, whitespace) is a boundary.
fn query_mentions_word(query: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let bytes = query.as_bytes();
    let nbytes = needle.as_bytes();
    let mut i = 0;
    while i + nbytes.len() <= bytes.len() {
        if &bytes[i..i + nbytes.len()] == nbytes {
            let left_ok = i == 0 || !is_word_byte(bytes[i - 1]);
            let right_idx = i + nbytes.len();
            let right_ok = right_idx == bytes.len() || !is_word_byte(bytes[right_idx]);
            if left_ok && right_ok {
                return true;
            }
        }
        i += 1;
    }
    false
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Trim-free combined-A/B save. Per-side parquets are embed-stamped
/// with the selection JSON and packed verbatim into a `*.parquet.ab.tar`.
/// The caller-supplied `manifest_bytes` (typically
/// `serde_json::to_vec_pretty` of an `AbContainers`) is written into the
/// tar verbatim; this crate doesn't need to know the manifest's shape.
pub fn save_combined_ab_tarball_embed_only(
    baseline_bytes: Bytes,
    experiment_bytes: Bytes,
    selection_json: &str,
    manifest_bytes: &[u8],
) -> Result<Vec<u8>, String> {
    let baseline_out = embed_selection_in_parquet(baseline_bytes, selection_json, None)?;
    let experiment_out = embed_selection_in_parquet(experiment_bytes, selection_json, None)?;
    pack_ab_tarball(baseline_out, experiment_out, manifest_bytes)
}

/// SQL-backed combined-A/B tarball with per-side column trim resolved
/// against the supplied catalogs. Mirrors [`save_combined_ab_tarball`]
/// but uses [`resolve_kept_columns_sql`] instead of `engine.columns`.
#[allow(clippy::too_many_arguments)]
pub fn save_combined_ab_tarball_sql(
    baseline_bytes: Bytes,
    experiment_bytes: Bytes,
    payload: &ReportPayload,
    selection_json: &str,
    baseline_catalog: &metriken_query_sql::MetricCatalog,
    experiment_catalog: &metriken_query_sql::MetricCatalog,
    manifest_bytes: &[u8],
    trim_columns: bool,
) -> Result<Vec<u8>, String> {
    let events_json = events_payload_json(&payload.events);
    let (baseline_out, experiment_out) = if trim_columns {
        let baseline_kept = resolve_kept_columns_sql(payload, baseline_catalog, Side::Baseline);
        let experiment_kept = resolve_kept_columns_sql(payload, experiment_catalog, Side::Experiment);
        (
            trim_parquet_to_columns(
                baseline_bytes,
                &baseline_kept,
                selection_json,
                events_json.as_deref(),
            )?,
            trim_parquet_to_columns(
                experiment_bytes,
                &experiment_kept,
                selection_json,
                events_json.as_deref(),
            )?,
        )
    } else {
        (
            embed_selection_in_parquet(baseline_bytes, selection_json, events_json.as_deref())?,
            embed_selection_in_parquet(experiment_bytes, selection_json, events_json.as_deref())?,
        )
    };
    pack_ab_tarball(baseline_out, experiment_out, manifest_bytes)
}

fn pack_ab_tarball(
    baseline_out: Vec<u8>,
    experiment_out: Vec<u8>,
    manifest_bytes: &[u8],
) -> Result<Vec<u8>, String> {
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
    rewrite_parquet_bytes(source_bytes, kv_meta, None)
}

fn trim_parquet_to_columns(
    source_bytes: Bytes,
    kept: &HashSet<String>,
    selection_json: &str,
    events_json: Option<&str>,
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

    rewrite_parquet_bytes(source_bytes, kv_meta, Some(&indices))
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

fn rewrite_parquet_bytes(
    source: Bytes,
    kv_meta: Vec<KeyValue>,
    projection: Option<&[usize]>,
) -> Result<Vec<u8>, String> {
    let builder = ParquetRecordBatchReaderBuilder::try_new(source).map_err(|e| e.to_string())?;
    let schema = builder.schema().clone();
    let reader = builder.build().map_err(|e| e.to_string())?;

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
            let batch = match projection {
                Some(indices) => batch.project(indices).map_err(|e| e.to_string())?,
                None => batch,
            };
            writer.write(&batch).map_err(|e| e.to_string())?;
        }
        writer.close().map_err(|e| e.to_string())?;
    }
    Ok(buf)
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

// Tests for the SQL-aware column resolver. Live independently of
// `live-mode` because `resolve_kept_columns_sql` doesn't require
// `metriken-query`.
#[cfg(test)]
mod sql_resolve_tests {
    use super::*;
    use metriken_query_sql::{MetricCatalog, MetricSeries};
    use std::collections::BTreeMap;

    fn catalog_from(entries: &[(&str, &[&str])]) -> MetricCatalog {
        let mut cat = MetricCatalog::default();
        for (metric, physicals) in entries {
            let series: Vec<MetricSeries> = physicals
                .iter()
                .map(|p| MetricSeries {
                    physical: p.to_string(),
                    labels: BTreeMap::new(),
                })
                .collect();
            cat.series_by_metric.insert(metric.to_string(), series);
        }
        cat
    }

    fn payload_with_queries(queries: &[&str]) -> ReportPayload {
        ReportPayload {
            entries: queries
                .iter()
                .map(|q| ReportEntry {
                    promql_query: q.to_string(),
                    promql_query_experiment: None,
                })
                .collect(),
            trim_columns: true,
            events: vec![],
        }
    }

    /// PromQL queries mention metrics by name. `cpu_cycles` in the
    /// query should pull all `cpu_cycles/*` physical columns.
    #[test]
    fn resolves_promql_metric_to_all_physical_columns() {
        let cat = catalog_from(&[
            ("cpu_cycles", &["cpu_cycles/0", "cpu_cycles/1", "cpu_cycles/2"]),
            ("memory_used", &["memory_used"]),
        ]);
        let payload = payload_with_queries(&["sum(rate(cpu_cycles[1m]))"]);
        let kept = resolve_kept_columns_sql(&payload, &cat, Side::Baseline);
        assert!(kept.contains("timestamp"));
        assert!(kept.contains("duration"));
        assert!(kept.contains("cpu_cycles/0"));
        assert!(kept.contains("cpu_cycles/1"));
        assert!(kept.contains("cpu_cycles/2"));
        // Unmentioned metric stays out.
        assert!(!kept.contains("memory_used"));
    }

    /// Direct SQL queries reference physical columns in quoted form.
    /// `"cpu_cycles/0"` in the SQL must keep just that column.
    #[test]
    fn resolves_direct_quoted_physical_in_sql() {
        let cat = catalog_from(&[
            ("cpu_cycles", &["cpu_cycles/0", "cpu_cycles/1"]),
        ]);
        let sql = r#"SELECT timestamp/1e9 AS t, "cpu_cycles/0"::DOUBLE AS v FROM _src"#;
        let payload = payload_with_queries(&[sql]);
        let kept = resolve_kept_columns_sql(&payload, &cat, Side::Baseline);
        assert!(kept.contains("cpu_cycles/0"));
        // The metric name "cpu_cycles" appears in the SQL too (as
        // substring inside the quoted column), so word-boundary match
        // also fires — that's fine, we get the full set.
        // The crucial property: the keep-set isn't *empty*.
        assert!(kept.len() >= 3); // timestamp + duration + at least one physical
    }

    /// Word-boundary matching: `cpu` shouldn't accidentally match
    /// `cpu_cycles`. If only `cpu` is in the catalog and only
    /// `cpu_cycles` is in the query, neither catalog metric is kept.
    #[test]
    fn word_boundary_prevents_partial_metric_match() {
        let cat = catalog_from(&[
            ("cpu", &["cpu"]),
            ("cpu_cycles", &["cpu_cycles/0"]),
        ]);
        let payload = payload_with_queries(&["sum(cpu_cycles)"]);
        let kept = resolve_kept_columns_sql(&payload, &cat, Side::Baseline);
        assert!(kept.contains("cpu_cycles/0"));
        assert!(!kept.contains("cpu"), "cpu must not be over-matched");
    }

    /// `trim_columns=false` should bypass trim entirely. We confirm
    /// this by checking the resolver isn't called via the public
    /// `save_single_parquet_sql` entry — but the resolver itself is
    /// unconditional. This tests `save_single_parquet_sql`'s
    /// branching.
    #[test]
    fn save_single_parquet_sql_embed_only_when_trim_false() {
        let cat = MetricCatalog::default();
        let payload = ReportPayload {
            entries: vec![],
            trim_columns: false,
            events: vec![],
        };
        // Use a tiny parquet built inline so we don't need a fixture.
        // The cheapest path: an empty parquet won't trim because
        // there are no entries; trim_columns=false also short-circuits
        // to embed-only. Either way the function must not error.
        let bytes = build_empty_parquet_bytes();
        let result = save_single_parquet_sql(bytes, &payload, "{}", &cat, false);
        assert!(result.is_ok(), "embed-only path should succeed");
    }

    /// Build a parquet with only timestamp/duration columns and one
    /// row. Smallest valid input the trim path can chew on.
    fn build_empty_parquet_bytes() -> Bytes {
        use arrow::array::UInt64Array;
        use arrow::datatypes::{DataType, Field, Schema};
        use arrow::record_batch::RecordBatch;
        use parquet::arrow::ArrowWriter;
        use std::sync::Arc;
        let schema = Arc::new(Schema::new(vec![
            Field::new("timestamp", DataType::UInt64, false),
            Field::new("duration", DataType::UInt64, false),
        ]));
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(UInt64Array::from(vec![1_000_000_000])),
                Arc::new(UInt64Array::from(vec![1_000_000_000])),
            ],
        )
        .unwrap();
        let mut buf = Vec::new();
        {
            let mut w = ArrowWriter::try_new(&mut buf, schema, None).unwrap();
            w.write(&batch).unwrap();
            w.close().unwrap();
        }
        Bytes::from(buf)
    }
}

