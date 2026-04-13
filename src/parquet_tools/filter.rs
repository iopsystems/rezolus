use clap::ArgMatches;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::parquet_metadata::{
    KEY_DESCRIPTIONS, KEY_PER_SOURCE_METADATA, KEY_SERVICE_QUERIES, KEY_SOURCE,
    NESTED_SERVICE_QUERIES,
};
use crate::viewer::{ServiceExtension, TemplateRegistry};

use super::annotate::extract_metric_selectors;

pub(super) fn run(args: &ArgMatches, registry: &TemplateRegistry) {
    let path = args.get_one::<PathBuf>("FILE").unwrap();
    let custom_file = args.get_one::<PathBuf>("queries");
    let output = args.get_one::<PathBuf>("output");

    let ext = resolve_service_extension(path, custom_file.map(|p| p.as_path()), registry)
        .unwrap_or_else(|e| {
            eprintln!("error: {e}");
            std::process::exit(1);
        });

    if let Err(e) = filter_parquet_file(path, &ext, output.map(|p| p.as_path())) {
        eprintln!("error: failed to filter parquet file: {e}");
        std::process::exit(1);
    }
}

/// Filter a parquet file to retain only columns needed by the service extension
/// KPI queries, plus `timestamp` and `duration`.
///
/// If `output` is `None`, the file is overwritten in-place.
pub(super) fn filter_parquet_file(
    path: &Path,
    ext: &ServiceExtension,
    output: Option<&Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

    let mut keep = extract_column_names(ext);
    keep.insert("timestamp".to_string());
    keep.insert("duration".to_string());

    let mut kv_meta = super::read_file_metadata(path)?;

    // Read schema to compute column indices
    let builder = ParquetRecordBatchReaderBuilder::try_new(std::fs::File::open(path)?)?;
    let schema = builder.schema().clone();
    let total_columns = schema.fields().len();

    let indices: Vec<usize> = schema
        .fields()
        .iter()
        .enumerate()
        .filter(|(_, f)| {
            let name = f.name();
            // Exact match, or match the base name before ':' (e.g. "response_latency:buckets")
            keep.contains(name.as_str())
                || name
                    .split_once(':')
                    .is_some_and(|(base, _)| keep.contains(base))
        })
        .map(|(i, _)| i)
        .collect();

    let kept_names: BTreeSet<&str> = indices
        .iter()
        .map(|&i| schema.field(i).name().as_str())
        .collect();

    filter_descriptions_metadata(&mut kv_meta, &kept_names);

    let buf = super::rewrite_parquet(path, kv_meta, Some(&indices))?;
    let dest = output.unwrap_or(path);
    std::fs::write(dest, &buf)?;

    println!(
        "Filtered {:?}: kept {} of {} columns",
        dest,
        indices.len(),
        total_columns,
    );

    Ok(())
}

/// Extract base metric column names from all KPI queries in a service extension.
fn extract_column_names(ext: &ServiceExtension) -> BTreeSet<String> {
    ext.kpis
        .iter()
        .flat_map(|kpi| extract_metric_selectors(&kpi.query))
        .map(|selector| {
            // Strip label selectors: "tokens{direction=\"output\"}" -> "tokens"
            selector.split('{').next().unwrap_or(&selector).to_string()
        })
        .collect()
}

/// Filter the `descriptions` metadata key to only include entries for retained columns.
fn filter_descriptions_metadata(kv_meta: &mut [parquet::format::KeyValue], kept: &BTreeSet<&str>) {
    if let Some(entry) = kv_meta.iter_mut().find(|kv| kv.key == KEY_DESCRIPTIONS) {
        if let Some(value) = &entry.value {
            if let Ok(mut map) =
                serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(value)
            {
                map.retain(|k, _| kept.contains(k.as_str()));
                if let Ok(filtered) = serde_json::to_string(&map) {
                    entry.value = Some(filtered);
                }
            }
        }
    }
}

