//! Save-as-Report write-side helpers — column-trim a source parquet
//! (or a combined-A/B tarball's per-side parquets) down to just the
//! columns referenced by the saved selection's queries.

use std::collections::HashSet;
use std::ops::Deref;
use std::path::Path;
use std::sync::Arc;

use parking_lot::RwLock;

use metriken_query::{QueryEngine, Tsdb};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::file::metadata::KeyValue;
use serde::Deserialize;
use tracing::warn;

use crate::parquet_metadata::{KEY_DESCRIPTIONS, KEY_REPORT, KEY_SELECTION, REPORT_VALUE_TRIMMED};

/// Subset of the JSON body POSTed to `/api/v1/save_with_selection`
/// that the trim path actually consumes. The full body carries more
/// (tagline, anchors, chartToggles, time_range, …) — we ignore those
/// here because they don't influence which columns the report needs.
#[derive(Debug, Clone, Deserialize)]
pub struct ReportPayload {
    #[serde(default)]
    pub entries: Vec<ReportEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReportEntry {
    pub promql_query: String,
    #[serde(default)]
    pub promql_query_experiment: Option<String>,
}

/// Which side of an A/B compare a column-resolution call is for.
/// Drives whether `promql_query_experiment` overrides `promql_query`
/// when both are present on a Notebook entry.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Side {
    Baseline,
    Experiment,
}

/// Resolve every relevant query in the payload against the supplied
/// engine, union the returned column sets, and add `timestamp` +
/// `duration`. A query that parses but matches no series contributes
/// nothing (no error) — the matching column set is intentionally
/// empty in that case.
///
/// A query that fails to PARSE is silently skipped rather than aborting
/// the whole save, because the writer should produce *something* even
/// if a single chart's saved query is malformed. (We log the parse
/// error so the operator can diagnose.)
pub fn resolve_kept_columns<T: Deref<Target = Tsdb>>(
    payload: &ReportPayload,
    engine: &QueryEngine<T>,
    side: Side,
) -> HashSet<String> {
    let mut out = HashSet::new();
    out.insert("timestamp".to_string());
    out.insert("duration".to_string());
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
            Err(e) => {
                warn!("report-save: failed to resolve columns for query {query:?}: {e}");
            }
        }
    }
    out
}

/// Trim a parquet file to just the columns whose names appear in `kept`.
/// Matches against the field name, the `base` before `:suffix`, or the
/// `metric` metadata key — same predicate `parquet filter` uses.
///
/// Stamps the output with `KEY_REPORT = "trimmed"` and the caller's
/// `selection_json` in the footer KV. Filters `descriptions` to kept names.
pub fn trim_parquet_to_columns(
    source_path: &Path,
    kept: &HashSet<String>,
    selection_json: &str,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let builder = ParquetRecordBatchReaderBuilder::try_new(std::fs::File::open(source_path)?)?;
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
        return Err("trim produced an empty column set (source missing timestamp?)".into());
    }

    let mut kv_meta = crate::parquet_tools::read_file_metadata(source_path)?;
    kv_meta.retain(|kv| kv.key != KEY_SELECTION && kv.key != KEY_REPORT);
    kv_meta.push(KeyValue {
        key: KEY_REPORT.to_string(),
        value: Some(REPORT_VALUE_TRIMMED.to_string()),
    });
    kv_meta.push(KeyValue {
        key: KEY_SELECTION.to_string(),
        value: Some(selection_json.to_string()),
    });

    let kept_names: std::collections::BTreeSet<&str> = indices
        .iter()
        .flat_map(|&i| {
            let f = schema.field(i);
            let mut names = vec![f.name().as_str()];
            if let Some(m) = f.metadata().get("metric") {
                names.push(m.as_str());
            }
            names
        })
        .collect();
    filter_descriptions(&mut kv_meta, &kept_names);

    crate::parquet_tools::rewrite_parquet(source_path, kv_meta, Some(&indices))
}

/// Mirror `parquet filter`'s field-keep predicate: exact match, base
/// before `:`, or `metric` metadata fallback.
fn keep_field(f: &arrow::datatypes::Field, kept: &HashSet<String>) -> bool {
    let name = f.name();
    if kept.contains(name) {
        return true;
    }
    if name
        .split_once(':')
        .is_some_and(|(base, _)| kept.contains(base))
    {
        return true;
    }
    if let Some(metric) = f.metadata().get("metric") {
        if kept.contains(metric) {
            return true;
        }
    }
    false
}

