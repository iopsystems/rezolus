use clap::ArgMatches;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::parquet_metadata::{KEY_NODE, KEY_SERVICE_QUERIES, KEY_SOURCE};
use crate::viewer::promql::QueryEngine;
use crate::viewer::tsdb::Tsdb;
use crate::viewer::{ServiceExtension, TemplateRegistry};

pub(super) fn run(args: &ArgMatches, registry: &TemplateRegistry) {
    let path = args.get_one::<PathBuf>("FILE").unwrap();
    let node = args.get_one::<String>("node");
    let new_source = args.get_one::<String>("source");
    let overwrite = args.get_flag("overwrite");

    if let Some(n) = node {
        set_node_metadata(path, n).unwrap_or_else(|e| {
            eprintln!("error: failed to set node metadata: {e}");
            std::process::exit(1);
        });
        println!("Set node={:?} on {:?}", n, path);
    }

    if args.get_flag("undo") {
        run_undo(path);
        return;
    }

    if let Some(src) = new_source {
        set_source_metadata(path, src, overwrite).unwrap_or_else(|e| {
            eprintln!("error: {e}");
            std::process::exit(1);
        });
        println!("Set source={:?} on {:?}", src, path);
    }

    let custom_file = args.get_one::<PathBuf>("queries");

    // If only --node and/or --source were requested, don't also auto-apply
    // a default service template.
    if (node.is_some() || new_source.is_some()) && custom_file.is_none() && !args.get_flag("filter")
    {
        return;
    }

    let source = read_source_metadata(path).unwrap_or_else(|| {
        eprintln!(
            "error: parquet file has no 'source' metadata. Use --queries to provide a template."
        );
        std::process::exit(1);
    });

    let json = if let Some(custom_path) = custom_file {
        let content =
            std::fs::read_to_string(custom_path).expect("failed to read service extension file");
        let _: ServiceExtension =
            serde_json::from_str(&content).expect("invalid service extension JSON");
        content
    } else {
        let template = registry.get(&source).unwrap_or_else(|| {
            eprintln!(
                "error: no template for source {:?}. Use --queries to provide one.",
                source
            );
            std::process::exit(1);
        });
        serde_json::to_string(template).expect("failed to serialize service extension template")
    };

    let mut ext: ServiceExtension = serde_json::from_str(&json).unwrap();

    // Validate KPI queries against the parquet data and set available flags
    validate_kpis(path, &mut ext);

    let annotated_json =
        serde_json::to_string(&ext).expect("failed to serialize service extension");
    annotate_parquet(path, &annotated_json).expect("failed to annotate parquet file");

    println!(
        "Annotated {:?} with {:?} service queries ({} KPIs)",
        path,
        ext.service_name,
        ext.kpis.len()
    );

    if args.get_flag("filter") {
        if let Err(e) = super::filter::filter_parquet_file(path, &ext, None) {
            eprintln!("error: failed to filter columns: {e}");
            std::process::exit(1);
        }
    }
}

/// Remove service_queries from all sources in per_source_metadata.
fn run_undo(path: &Path) {
    unannotate_parquet(path).unwrap_or_else(|e| {
        eprintln!("error: failed to remove annotation: {e}");
        std::process::exit(1);
    });
    println!("Removed service extension annotation from {:?}", path);
}

/// Validate that each KPI query returns data from the parquet file.
/// Sets `available` on each KPI based on whether its query returns data.
/// Prints warnings for unavailable KPIs and exits if none match.
fn validate_kpis(path: &Path, ext: &mut ServiceExtension) {
    let tsdb = match Tsdb::load(path) {
        Ok(tsdb) => Arc::new(tsdb),
        Err(e) => {
            eprintln!("warning: could not load parquet for validation: {e}");
            return;
        }
    };

    let engine = QueryEngine::new(tsdb);
    let (start, end) = engine.get_time_range();
    let step = 1.0;

    let mut matched = 0;
    let mut missing_metrics = BTreeSet::new();

    for kpi in &mut ext.kpis {
        let query = kpi.effective_query();
        let has_data = match engine.query_range(&query, start, end, step) {
            Ok(result) => !query_result_is_empty(&result),
            Err(_) => false,
        };
        if !has_data {
            missing_metrics.extend(extract_metric_selectors(&kpi.query));
        }
        kpi.available = has_data;
        if has_data {
            matched += 1;
        }
    }

    if !missing_metrics.is_empty() {
        eprintln!("missing metrics:");
        for name in &missing_metrics {
            eprintln!("  - {name}");
        }
    }

    if matched == 0 {
        eprintln!("error: no KPI queries matched any data in the parquet file");
        std::process::exit(1);
    }

    println!(
        "Validated: {matched}/{} KPIs have matching data",
        ext.kpis.len()
    );
}

