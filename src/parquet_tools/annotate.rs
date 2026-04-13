use clap::ArgMatches;
use std::collections::BTreeSet;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::parquet_metadata::{KEY_PER_SOURCE_METADATA, KEY_SOURCE, NESTED_SERVICE_QUERIES};
use crate::viewer::promql::QueryEngine;
use crate::viewer::tsdb::Tsdb;
use crate::viewer::ServiceExtension;

static TEMPLATES: &[(&str, &str)] = &[
    ("llm-perf", include_str!("templates/llm_perf.json")),
    ("cachecannon", include_str!("templates/cachecannon.json")),
];

/// Source name aliases for renamed projects (old name → canonical name).
static SOURCE_ALIASES: &[(&str, &str)] = &[("llm-bench", "llm-perf")];

fn lookup_template(source: &str) -> Option<&'static str> {
    let canonical = SOURCE_ALIASES
        .iter()
        .find(|(alias, _)| *alias == source)
        .map(|(_, canon)| *canon)
        .unwrap_or(source);
    TEMPLATES
        .iter()
        .find(|(name, _)| *name == canonical)
        .map(|(_, json)| *json)
}

pub(super) fn run(args: &ArgMatches) {
    let path = args.get_one::<PathBuf>("FILE").unwrap();
    let custom_file = args.get_one::<PathBuf>("service-extension");

    let source = read_source_metadata(path).unwrap_or_else(|| {
        eprintln!(
            "error: parquet file has no 'source' metadata. Use --file to provide a template."
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
        let template = lookup_template(&source).unwrap_or_else(|| {
            eprintln!(
                "error: no built-in template for source {:?}. Use --file to provide one.",
                source
            );
            std::process::exit(1);
        });
        template.to_string()
    };

    let mut ext: ServiceExtension = serde_json::from_str(&json).unwrap();

    // Validate KPI queries against the parquet data and set available flags
    validate_kpis(path, &mut ext);

    let annotated_json =
        serde_json::to_string(&ext).expect("failed to serialize service extension");
    annotate_parquet(path, &source, &annotated_json).expect("failed to annotate parquet file");

    println!(
        "Annotated {:?} with {:?} service queries ({} KPIs)",
        path,
        ext.service_name,
        ext.kpis.len()
    );
}

/// Build the effective PromQL query for a KPI, accounting for histogram wrapping.
fn effective_query(kpi: &crate::viewer::Kpi) -> String {
    if kpi.metric_type == "histogram" {
        let subtype = kpi.subtype.as_deref().unwrap_or("percentiles");
        if subtype == "buckets" {
            format!("histogram_heatmap({})", kpi.query)
        } else {
            let quantiles = match &kpi.percentiles {
                Some(p) => format!(
                    "[{}]",
                    p.iter()
                        .map(|v| v.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                None => format!(
                    "[{}]",
                    crate::common::DEFAULT_PERCENTILES
                        .iter()
                        .map(|v| v.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            };
            format!("histogram_percentiles({}, {})", quantiles, kpi.query)
        }
    } else {
        kpi.query.clone()
    }
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
        let query = effective_query(kpi);
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
fn extract_metric_selectors(query: &str) -> BTreeSet<String> {
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

fn read_source_metadata(path: &Path) -> Option<String> {
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
    source: &str,
    service_queries_json: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    use parquet::arrow::ArrowWriter;
    use parquet::file::properties::WriterProperties;
    use parquet::file::reader::FileReader;
    use parquet::file::serialized_reader::SerializedFileReader;
    use parquet::format::KeyValue;

    // Read existing metadata
    let meta_reader = SerializedFileReader::new(std::fs::File::open(path)?)?;
    let mut kv_meta: Vec<KeyValue> = meta_reader
        .metadata()
        .file_metadata()
        .key_value_metadata()
        .cloned()
        .unwrap_or_default();

    // Build or update the nested metadata map
    let mut metadata_map: serde_json::Map<String, serde_json::Value> = kv_meta
        .iter()
        .find(|kv| kv.key == KEY_PER_SOURCE_METADATA)
        .and_then(|kv| kv.value.as_deref())
        .and_then(|v| serde_json::from_str(v).ok())
        .unwrap_or_default();

    // Nest service_queries under this source
    let service_queries: serde_json::Value = serde_json::from_str(service_queries_json)?;
    let source_entry = metadata_map
        .entry(source.to_string())
        .or_insert_with(|| serde_json::json!({}));
    if let serde_json::Value::Object(map) = source_entry {
        map.insert(NESTED_SERVICE_QUERIES.to_string(), service_queries);
    }

    kv_meta.retain(|kv| kv.key != KEY_PER_SOURCE_METADATA);
    kv_meta.push(KeyValue {
        key: KEY_PER_SOURCE_METADATA.to_string(),
        value: Some(serde_json::to_string(&metadata_map)?),
    });

    let props = WriterProperties::builder()
        .set_key_value_metadata(Some(kv_meta))
        .build();

    // Read all record batches
    let builder = ParquetRecordBatchReaderBuilder::try_new(std::fs::File::open(path)?)?;
    let schema = builder.schema().clone();
    let reader = builder.build()?;

    // Write to memory buffer with updated metadata
    let mut output = Vec::new();
    {
        let mut writer = ArrowWriter::try_new(Cursor::new(&mut output), schema, Some(props))?;
        for batch in reader {
            writer.write(&batch?)?;
        }
        writer.close()?;
    }

    // Write back to the same file (in-place)
    std::fs::write(path, &output)?;

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
}