fn filter_descriptions(kv_meta: &mut [KeyValue], kept_names: &std::collections::BTreeSet<&str>) {
    if let Some(entry) = kv_meta.iter_mut().find(|kv| kv.key == KEY_DESCRIPTIONS) {
        if let Some(value) = &entry.value {
            if let Ok(mut map) =
                serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(value)
            {
                map.retain(|k, _| kept_names.contains(k.as_str()));
                if let Ok(filtered) = serde_json::to_string(&map) {
                    entry.value = Some(filtered);
                }
            }
        }
    }
}

/// Trim a combined-A/B tarball's per-side parquets and repack. Returns a
/// POSIX tar (`baseline.parquet` + `experiment.parquet` + `ab.json`). The
/// manifest is written through unchanged — alias, sources, and category
/// survive the round-trip.
#[allow(clippy::too_many_arguments)]
pub fn trim_combined_ab_to_tarball(
    baseline_path: &std::path::Path,
    experiment_path: &std::path::Path,
    payload: &ReportPayload,
    selection_json: &str,
    baseline_tsdb: &Arc<RwLock<Tsdb>>,
    experiment_tsdb: &Arc<RwLock<Tsdb>>,
    manifest: &crate::parquet_metadata::AbContainers,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let baseline_kept = {
        let t = baseline_tsdb.read();
        let engine = QueryEngine::new(&*t);
        resolve_kept_columns(payload, &engine, Side::Baseline)
    };
    let experiment_kept = {
        let t = experiment_tsdb.read();
        let engine = QueryEngine::new(&*t);
        resolve_kept_columns(payload, &engine, Side::Experiment)
    };

    let baseline_bytes = trim_parquet_to_columns(baseline_path, &baseline_kept, selection_json)?;
    let experiment_bytes =
        trim_parquet_to_columns(experiment_path, &experiment_kept, selection_json)?;
    let manifest_bytes = serde_json::to_vec_pretty(manifest)?;

    let mut buf: Vec<u8> = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut buf);
        builder.mode(tar::HeaderMode::Deterministic);
        append_tar_entry(&mut builder, "baseline.parquet", &baseline_bytes)?;
        append_tar_entry(&mut builder, "experiment.parquet", &experiment_bytes)?;
        append_tar_entry(&mut builder, "ab.json", &manifest_bytes)?;
        builder.into_inner()?;
    }
    Ok(buf)
}

