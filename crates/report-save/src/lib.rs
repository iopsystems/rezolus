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
use std::ops::Deref;

use arrow::datatypes::Field;
use metriken_query::{Bytes, QueryEngine, Tsdb};
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

// ── Column resolution ────────────────────────────────────────────────

/// Resolve every entry's query against `engine`, union the returned
/// columns, and always keep `timestamp` + `duration` (which
/// `engine.columns` never returns but `Tsdb::load` requires).
///
/// Queries that fail to PARSE are logged and skipped — one malformed
/// chart shouldn't abort the whole save. Queries that parse but match
/// no series contribute nothing.
pub fn resolve_kept_columns<T: Deref<Target = Tsdb>>(
    payload: &ReportPayload,
    engine: &QueryEngine<T>,
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
        match engine.columns(query) {
            Ok(cols) => out.extend(cols),
            Err(e) => warn!("report-save: skipped malformed query {query:?}: {e}"),
        }
    }
    out
}

// ── Top-level save entry points ──────────────────────────────────────

/// Project the source parquet down to the saved selection's columns
/// (when `trim_columns` is true), or just embed the selection JSON in
/// the footer (when false). Returns the new parquet bytes ready to
/// stream / download.
pub fn save_single_parquet(
    source_bytes: Bytes,
    payload: &ReportPayload,
    selection_json: &str,
    tsdb: &Tsdb,
    trim_columns: bool,
) -> Result<Vec<u8>, String> {
    if trim_columns {
        let engine = QueryEngine::new(tsdb);
        let kept = resolve_kept_columns(payload, &engine, Side::Baseline);
        trim_parquet_to_columns(source_bytes, &kept, selection_json)
    } else {
        embed_selection_in_parquet(source_bytes, selection_json)
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
    baseline_tsdb: &Tsdb,
    experiment_tsdb: &Tsdb,
    manifest_bytes: &[u8],
    trim_columns: bool,
) -> Result<Vec<u8>, String> {
    let (baseline_out, experiment_out) = if trim_columns {
        let baseline_kept = {
            let engine = QueryEngine::new(baseline_tsdb);
            resolve_kept_columns(payload, &engine, Side::Baseline)
        };
        let experiment_kept = {
            let engine = QueryEngine::new(experiment_tsdb);
            resolve_kept_columns(payload, &engine, Side::Experiment)
        };
        (
            trim_parquet_to_columns(baseline_bytes, &baseline_kept, selection_json)?,
            trim_parquet_to_columns(experiment_bytes, &experiment_kept, selection_json)?,
        )
    } else {
        (
            embed_selection_in_parquet(baseline_bytes, selection_json)?,
            embed_selection_in_parquet(experiment_bytes, selection_json)?,
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
) -> Result<Vec<u8>, String> {
    let mut kv_meta = read_file_metadata(source_bytes.clone())?;
    kv_meta.retain(|kv| kv.key != KEY_SELECTION);
    kv_meta.push(KeyValue {
        key: KEY_SELECTION.to_string(),
        value: Some(selection_json.to_string()),
    });
    rewrite_parquet_bytes(source_bytes, kv_meta, None)
}

fn trim_parquet_to_columns(
    source_bytes: Bytes,
    kept: &HashSet<String>,
    selection_json: &str,
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
    kv_meta.retain(|kv| kv.key != KEY_SELECTION && kv.key != KEY_REPORT);
    kv_meta.push(KeyValue {
        key: KEY_REPORT.to_string(),
        value: Some(REPORT_VALUE_TRIMMED.to_string()),
    });
    kv_meta.push(KeyValue {
        key: KEY_SELECTION.to_string(),
        value: Some(selection_json.to_string()),
    });

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

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::{Int64Array, UInt64Array};
    use arrow::datatypes::{DataType, Field, Schema};
    use arrow::record_batch::RecordBatch;
    use std::sync::Arc;

    /// Build a tiny single-source parquet (timestamp, duration, m_a,
    /// m_b) entirely in memory and return both its bytes and the loaded
    /// Tsdb. The Tsdb keeps an Arc on the bytes internally; we hand
    /// back the bytes too so callers can pass them to save_*.
    fn build_test(parquet: bool) -> (Bytes, Tsdb) {
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
        let tsdb = Tsdb::load_from_bytes(bytes.clone()).expect("tsdb loads");
        let _ = parquet;
        (bytes, tsdb)
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
        let (_bytes, tsdb) = build_test(true);
        let engine = QueryEngine::new(&tsdb);
        let payload = ReportPayload {
            entries: vec![ReportEntry {
                promql_query: "m_a".into(),
                promql_query_experiment: None,
            }],
            trim_columns: true,
        };
        let kept = resolve_kept_columns(&payload, &engine, Side::Baseline);
        assert!(kept.contains("timestamp"));
        assert!(kept.contains("duration"));
        assert!(kept.contains("m_a"));
        assert!(!kept.contains("m_b"));
    }

    #[test]
    fn experiment_side_falls_back_to_promql_query_when_experiment_unset() {
        let (_bytes, tsdb) = build_test(true);
        let engine = QueryEngine::new(&tsdb);
        let payload = ReportPayload {
            entries: vec![ReportEntry {
                promql_query: "m_a".into(),
                promql_query_experiment: None,
            }],
            trim_columns: true,
        };
        let kept = resolve_kept_columns(&payload, &engine, Side::Experiment);
        assert!(kept.contains("m_a"));
        assert!(!kept.contains("m_b"));
    }

    #[test]
    fn experiment_side_uses_promql_query_experiment_when_set() {
        let (_bytes, tsdb) = build_test(true);
        let engine = QueryEngine::new(&tsdb);
        let payload = ReportPayload {
            entries: vec![ReportEntry {
                promql_query: "m_a".into(),
                promql_query_experiment: Some("m_b".into()),
            }],
            trim_columns: true,
        };
        let kept_b = resolve_kept_columns(&payload, &engine, Side::Baseline);
        let kept_e = resolve_kept_columns(&payload, &engine, Side::Experiment);
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
        let (bytes, tsdb) = build_test(true);
        let payload = ReportPayload {
            entries: vec![ReportEntry {
                promql_query: "m_a".into(),
                promql_query_experiment: None,
            }],
            trim_columns: true,
        };
        let body = r#"{"version":1,"entries":[{"chartId":"c","promql_query":"m_a"}]}"#;
        let out = save_single_parquet(bytes, &payload, body, &tsdb, true).unwrap();
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
        let (bytes, tsdb) = build_test(true);
        let payload = ReportPayload {
            entries: vec![ReportEntry {
                promql_query: "m_a".into(),
                promql_query_experiment: None,
            }],
            trim_columns: false,
        };
        let selection = r#"{"version":1,"entries":[{"chartId":"c","promql_query":"m_a"}]}"#;
        let out = save_single_parquet(bytes, &payload, selection, &tsdb, false).unwrap();
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
    fn combined_ab_round_trip_trims_each_side_and_repacks() {
        let (bytes_a, tsdb_a) = build_test(true);
        let (bytes_b, tsdb_b) = build_test(true);
        let payload = ReportPayload {
            entries: vec![ReportEntry {
                promql_query: "m_a".into(),
                promql_query_experiment: Some("m_b".into()),
            }],
            trim_columns: true,
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
            &tsdb_a,
            &tsdb_b,
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
}