/// Resolve a ServiceExtension from the available sources.
///
/// Resolution order:
/// 1. Custom file (if provided via `--file`)
/// 2. Top-level `service_queries` key in parquet metadata
/// 3. `per_source_metadata.<source>.service_queries` (combined files)
/// 4. Built-in template looked up by source name
fn resolve_service_extension(
    path: &Path,
    custom_file: Option<&Path>,
    registry: &TemplateRegistry,
) -> Result<ServiceExtension, Box<dyn std::error::Error>> {
    use parquet::file::reader::FileReader;
    use parquet::file::serialized_reader::SerializedFileReader;

    // 1. Custom file
    if let Some(custom_path) = custom_file {
        let content = std::fs::read_to_string(custom_path)?;
        let ext: ServiceExtension = serde_json::from_str(&content)?;
        return Ok(ext);
    }

    // Read parquet metadata
    let file = std::fs::File::open(path)?;
    let reader = SerializedFileReader::new(file)?;
    let kv = reader
        .metadata()
        .file_metadata()
        .key_value_metadata()
        .cloned()
        .unwrap_or_default();

    // 2. Top-level service_queries
    if let Some(sq) = kv
        .iter()
        .find(|kv| kv.key == KEY_SERVICE_QUERIES)
        .and_then(|kv| kv.value.as_deref())
    {
        if let Ok(ext) = serde_json::from_str::<ServiceExtension>(sq) {
            return Ok(ext);
        }
    }

    // For combined files we must collect KPIs from ALL sources so the filter
    // retains columns needed by every service, not just the first one found.
    let mut all_kpis = Vec::new();
    let mut first_ext: Option<ServiceExtension> = None;

    // 3. per_source_metadata.<source>.service_queries — collect from all sources
    let source = kv
        .iter()
        .find(|kv| kv.key == KEY_SOURCE)
        .and_then(|kv| kv.value.as_deref());

    if let Some(psm_str) = kv
        .iter()
        .find(|kv| kv.key == KEY_PER_SOURCE_METADATA)
        .and_then(|kv| kv.value.as_deref())
    {
        if let Ok(psm) = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(psm_str)
        {
            for (_source_name, source_meta) in &psm {
                if let Some(sq) = source_meta.get(NESTED_SERVICE_QUERIES) {
                    if let Ok(ext) = serde_json::from_value::<ServiceExtension>(sq.clone()) {
                        all_kpis.extend(ext.kpis.clone());
                        if first_ext.is_none() {
                            first_ext = Some(ext);
                        }
                    }
                }
            }
        }
    }

    // 4. Template registry by source name — collect from all sources
    if let Some(source_str) = source {
        let sources: Vec<String> = serde_json::from_str::<Vec<String>>(source_str)
            .unwrap_or_else(|_| vec![source_str.trim_matches('"').to_string()]);
        for s in &sources {
            if let Some(template) = registry.get(s) {
                all_kpis.extend(template.kpis.clone());
                if first_ext.is_none() {
                    first_ext = Some(template.clone());
                }
            }
        }
    }

    if let Some(mut ext) = first_ext {
        ext.kpis = all_kpis;
        return Ok(ext);
    }

    Err("no service extension found: use --queries to provide one".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_ext(queries: &[&str]) -> ServiceExtension {
        ServiceExtension {
            service_name: "test".to_string(),
            aliases: Vec::new(),
            service_metadata: Default::default(),
            slo: None,
            kpis: queries
                .iter()
                .map(|q| crate::viewer::Kpi {
                    role: "test".to_string(),
                    title: "test".to_string(),
                    description: None,
                    query: q.to_string(),
                    metric_type: "gauge".to_string(),
                    subtype: None,
                    unit_system: None,
                    percentiles: None,
                    available: false,
                    denominator: false,
                })
                .collect(),
        }
    }

    #[test]
    fn extract_column_names_basic() {
        let ext = make_test_ext(&["requests_inflight", "ttft"]);
        let names = extract_column_names(&ext);
        assert!(names.contains("requests_inflight"));
        assert!(names.contains("ttft"));
        assert_eq!(names.len(), 2);
    }

    #[test]
    fn extract_column_names_strips_labels() {
        let ext = make_test_ext(&[r#"sum(irate(tokens{direction="output"}[5s]))"#]);
        let names = extract_column_names(&ext);
        assert!(names.contains("tokens"));
        assert!(!names.iter().any(|n| n.contains('{')));
    }

    #[test]
    fn extract_column_names_deduplicates() {
        let ext = make_test_ext(&[
            r#"sum(irate(requests{status="error"}[5s])) / sum(irate(requests{status="sent"}[5s]))"#,
        ]);
        let names = extract_column_names(&ext);
        // "requests" appears twice in query but should be deduplicated
        assert!(names.contains("requests"));
        assert_eq!(names.len(), 1);
    }
}