fn append_tar_entry<W: std::io::Write>(
    builder: &mut tar::Builder<W>,
    name: &str,
    data: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    let mut header = tar::Header::new_gnu();
    header.set_path(name)?;
    header.set_size(data.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    builder.append(&header, data)?;
    Ok(())
}

/// HTTP-friendly wrapper for the single-parquet trim path. Resolves kept
/// columns against the supplied Tsdb, trims the source parquet, and returns
/// the bytes ready to stream. The original JSON body is embedded verbatim
/// under `selection` in the output footer.
pub fn trim_single_parquet(
    source_path: &std::path::Path,
    payload: &ReportPayload,
    selection_json: &str,
    tsdb: &Arc<RwLock<Tsdb>>,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let tsdb_read = tsdb.read();
    let engine = QueryEngine::new(&*tsdb_read);
    let kept = resolve_kept_columns(payload, &engine, Side::Baseline);
    drop(tsdb_read);
    trim_parquet_to_columns(source_path, &kept, selection_json)
}

#[cfg(test)]
mod tests {
    use super::*;

    use arrow::array::{Int64Array, UInt64Array};
    use arrow::datatypes::{DataType, Field, Schema};
    use arrow::record_batch::RecordBatch;
    use metriken_query::{QueryEngine, Tsdb};
    use parquet::arrow::ArrowWriter;
    use parquet::file::metadata::KeyValue;
    use parquet::file::properties::WriterProperties;
    use std::sync::Arc;

    /// Write a tiny single-source parquet with timestamp + duration + two
    /// gauge columns (`m_a` and `m_b`) and load it as a Tsdb. Returns the
    /// loaded Tsdb plus the temp file holding the bytes (kept alive so
    /// Tsdb references stay valid where needed).
    fn build_test_tsdb() -> (Tsdb, tempfile::NamedTempFile) {
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
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let mut writer = ArrowWriter::try_new(tmp.reopen().unwrap(), schema, Some(props)).unwrap();
        writer.write(&batch).unwrap();
        writer.close().unwrap();
        let tsdb = Tsdb::load(tmp.path()).expect("tsdb loads");
        (tsdb, tmp)
    }

    #[test]
    fn baseline_side_kept_set_includes_timestamp_and_duration() {
        let (tsdb, _tmp) = build_test_tsdb();
        let engine = QueryEngine::new(&tsdb);
        let payload = ReportPayload {
            entries: vec![ReportEntry {
                promql_query: "m_a".into(),
                promql_query_experiment: None,
            }],
        };
        let kept = resolve_kept_columns(&payload, &engine, Side::Baseline);
        assert!(kept.contains("timestamp"), "kept must include timestamp");
        assert!(kept.contains("duration"), "kept must include duration");
        assert!(kept.contains("m_a"), "kept must include the queried column");
        assert!(
            !kept.contains("m_b"),
            "kept must NOT include unqueried columns"
        );
    }

    #[test]
    fn experiment_side_falls_back_to_promql_query_when_experiment_unset() {
        let (tsdb, _tmp) = build_test_tsdb();
        let engine = QueryEngine::new(&tsdb);
        let payload = ReportPayload {
            entries: vec![ReportEntry {
                promql_query: "m_a".into(),
                promql_query_experiment: None,
            }],
        };
        let kept = resolve_kept_columns(&payload, &engine, Side::Experiment);
        assert!(kept.contains("m_a"));
        assert!(!kept.contains("m_b"));
    }

    #[test]
    fn experiment_side_uses_promql_query_experiment_when_set() {
        let (tsdb, _tmp) = build_test_tsdb();
        let engine = QueryEngine::new(&tsdb);
        let payload = ReportPayload {
            entries: vec![ReportEntry {
                promql_query: "m_a".into(),
                promql_query_experiment: Some("m_b".into()),
            }],
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
                {
                    "chartId": "c2",
                    "promql_query": "cpu_usage",
                    "promql_query_experiment": "cpu_usage{state=\"user\"}"
                }
            ]
        }"#;
        let payload: ReportPayload = serde_json::from_str(json).unwrap();
        assert_eq!(payload.entries.len(), 2);
        assert_eq!(payload.entries[0].promql_query, "cpu_cores");
        assert_eq!(payload.entries[1].promql_query, "cpu_usage");
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

    use crate::parquet_metadata::{KEY_REPORT, REPORT_VALUE_TRIMMED};
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    use parquet::file::reader::FileReader;
    use parquet::file::serialized_reader::SerializedFileReader;

    #[test]
    fn single_parquet_round_trip_trims_to_one_column() {
        let (tsdb, tmp) = build_test_tsdb();
        let tsdb_arc = std::sync::Arc::new(parking_lot::RwLock::new(tsdb));
        let payload = ReportPayload {
            entries: vec![ReportEntry {
                promql_query: "m_a".into(),
                promql_query_experiment: None,
            }],
        };
        let body = r#"{"version":1,"entries":[{"chartId":"c","promql_query":"m_a"}]}"#;
        let out = trim_single_parquet(tmp.path(), &payload, body, &tsdb_arc)
            .expect("trim_single_parquet succeeds");
        let verify = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(verify.path(), &out).unwrap();
        let builder =
            ParquetRecordBatchReaderBuilder::try_new(std::fs::File::open(verify.path()).unwrap())
                .unwrap();
        let names: Vec<String> = builder
            .schema()
            .fields()
            .iter()
            .map(|f| f.name().clone())
            .collect();
        assert_eq!(names, vec!["timestamp", "duration", "m_a"]);
    }

    #[test]
    fn trim_keeps_only_named_columns_plus_timestamp_duration() {
        let (_tsdb, tmp) = build_test_tsdb();
        let mut kept = HashSet::new();
        kept.insert("timestamp".to_string());
        kept.insert("duration".to_string());
        kept.insert("m_a".to_string());

        let selection = r#"{"version": 1, "entries": []}"#;
        let out = trim_parquet_to_columns(tmp.path(), &kept, selection).expect("trim succeeds");

        // Parse the output and confirm schema + footer.
        // Write to a temp file so we can use File-based readers.
        let out_tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(out_tmp.path(), &out).unwrap();
        let builder =
            ParquetRecordBatchReaderBuilder::try_new(std::fs::File::open(out_tmp.path()).unwrap())
                .unwrap();
        let names: Vec<String> = builder
            .schema()
            .fields()
            .iter()
            .map(|f| f.name().clone())
            .collect();
        assert_eq!(names, vec!["timestamp", "duration", "m_a"]);

        // Footer carries the report marker + the new selection.
        let reader =
            SerializedFileReader::new(std::fs::File::open(out_tmp.path()).unwrap()).unwrap();
        let kv = reader
            .metadata()
            .file_metadata()
            .key_value_metadata()
            .cloned()
            .unwrap();
        let report = kv
            .iter()
            .find(|kv| kv.key == KEY_REPORT)
            .and_then(|kv| kv.value.as_deref())
            .unwrap();
        assert_eq!(report, REPORT_VALUE_TRIMMED);
        let sel = kv
            .iter()
            .find(|kv| kv.key == "selection")
            .and_then(|kv| kv.value.as_deref())
            .unwrap();
        assert_eq!(sel, selection);
    }

    #[test]
    fn combined_ab_round_trip_trims_each_side_and_repacks() {
        use crate::parquet_metadata::{AbContainers, AbSide};

        let (tsdb_a, tmp_a) = build_test_tsdb();
        let (tsdb_b, tmp_b) = build_test_tsdb();
        let tsdb_a = std::sync::Arc::new(parking_lot::RwLock::new(tsdb_a));
        let tsdb_b = std::sync::Arc::new(parking_lot::RwLock::new(tsdb_b));

        let payload = ReportPayload {
            entries: vec![ReportEntry {
                promql_query: "m_a".into(),
                promql_query_experiment: Some("m_b".into()),
            }],
        };
        let body = r#"{"version":1,"entries":[]}"#;
        let manifest = AbContainers {
            version: AbContainers::SCHEMA_VERSION,
            baseline: AbSide {
                alias: "a".into(),
                sources: vec!["svc".into()],
            },
            experiment: AbSide {
                alias: "b".into(),
                sources: vec!["svc".into()],
            },
            category: None,
        };

        let out = trim_combined_ab_to_tarball(
            tmp_a.path(),
            tmp_b.path(),
            &payload,
            body,
            &tsdb_a,
            &tsdb_b,
            &manifest,
        )
        .expect("AB trim succeeds");

        // Verify tar contains the three expected entries.
        let mut archive = tar::Archive::new(std::io::Cursor::new(&out));
        let mut names = Vec::new();
        let mut baseline_bytes: Vec<u8> = Vec::new();
        let mut experiment_bytes: Vec<u8> = Vec::new();
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            let path = entry.path().unwrap().to_path_buf();
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            names.push(name.clone());
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
        assert!(names.iter().any(|n| n == "baseline.parquet"));
        assert!(names.iter().any(|n| n == "experiment.parquet"));
        assert!(names.iter().any(|n| n == "ab.json"));

        // Write to tempfiles to use File-based parquet readers.
        let verify_b = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(verify_b.path(), &baseline_bytes).unwrap();
        let verify_e = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(verify_e.path(), &experiment_bytes).unwrap();

        let b_schema =
            ParquetRecordBatchReaderBuilder::try_new(std::fs::File::open(verify_b.path()).unwrap())
                .unwrap()
                .schema()
                .clone();
        let e_schema =
            ParquetRecordBatchReaderBuilder::try_new(std::fs::File::open(verify_e.path()).unwrap())
                .unwrap()
                .schema()
                .clone();
        let b_names: Vec<&str> = b_schema
            .fields()
            .iter()
            .map(|f| f.name().as_str())
            .collect();
        let e_names: Vec<&str> = e_schema
            .fields()
            .iter()
            .map(|f| f.name().as_str())
            .collect();
        assert_eq!(b_names, vec!["timestamp", "duration", "m_a"]);
        assert_eq!(e_names, vec!["timestamp", "duration", "m_b"]);

        // Each per-side parquet must carry KEY_REPORT = "trimmed".
        use parquet::file::reader::FileReader;
        use parquet::file::serialized_reader::SerializedFileReader;
        for verify in [&verify_b, &verify_e] {
            let reader =
                SerializedFileReader::new(std::fs::File::open(verify.path()).unwrap()).unwrap();
            let kv = reader
                .metadata()
                .file_metadata()
                .key_value_metadata()
                .cloned()
                .unwrap();
            let report = kv
                .iter()
                .find(|kv| kv.key == KEY_REPORT)
                .and_then(|kv| kv.value.as_deref())
                .unwrap();
            assert_eq!(report, REPORT_VALUE_TRIMMED);
        }
    }
}
