use clap::ArgMatches;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use ::dashboard::keys::{
    KEY_PER_SOURCE_METADATA, KEY_SERVICE_QUERIES, KEY_SOURCE, NESTED_SERVICE_QUERIES,
};
use ::dashboard::{ServiceExtension, TemplateRegistry};

use super::legacy::{self, Container};

pub(super) fn run(args: &ArgMatches, registry: &TemplateRegistry) {
    let path = args.get_one::<PathBuf>("FILE").unwrap();
    let output = args.get_one::<PathBuf>("output").map(PathBuf::as_path);
    let csv = |name: &str| -> Option<BTreeSet<String>> {
        args.get_one::<String>(name).map(|list| {
            list.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
    };
    let samplers = csv("samplers");
    let mut metrics = csv("metrics");
    let container = Container::of(path);

    // A parquet file with neither flag keeps what its KPIs need, as the
    // parquet filter did: the metrics its KPI queries name, and every metric
    // of the agent's columns.
    if container == Container::Parquet && samplers.is_none() && metrics.is_none() {
        let custom = args.get_one::<PathBuf>("queries").map(PathBuf::as_path);
        metrics = Some(
            resolve_service_extension(path, custom, registry)
                .and_then(|ext| kpi_metrics(path, &ext))
                .unwrap_or_else(|e| {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }),
        );
    } else if args.get_one::<PathBuf>("queries").is_some() {
        eprintln!(
            "warning: --queries picks a parquet file's KPI metrics when neither --samplers \
             nor --metrics is given; it is ignored here"
        );
    }

    if samplers.is_none() && metrics.is_none() {
        eprintln!(
            "error: filtering an archive requires --samplers <a,b,...> (samplers to keep) \
             and/or --metrics <a,b,...> (metric columns to keep)"
        );
        std::process::exit(1);
    }

    if let Err(e) = filter(path, samplers.as_ref(), metrics.as_ref(), output) {
        eprintln!("error: failed to filter {}: {e}", path.display());
        std::process::exit(1);
    }
}

/// Filter the recording at `path` to `keep_samplers` and/or `keep_metrics`:
/// a dendro archive in place or into `output` (a `.dendro`), a legacy one
/// into `output`, which it needs.
fn filter(
    path: &Path,
    keep_samplers: Option<&BTreeSet<String>>,
    keep_metrics: Option<&BTreeSet<String>>,
    output: Option<&Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    match Container::of(path) {
        Container::Dendro => {
            if let Some(output) = output.filter(|o| o.extension().is_none_or(|e| e != "dendro")) {
                return Err(format!(
                    "{}: 6.x writes .dendro archives only; give an output ending in .dendro",
                    output.display()
                )
                .into());
            }
            filter_dendro(path, keep_samplers, keep_metrics, output)
        }
        c if c.is_legacy() => filter_legacy(path, c, keep_samplers, keep_metrics, output),
        _ => Err(format!(
            "{} is not a recording (.dendro, .rez or .parquet)",
            path.display()
        )
        .into()),
    }
}

/// Filter a legacy recording into a new `.dendro` at `output`: converted
/// beside it first, then filtered as a dendro archive. The input is not
/// changed; without `output` the filter is refused, naming the command.
fn filter_legacy(
    path: &Path,
    container: Container,
    keep_samplers: Option<&BTreeSet<String>>,
    keep_metrics: Option<&BTreeSet<String>>,
    output: Option<&Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(output) = output else {
        return Err(format!(
            "{} is a {}; 6.x writes .dendro only and does not change it. Write the \
             filtered recording to a new archive instead:\n  {}",
            path.display(),
            container.name(),
            legacy::command_with_output(&legacy::dendro_beside(path))
        )
        .into());
    };
    legacy::check_new_dendro(output)?;
    let staging = super::legacy::staging_dir(output)?;
    let converted = staging.path().join("converted.dendro");
    println!("{}", legacy::convert_for(path, &converted, output)?);
    filter_dendro(&converted, keep_samplers, keep_metrics, Some(output))
}

/// The metrics a parquet file's KPI filter keeps: the base names of the
/// metrics `ext`'s queries select, and the metric of every agent column (a
/// column whose own `source`, else the file's, is `rezolus`). A column's
/// metric is its `metric` metadata, else its name before `:`.
fn kpi_metrics(
    path: &Path,
    ext: &ServiceExtension,
) -> Result<BTreeSet<String>, Box<dyn std::error::Error>> {
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    let builder = ParquetRecordBatchReaderBuilder::try_new(std::fs::File::open(path)?)?;
    let file_source: Option<String> = builder
        .metadata()
        .file_metadata()
        .key_value_metadata()
        .and_then(|kv| kv.iter().find(|kv| kv.key == KEY_SOURCE))
        .and_then(|kv| kv.value.clone());
    let mut keep = extract_column_names(ext);
    for f in builder.schema().fields() {
        let source = f
            .metadata()
            .get(KEY_SOURCE)
            .map(String::as_str)
            .or(file_source.as_deref());
        if source == Some("rezolus") {
            let metric = f
                .metadata()
                .get("metric")
                .cloned()
                .unwrap_or_else(|| f.name().split(':').next().unwrap_or(f.name()).to_string());
            keep.insert(metric);
        }
    }
    Ok(keep)
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

/// Filter a dendro archive by sampler and/or by metric column, through
/// dendro's `copy_sources_into` (`crate::dendro_copy`).
///
/// - `keep_samplers` keeps a stream when its sampler is named; an occupant
///   stream (`<table>/occupants`) counts as its table's, so it goes with it.
/// - `keep_metrics` trims columns with metriken-storage's `KeepMetrics`,
///   which keeps a long table long (its `occupant` column and layout
///   markers) and copies occupant streams whole, re-encoding with the
///   writer's properties. A table left with no kept metric is dropped by the
///   copy, and its occupant stream is then removed here, since dendro only
///   sees the stream's own columns.
///
/// The live tail of each kept stream is sealed into the copy, as a snapshot
/// seals it.
fn filter_dendro(
    path: &Path,
    keep_samplers: Option<&std::collections::BTreeSet<String>>,
    keep_metrics: Option<&std::collections::BTreeSet<String>>,
    output: Option<&Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    use ::rez::occupants::table_of;
    use dendro::archive::{Archive, ArchiveMut};
    use dendro::rewrite::CopySpec;
    use metriken_storage::table_sampler;

    // An `-o` naming another file must be a new `.dendro`; one naming the
    // input itself (`./live.dendro` for `live.dendro`) filters in place.
    let in_place = output.is_none_or(|o| super::legacy::same_path(o, path));
    if let Some(output) = output.filter(|_| !in_place) {
        super::legacy::check_new_dendro(output)?;
    }

    let src = Archive::open(path)?;
    let tables: Vec<String> = crate::dendro_copy::streams(&src)?
        .into_iter()
        .filter(|s| table_of(s).is_none())
        .collect();
    let present: BTreeSet<String> = tables
        .iter()
        .map(|t| table_sampler(t).to_string())
        .collect();
    if let Some(keep) = keep_samplers {
        let unmatched: Vec<&str> = keep
            .iter()
            .map(String::as_str)
            .filter(|s| !present.contains(*s))
            .collect();
        if !unmatched.is_empty() {
            return Err(format!(
                "no sampler named {} in {}; it holds: {}",
                unmatched.join(", "),
                path.display(),
                present
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .join(", "),
            )
            .into());
        }
    }

    let dest = output.unwrap_or(path);
    let staging = super::legacy::staging_dir(dest)?;
    let staged = staging.path().join("filtered.dendro");

    let keep_stream = |stream: &str| -> bool {
        keep_samplers
            .is_none_or(|keep| keep.contains(table_sampler(table_of(stream).unwrap_or(stream))))
    };
    let keep_columns = keep_metrics.map(metriken_storage::KeepMetrics::new);
    let spec = CopySpec {
        keep_streams: Some(&keep_stream),
        keep_columns: keep_columns
            .as_ref()
            .map(|k| k as &dyn dendro::rewrite::ColumnFilter),
        writer_props: Some(metriken_storage::segment_props(
            metriken_storage::default_compression(),
        )),
        ..CopySpec::everything()
    };
    crate::dendro_copy::copy(&src, &staged, &spec)?;
    drop(src);

    // An occupant stream whose table did not survive the projection names
    // occupants of nothing.
    let mut dst = ArchiveMut::open(&staged)?;
    let mut kept = 0usize;
    for source in dst.read_sources()? {
        let streams = dst.all_streams(source.id)?;
        let tables: BTreeSet<&str> = streams
            .iter()
            .map(String::as_str)
            .filter(|s| table_of(s).is_none())
            .collect();
        kept += tables.len();
        let orphan = |stream: &str| table_of(stream).is_some_and(|t| !tables.contains(t));
        if streams.iter().any(|s| orphan(s)) {
            dst.evict_streams_before(source.id, i64::MAX, &orphan)?;
        }
    }
    drop(dst);

    if kept == 0 {
        return Err(format!(
            "filtering {} kept no tables — refusing to overwrite it with an empty archive; \
             check the --metrics / --samplers names",
            path.display(),
        )
        .into());
    }
    // In place, the filtered copy replaces the input, as asked; to a new
    // output, nothing that appeared there meanwhile is replaced.
    if in_place {
        std::fs::rename(&staged, path)?;
    } else {
        super::legacy::publish(&staged, dest)?;
    }
    println!(
        "Filtered {:?}: kept {} of {} tables",
        dest,
        kept,
        tables.len()
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

/// Resolve a ServiceExtension from the available sources.
///
/// Resolution order:
/// 1. Custom file (if provided via `--file`)
/// 2. Top-level `service_queries` key in parquet metadata
/// 3. `per_source_metadata.<source>.<entry>.service_queries` (combined
///    files; one template per source, from its first entry that has one, as
///    the viewers read it in `dashboard::source_templates`)
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
            for source_group in psm.values() {
                let Some(entries) = source_group.as_object() else {
                    continue;
                };
                let ext = entries.values().find_map(|entry| {
                    entry
                        .get(NESTED_SERVICE_QUERIES)
                        .and_then(|sq| serde_json::from_value::<ServiceExtension>(sq.clone()).ok())
                });
                if let Some(ext) = ext {
                    all_kpis.extend(ext.kpis.clone());
                    if first_ext.is_none() {
                        first_ext = Some(ext);
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
    use std::collections::HashMap;

    /// A one-row parquet with the given columns, each carrying the metadata
    /// given, plus whatever file-level keys are asked for.
    fn write_parquet(path: &Path, columns: &[(&str, &[(&str, &str)])], file_meta: &[(&str, &str)]) {
        use arrow::array::{ArrayRef, UInt64Array};
        use arrow::datatypes::{DataType, Field, Schema};
        use arrow::record_batch::RecordBatch;
        use parquet::arrow::ArrowWriter;
        use parquet::file::properties::WriterProperties;
        use std::sync::Arc;

        let fields: Vec<Field> = columns
            .iter()
            .map(|(name, meta)| {
                Field::new(*name, DataType::UInt64, false).with_metadata(
                    meta.iter()
                        .map(|(k, v)| (k.to_string(), v.to_string()))
                        .collect::<HashMap<_, _>>(),
                )
            })
            .collect();
        let schema = Arc::new(Schema::new(fields));
        let arrays: Vec<ArrayRef> = columns
            .iter()
            .map(|_| Arc::new(UInt64Array::from(vec![1u64])) as ArrayRef)
            .collect();
        let batch = RecordBatch::try_new(schema.clone(), arrays).unwrap();

        let kv: Vec<parquet::file::metadata::KeyValue> = file_meta
            .iter()
            .map(|(k, v)| {
                parquet::file::metadata::KeyValue::new(k.to_string(), Some(v.to_string()))
            })
            .collect();
        let props = WriterProperties::builder()
            .set_key_value_metadata(if kv.is_empty() { None } else { Some(kv) })
            .build();
        let file = std::fs::File::create(path).unwrap();
        let mut writer = ArrowWriter::try_new(file, schema, Some(props)).unwrap();
        writer.write(&batch).unwrap();
        writer.close().unwrap();
    }

    /// A combined parquet's templates are read from each source's entries,
    /// `per_source_metadata.<source>.<entry>.service_queries`, where
    /// `recording combine` writes them, and every source's KPIs are kept.
    #[test]
    fn combined_templates_are_read_from_each_sources_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("combined.parquet");
        let kpi =
            |q: &str| serde_json::json!({"role": "r", "title": q, "query": q, "type": "gauge"});
        let psm = serde_json::json!({
            "widgetd": {"0": {"service_queries": {"service_name": "widgetd", "kpis": [kpi("widgets")]}}},
            "gadgetd": {"0": {"version": "1"}, "1": {"service_queries": {"service_name": "gadgetd", "kpis": [kpi("gadgets")]}}},
        })
        .to_string();
        write_parquet(
            &path,
            &[("timestamp", &[])],
            &[
                ("source", r#"["gadgetd","widgetd"]"#),
                ("per_source_metadata", psm.as_str()),
            ],
        );
        let registry = TemplateRegistry::embedded().unwrap();
        let ext = resolve_service_extension(&path, None, &registry).unwrap();
        let mut queries: Vec<&str> = ext.kpis.iter().map(|k| k.query.as_str()).collect();
        queries.sort();
        assert_eq!(queries, ["gadgets", "widgets"]);
    }

    /// The recorder no longer writes `source` into every column, so the rule
    /// that keeps agent columns has to read the file's own `source` key.
    ///
    /// Without the fallback this drops every rezolus column from a
    /// single-source recording — the columns an operator keeps a recording for.
    #[test]
    fn agent_columns_are_kept_from_the_files_source_when_a_column_has_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.parquet");
        write_parquet(
            &path,
            &[
                ("timestamp", &[]),
                ("cpu_usage", &[("metric", "cpu_usage")]),
            ],
            &[("source", "rezolus")],
        );
        // A KPI set that names neither column: only the rezolus rule can keep
        // them.
        let kept = kpi_metrics(&path, &make_test_ext(&["something_else"])).unwrap();
        assert!(
            kept.contains("cpu_usage"),
            "an agent column must survive on the file's source alone, got {kept:?}"
        );
    }

    /// And the rule still discriminates: a file from something else keeps only
    /// what its KPIs name, end to end into the `.dendro` it is filtered to. A
    /// fallback that kept everything would make the filter a no-op for service
    /// recordings.
    #[test]
    fn a_non_agent_file_keeps_only_what_its_queries_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("svc.parquet");
        let out = dir.path().join("slim.dendro");
        write_parquet(
            &path,
            &[
                ("timestamp", &[("metric_type", "timestamp")]),
                (
                    "wanted",
                    &[("metric", "wanted"), ("metric_type", "counter")],
                ),
                (
                    "unwanted",
                    &[("metric", "unwanted"), ("metric_type", "counter")],
                ),
            ],
            &[("source", "llm-perf")],
        );
        let kept = kpi_metrics(&path, &make_test_ext(&["wanted"])).unwrap();
        assert_eq!(kept, set(&["wanted"]));
        filter(&path, None, Some(&kept), Some(&out)).unwrap();
        let readers = ::rez::reader::RezReader::open_recordings(
            &out,
            metriken_storage::BufferPool::new(8 * 1024 * 1024),
        )
        .unwrap();
        let names = readers[0].1.counter_names();
        assert_eq!(names, ["wanted"], "{names:?}");
    }

    fn make_test_ext(queries: &[&str]) -> ServiceExtension {
        ServiceExtension {
            service_name: "test".to_string(),
            aliases: Vec::new(),
            service_metadata: Default::default(),
            kpis: queries
                .iter()
                .map(|q| ::dashboard::Kpi {
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
                    subgroup: None,
                    subgroup_description: None,
                    full_width: false,
                    check: None,
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

    // ── legacy inputs ──────────────────────────────────────────────────

    /// A `.rez` of either container is filtered into a new `.dendro` holding
    /// the kept samplers with their rows, and is not changed itself.
    #[test]
    fn a_rez_is_filtered_into_a_new_dendro() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["v3.rez", "v2.rez"] {
            let path = ::rez::fixtures::committed_rez(name, dir.path());
            let before = std::fs::read(&path).unwrap();
            let out = dir.path().join(format!("{name}.dendro"));
            filter(&path, Some(&set(&["a"])), None, Some(&out)).unwrap();
            assert_eq!(dendro_streams(&out), ["a"], "{name}");
            let db = dendro::archive::Archive::open(&out).unwrap();
            let id = db.read_sources().unwrap()[0].id;
            assert!(
                db.total_rows(id, "a").unwrap() > 0,
                "{name}: the rows came across"
            );
            assert_eq!(std::fs::read(&path).unwrap(), before, "{name} unchanged");
        }
    }

    /// `--metrics` on a `.rez` keeps the table holding the kept metric.
    #[test]
    fn a_rez_is_filtered_by_metric() {
        let dir = tempfile::tempdir().unwrap();
        let path = ::rez::fixtures::committed_rez("v3.rez", dir.path());
        let out = dir.path().join("slim.dendro");
        filter(&path, None, Some(&set(&["mem_free"])), Some(&out)).unwrap();
        assert_eq!(dendro_streams(&out), ["b"]);
    }

    /// Without `-o` a legacy recording is refused, naming `-o` and the
    /// `.dendro` to write; an output that is not a new `.dendro` is refused;
    /// an unmatched sampler names the typo and writes nothing.
    #[test]
    fn a_legacy_filter_needs_a_new_dendro() {
        let dir = tempfile::tempdir().unwrap();
        let path = ::rez::fixtures::committed_rez("v3.rez", dir.path());
        let err = filter(&path, Some(&set(&["a"])), None, None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("-o") && err.contains("v3.dendro"), "{err}");
        let err = filter(
            &path,
            Some(&set(&["a"])),
            None,
            Some(&dir.path().join("x.rez")),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains(".dendro archives only"), "{err}");
        let out = dir.path().join("typo.dendro");
        let err = filter(&path, Some(&set(&["cpu_usge"])), None, Some(&out))
            .unwrap_err()
            .to_string();
        assert!(err.contains("cpu_usge"), "{err}");
        assert!(!out.exists());
    }

    fn dendro_streams(path: &Path) -> Vec<String> {
        let db = dendro::archive::Archive::open(path).unwrap();
        let id = db.read_sources().unwrap()[0].id;
        let mut s = db.all_streams(id).unwrap();
        s.sort();
        s
    }

    fn set(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    /// A filter in place replaces the input with the filtered copy, also when
    /// `-o` names the input by another spelling; an `-o` naming another file
    /// that exists is refused before anything is written.
    #[test]
    fn filter_in_place_and_an_existing_output() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("rec.dendro");
        crate::dendro_copy::fixtures::recorded(&src, 10, true);
        let all = dendro_streams(&src);
        assert!(all.len() > 2, "{all:?}");
        let taken = dir.path().join("taken.dendro");
        std::fs::write(&taken, b"theirs").unwrap();
        let err = filter_dendro(&src, Some(&set(&["threads"])), None, Some(&taken))
            .unwrap_err()
            .to_string();
        assert!(err.contains("already exists"), "{err}");
        assert_eq!(std::fs::read(&taken).unwrap(), b"theirs");
        assert_eq!(dendro_streams(&src), all);

        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let respelled = dir.path().join("sub").join("..").join("rec.dendro");
        filter_dendro(&src, Some(&set(&["threads"])), None, Some(&respelled)).unwrap();
        assert_eq!(
            dendro_streams(&src),
            vec![
                "threads/tasks".to_string(),
                "threads/tasks/occupants".to_string()
            ]
        );
    }

    /// `--samplers` keeps a long table's occupant stream with its table.
    #[test]
    fn filter_dendro_by_sampler_keeps_the_occupant_stream_with_its_table() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("rec.dendro");
        crate::dendro_copy::fixtures::recorded(&src, 10, true);
        let out = dir.path().join("threads.dendro");
        filter_dendro(&src, Some(&set(&["threads"])), None, Some(&out)).unwrap();
        assert_eq!(
            dendro_streams(&out),
            vec![
                "threads/tasks".to_string(),
                "threads/tasks/occupants".to_string()
            ]
        );
    }

    /// `--metrics` trims columns and keeps a long table long; a table with no
    /// kept metric goes, and so does its occupant stream.
    #[test]
    fn filter_dendro_by_metric_keeps_long_tables_long() {
        use metriken_query::MetricsSource;
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("rec.dendro");
        crate::dendro_copy::fixtures::recorded(&src, 10, true);

        let mem = dir.path().join("mem.dendro");
        filter_dendro(&src, None, Some(&set(&["mem_free"])), Some(&mem)).unwrap();
        assert_eq!(dendro_streams(&mem), vec!["memory/meminfo".to_string()]);

        let tasks = dir.path().join("tasks.dendro");
        filter_dendro(&src, None, Some(&set(&["task_ops"])), Some(&tasks)).unwrap();
        assert_eq!(
            dendro_streams(&tasks),
            vec![
                "threads/tasks".to_string(),
                "threads/tasks/occupants".to_string()
            ]
        );
        let open = |p: &Path| {
            ::rez::reader::RezReader::open_recordings(
                p,
                metriken_storage::BufferPool::new(64 * 1024 * 1024),
            )
            .unwrap()
            .remove(0)
            .1
        };
        let (a, b) = (open(&src), open(&tasks));
        let q = "sum by (comm) (rate(task_ops[3s]))";
        let (start, end) = a.time_range().unwrap();
        let answer = |r: &::rez::reader::RezReader| {
            let metriken_query::QueryResult::Matrix { result } =
                r.query_range(q, start, end + 1.0, 1.0).unwrap()
            else {
                panic!("a matrix");
            };
            let mut out: Vec<(String, Vec<(f64, f64)>)> = result
                .into_iter()
                .map(|s| (s.metric.get("comm").cloned().unwrap_or_default(), s.values))
                .collect();
            out.sort_by(|x, y| x.0.cmp(&y.0));
            out
        };
        let (x, y) = (answer(&a), answer(&b));
        assert_eq!(x.len(), 2, "two threads, by their labels: {x:?}");
        assert_eq!(x, y, "the kept metric answers the same");
    }

    /// The `.rez` guards hold for a dendro archive: an unknown sampler is
    /// refused, and so is a filter that would keep nothing; the input is
    /// untouched either way.
    #[test]
    fn filter_dendro_refuses_an_unknown_sampler_and_an_empty_result() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("rec.dendro");
        crate::dendro_copy::fixtures::recorded(&src, 4, true);
        let before = std::fs::read(&src).unwrap();
        let err = filter_dendro(&src, Some(&set(&["nope"])), None, None).unwrap_err();
        assert!(err.to_string().contains("no sampler named nope"), "{err}");
        let err = filter_dendro(&src, None, Some(&set(&["absent"])), None).unwrap_err();
        assert!(err.to_string().contains("kept no tables"), "{err}");
        assert_eq!(std::fs::read(&src).unwrap(), before);
    }
}