/// Extract metric selectors (name + optional labels) from a PromQL query.
///
/// Matches `metric_name` or `metric_name{labels...}`, skipping anything
/// followed by `(` (i.e. function calls like `sum(`, `irate(`).
pub(super) fn extract_metric_selectors(query: &str) -> BTreeSet<String> {
    use regex::Regex;
    use std::sync::LazyLock;

    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"[a-zA-Z_][a-zA-Z0-9_]*(\{[^}]*\})?").unwrap());

    RE.find_iter(query)
        .filter(|m| {
            // Skip duration suffixes like 5s, 1m, 1h (preceded by a digit)
            if m.start() > 0 && query.as_bytes()[m.start() - 1].is_ascii_digit() {
                return false;
            }
            // Skip function calls: next non-whitespace char after match is '('
            query[m.end()..].trim_start().as_bytes().first() != Some(&b'(')
        })
        .map(|m| m.as_str().to_string())
        .collect()
}

fn query_result_is_empty(result: &crate::viewer::promql::QueryResult) -> bool {
    use crate::viewer::promql::QueryResult;
    match result {
        QueryResult::Vector { result } => result.is_empty(),
        QueryResult::Matrix { result } => result.is_empty(),
        QueryResult::Scalar { .. } => false,
        QueryResult::HistogramHeatmap { result } => result.data.is_empty(),
    }
}

pub(super) fn read_source_metadata(path: &Path) -> Option<String> {
    use parquet::file::reader::FileReader;
    use parquet::file::serialized_reader::SerializedFileReader;

    let file = std::fs::File::open(path).ok()?;
    let reader = SerializedFileReader::new(file).ok()?;
    let kv = reader.metadata().file_metadata().key_value_metadata()?;

    kv.iter()
        .find(|kv| kv.key == KEY_SOURCE)
        .and_then(|kv| kv.value.clone())
}

fn annotate_parquet(
    path: &Path,
    service_queries_json: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    use parquet::file::metadata::KeyValue;

    let mut kv_meta = super::read_file_metadata(path)?;

    kv_meta.retain(|kv| kv.key != KEY_SERVICE_QUERIES);
    kv_meta.push(KeyValue {
        key: KEY_SERVICE_QUERIES.to_string(),
        value: Some(service_queries_json.to_string()),
    });

    let buf = super::rewrite_parquet(path, kv_meta, None)?;
    std::fs::write(path, &buf)?;
    Ok(())
}

/// Set (or replace) the top-level `source` key in parquet metadata.
///
/// - If no `source` exists: writes the value.
/// - If `source` matches `value`: no-op (idempotent).
/// - If `source` differs from `value` and `overwrite=false`: returns an error.
/// - If `source` differs and `overwrite=true`: replaces the value.
fn set_source_metadata(
    path: &Path,
    value: &str,
    overwrite: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use parquet::file::metadata::KeyValue;

    let mut kv_meta = super::read_file_metadata(path)?;

    let existing = kv_meta
        .iter()
        .find(|kv| kv.key == KEY_SOURCE)
        .and_then(|kv| kv.value.as_deref());

    match existing {
        Some(cur) if cur == value => return Ok(()),
        Some(cur) if !overwrite => {
            return Err(format!(
                "file already has source={:?}; pass --overwrite to replace it with {:?}",
                cur, value
            )
            .into());
        }
        _ => {}
    }

    kv_meta.retain(|kv| kv.key != KEY_SOURCE);
    kv_meta.push(KeyValue {
        key: KEY_SOURCE.to_string(),
        value: Some(value.to_string()),
    });

    let buf = super::rewrite_parquet(path, kv_meta, None)?;
    std::fs::write(path, &buf)?;
    Ok(())
}

