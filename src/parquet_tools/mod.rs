use clap::{value_parser, ArgMatches, Command};
use std::collections::BTreeSet;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::viewer::promql::QueryEngine;
use crate::viewer::tsdb::Tsdb;
use crate::viewer::ServiceExtension;

static TEMPLATES: &[(&str, &str)] = &[("llm-perf", include_str!("templates/llm_perf.json"))];

fn lookup_template(source: &str) -> Option<&'static str> {
    TEMPLATES
        .iter()
        .find(|(name, _)| *name == source)
        .map(|(_, json)| *json)
}

pub fn command() -> Command {
    Command::new("parquet")
        .about("Parquet file operations")
        .subcommand_required(true)
        .subcommand(
            Command::new("annotate")
                .about("Add service extension metadata to a parquet file")
                .arg(
                    clap::Arg::new("FILE")
                        .help("Parquet file to annotate")
                        .value_parser(value_parser!(PathBuf))
                        .required(true)
                        .index(1),
                )
                .arg(
                    clap::Arg::new("service-extension")
                        .long("file")
                        .value_name("PATH")
                        .help("Custom service extension JSON file (overrides built-in template)")
                        .value_parser(value_parser!(PathBuf))
                        .action(clap::ArgAction::Set),
                ),
        )
}

pub fn run(args: ArgMatches) {
    match args.subcommand() {
        Some(("annotate", sub_args)) => run_annotate(sub_args),
        _ => unreachable!(),
    }
}

fn run_annotate(args: &ArgMatches) {
    let path = args.get_one::<PathBuf>("FILE").unwrap();
    let custom_file = args.get_one::<PathBuf>("service-extension");

    let json = if let Some(custom_path) = custom_file {
        let content =
            std::fs::read_to_string(custom_path).expect("failed to read service extension file");
        // Validate the JSON parses correctly
        let _: ServiceExtension =
            serde_json::from_str(&content).expect("invalid service extension JSON");
        content
    } else {
        // Look up built-in template from parquet source metadata
        let source = read_source_metadata(path);
        let name = source.as_deref().unwrap_or_else(|| {
            eprintln!(
                "error: parquet file has no 'source' metadata. Use --file to provide a template."
            );
            std::process::exit(1);
        });
        let template = lookup_template(name).unwrap_or_else(|| {
            eprintln!(
                "error: no built-in template for source {:?}. Use --file to provide one.",
                name
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
    annotate_parquet(path, &annotated_json).expect("failed to annotate parquet file");

    println!(
        "Annotated {:?} with {:?} service extension ({} KPIs)",
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
            format!(
                "histogram_percentiles([0.5, 0.9, 0.99, 0.999, 0.9999], {})",
                kpi.query
            )
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

/// Extract metric selectors (name + labels) from a PromQL query string.
///
/// Returns selectors like `tokens{direction="output"}` or `requests_inflight`,
/// excluding known PromQL function names.
fn extract_metric_selectors(query: &str) -> BTreeSet<String> {
    let bytes = query.as_bytes();
    let len = bytes.len();
    let mut selectors = BTreeSet::new();
    let mut i = 0;

    while i < len {
        if bytes[i].is_ascii_alphabetic() || bytes[i] == b'_' {
            let start = i;
            while i < len && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let ident = &query[start..i];

            // Skip whitespace, then check what follows
            let rest = query[i..].trim_start();
            let next = rest.as_bytes().first().copied();

            // function call — skip
            if next == Some(b'(') && is_promql_function(ident) {
                i = query.len() - rest.len();
                continue;
            }

            // metric{labels} — include the braces
            if next == Some(b'{') {
                if let Some(close) = rest.find('}') {
                    let sel_end = query.len() - rest.len() + close + 1;
                    selectors.insert(query[start..sel_end].to_string());
                    i = sel_end;
                    continue;
                }
            }

            selectors.insert(ident.to_string());
        } else {
            i += 1;
        }
    }

    selectors
}

fn is_promql_function(name: &str) -> bool {
    matches!(
        name,
        "sum"
            | "avg"
            | "min"
            | "max"
            | "count"
            | "rate"
            | "irate"
            | "increase"
            | "delta"
            | "idelta"
            | "abs"
            | "ceil"
            | "floor"
            | "round"
            | "clamp"
            | "clamp_min"
            | "clamp_max"
            | "ln"
            | "log2"
            | "log10"
            | "exp"
            | "sqrt"
            | "sgn"
            | "sort"
            | "sort_desc"
            | "label_replace"
            | "label_join"
            | "histogram_quantile"
            | "histogram_percentiles"
            | "histogram_heatmap"
    )
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
        .find(|kv| kv.key == "source")
        .and_then(|kv| kv.value.clone())
}

fn annotate_parquet(
    path: &Path,
    service_extension_json: &str,
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

    // Remove any existing service extension keys
    kv_meta.retain(|kv| kv.key != "metric_type" && kv.key != "service_extension");

    // Add new keys
    kv_meta.push(KeyValue {
        key: "metric_type".to_string(),
        value: Some("rezolus:service-extension".to_string()),
    });
    kv_meta.push(KeyValue {
        key: "service_extension".to_string(),
        value: Some(service_extension_json.to_string()),
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
