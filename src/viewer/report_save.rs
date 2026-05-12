//! Save-as-Report write-side helpers — column-trim a source parquet
//! (or a combined-A/B tarball's per-side parquets) down to just the
//! columns referenced by the saved selection's queries.

use std::collections::HashSet;
use std::ops::Deref;

use metriken_query::{QueryEngine, Tsdb};
use serde::Deserialize;
use tracing::warn;

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
                warn!(
                    "report-save: failed to resolve columns for query {query:?}: {e}"
                );
            }
        }
    }
    out
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
            KeyValue { key: "source".into(), value: Some("svc".into()) },
            KeyValue { key: "sampling_interval_ms".into(), value: Some("1000".into()) },
        ];
        let props = WriterProperties::builder()
            .set_key_value_metadata(Some(kv))
            .build();
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let mut writer =
            ArrowWriter::try_new(tmp.reopen().unwrap(), schema, Some(props)).unwrap();
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
        assert!(!kept.contains("m_b"), "kept must NOT include unqueried columns");
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
}