/// Set (or replace) the top-level `node` key in parquet metadata.
fn set_node_metadata(path: &Path, node: &str) -> Result<(), Box<dyn std::error::Error>> {
    use parquet::file::metadata::KeyValue;

    let mut kv_meta = super::read_file_metadata(path)?;
    kv_meta.retain(|kv| kv.key != KEY_NODE);
    kv_meta.push(KeyValue {
        key: KEY_NODE.to_string(),
        value: Some(node.to_string()),
    });

    let buf = super::rewrite_parquet(path, kv_meta, None)?;
    std::fs::write(path, &buf)?;
    Ok(())
}

/// Remove the top-level `service_queries` key from parquet metadata.
fn unannotate_parquet(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut kv_meta = super::read_file_metadata(path)?;

    let before = kv_meta.len();
    kv_meta.retain(|kv| kv.key != KEY_SERVICE_QUERIES);

    if kv_meta.len() == before {
        eprintln!("warning: no service_queries annotation found");
        return Ok(());
    }

    let buf = super::rewrite_parquet(path, kv_meta, None)?;
    std::fs::write(path, &buf)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_selectors_from_counter_query() {
        let q = r#"sum(irate(tokens{direction="output"}[5s]))"#;
        let sel: Vec<_> = extract_metric_selectors(q).into_iter().collect();
        assert_eq!(sel, vec![r#"tokens{direction="output"}"#]);
    }

    #[test]
    fn extract_selectors_from_ratio_query() {
        let q =
            r#"sum(irate(requests{status="error"}[5s])) / sum(irate(requests{status="sent"}[5s]))"#;
        let sel: Vec<_> = extract_metric_selectors(q).into_iter().collect();
        assert_eq!(
            sel,
            vec![r#"requests{status="error"}"#, r#"requests{status="sent"}"#]
        );
    }

    #[test]
    fn extract_selectors_from_bare_metric() {
        let sel: Vec<_> = extract_metric_selectors("requests_inflight")
            .into_iter()
            .collect();
        assert_eq!(sel, vec!["requests_inflight"]);
    }

    #[test]
    fn extract_selectors_from_histogram() {
        let sel: Vec<_> = extract_metric_selectors("ttft").into_iter().collect();
        assert_eq!(sel, vec!["ttft"]);
    }

    // ── set_node_metadata tests ──

    use arrow::array::{Int64Array, UInt64Array};
    use arrow::datatypes::{DataType, Field, Schema};
    use arrow::record_batch::RecordBatch;
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    use parquet::arrow::ArrowWriter;
    use parquet::file::metadata::KeyValue;
    use parquet::file::properties::WriterProperties;
    use parquet::file::reader::FileReader;
    use parquet::file::serialized_reader::SerializedFileReader;
    use std::collections::HashMap;
    use std::sync::Arc;
    use tempfile::NamedTempFile;

    fn make_minimal_parquet(initial_kv: Vec<(&str, &str)>) -> NamedTempFile {
        let ts_field = Field::new("timestamp", DataType::UInt64, false);
        let metric_field = Field::new("m", DataType::Int64, true).with_metadata(HashMap::from([(
            "metric_type".to_string(),
            "gauge".to_string(),
        )]));
        let schema = Arc::new(Schema::new(vec![ts_field, metric_field]));

        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(UInt64Array::from(vec![1u64, 2, 3])),
                Arc::new(Int64Array::from(vec![Some(10), Some(20), Some(30)])),
            ],
        )
        .unwrap();

        let kv: Vec<KeyValue> = initial_kv
            .into_iter()
            .map(|(k, v)| KeyValue {
                key: k.to_string(),
                value: Some(v.to_string()),
            })
            .collect();
        let props = WriterProperties::builder()
            .set_key_value_metadata(Some(kv))
            .build();

        let tmp = NamedTempFile::new().unwrap();
        let file = std::fs::File::create(tmp.path()).unwrap();
        let mut writer = ArrowWriter::try_new(file, schema, Some(props)).unwrap();
        writer.write(&batch).unwrap();
        writer.close().unwrap();
        tmp
    }

    fn read_kv(path: &std::path::Path) -> Vec<(String, String)> {
        let reader = SerializedFileReader::new(std::fs::File::open(path).unwrap()).unwrap();
        reader
            .metadata()
            .file_metadata()
            .key_value_metadata()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|kv| (kv.key, kv.value.unwrap_or_default()))
            .collect()
    }

    #[test]
    fn set_node_adds_key_when_missing() {
        let tmp = make_minimal_parquet(vec![("source", "rezolus")]);
        set_node_metadata(tmp.path(), "web01").unwrap();

        let kv = read_kv(tmp.path());
        assert!(kv.iter().any(|(k, v)| k == KEY_NODE && v == "web01"));
        // Pre-existing keys are preserved
        assert!(kv.iter().any(|(k, v)| k == KEY_SOURCE && v == "rezolus"));
    }

    #[test]
    fn set_node_replaces_existing_value() {
        let tmp = make_minimal_parquet(vec![("source", "rezolus"), ("node", "web01")]);
        set_node_metadata(tmp.path(), "web02").unwrap();

        let kv = read_kv(tmp.path());
        let nodes: Vec<&str> = kv
            .iter()
            .filter(|(k, _)| k == KEY_NODE)
            .map(|(_, v)| v.as_str())
            .collect();
        assert_eq!(nodes, vec!["web02"]); // replaced, not duplicated
    }

    // ── set_source_metadata tests ──

    #[test]
    fn set_source_adds_when_missing() {
        let tmp = make_minimal_parquet(vec![]);
        set_source_metadata(tmp.path(), "vllm", false).unwrap();

        let kv = read_kv(tmp.path());
        assert!(kv.iter().any(|(k, v)| k == KEY_SOURCE && v == "vllm"));
    }

    #[test]
    fn set_source_idempotent_when_value_matches() {
        let tmp = make_minimal_parquet(vec![("source", "vllm")]);
        // Same value, no --overwrite needed: no error, no duplicate.
        set_source_metadata(tmp.path(), "vllm", false).unwrap();

        let kv = read_kv(tmp.path());
        let sources: Vec<&str> = kv
            .iter()
            .filter(|(k, _)| k == KEY_SOURCE)
            .map(|(_, v)| v.as_str())
            .collect();
        assert_eq!(sources, vec!["vllm"]);
    }

    #[test]
    fn set_source_errors_when_replacing_without_overwrite() {
        let tmp = make_minimal_parquet(vec![("source", "vllm")]);
        let err = set_source_metadata(tmp.path(), "sglang", false)
            .expect_err("should refuse to overwrite without flag");
        let msg = err.to_string();
        assert!(
            msg.contains("source") && msg.contains("--overwrite"),
            "got: {msg}"
        );

        // Original value intact
        let kv = read_kv(tmp.path());
        assert!(kv.iter().any(|(k, v)| k == KEY_SOURCE && v == "vllm"));
    }

    #[test]
    fn set_source_replaces_with_overwrite() {
        let tmp = make_minimal_parquet(vec![("source", "vllm")]);
        set_source_metadata(tmp.path(), "sglang", true).unwrap();

        let kv = read_kv(tmp.path());
        let sources: Vec<&str> = kv
            .iter()
            .filter(|(k, _)| k == KEY_SOURCE)
            .map(|(_, v)| v.as_str())
            .collect();
        assert_eq!(sources, vec!["sglang"]);
    }

    #[test]
    fn set_source_preserves_other_metadata() {
        let tmp = make_minimal_parquet(vec![("source", "vllm"), ("node", "gpu01")]);
        set_source_metadata(tmp.path(), "sglang", true).unwrap();

        let kv = read_kv(tmp.path());
        assert!(kv.iter().any(|(k, v)| k == "node" && v == "gpu01"));
    }

    #[test]
    fn set_node_preserves_data_rows() {
        let tmp = make_minimal_parquet(vec![("source", "rezolus")]);
        set_node_metadata(tmp.path(), "web01").unwrap();

        let builder =
            ParquetRecordBatchReaderBuilder::try_new(std::fs::File::open(tmp.path()).unwrap())
                .unwrap();
        let mut reader = builder.build().unwrap();
        let batch = reader.next().unwrap().unwrap();
        assert_eq!(batch.num_rows(), 3);
    }
}
