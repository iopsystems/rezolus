use clap::ArgMatches;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::parquet_metadata::{
    KEY_DESCRIPTIONS, KEY_PER_SOURCE_METADATA, KEY_SERVICE_QUERIES, KEY_SOURCE,
    NESTED_SERVICE_QUERIES,
};
use crate::recorder::rez::RezFormat;
use crate::viewer::{ServiceExtension, TemplateRegistry};

use super::annotate::extract_metric_selectors;

/// Which branch `run()` takes for `path`. Dispatch is by CONTENT, not
/// extension, and recognizes both `.rez` containers: each has its own
/// filter (`filter_rez_v3` copies segment BLOBs between SQLite catalogs,
/// `filter_rez` rewrites tar members), so a `.rez` never falls through to the
/// plain-parquet path and fails with a misleading footer or tar-parse error.
/// Split out from `run()` so it is testable without triggering `run()`'s
/// `std::process::exit`.
fn dispatch_format(path: &Path) -> RezFormat {
    crate::recorder::rez::detect_rez_format(path).unwrap_or(RezFormat::NotRez)
}

pub(super) fn run(args: &ArgMatches, registry: &TemplateRegistry) {
    let path = args.get_one::<PathBuf>("FILE").unwrap();

    // `.rez` archives: keep whole per-sampler tables by --samplers, and/or trim
    // metric COLUMNS by --metrics. `--samplers` picks which tables survive;
    // `--metrics` projects the columns inside the surviving ones.
    match dispatch_format(path) {
        format @ (RezFormat::V3Sqlite | RezFormat::V2Tar) => {
            let csv = |name: &str| -> Option<std::collections::BTreeSet<String>> {
                args.get_one::<String>(name).map(|list| {
                    list.split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                })
            };
            let samplers = csv("samplers");
            let metrics = csv("metrics");
            if samplers.is_none() && metrics.is_none() {
                eprintln!(
                    "error: filtering a .rez requires --samplers <a,b,...> (samplers to keep) \
                     and/or --metrics <a,b,...> (metric columns to keep)"
                );
                std::process::exit(1);
            }
            let output = args.get_one::<PathBuf>("output").map(|p| p.as_path());
            let result = filter_rez_any(path, format, samplers.as_ref(), metrics.as_ref(), output);
            if let Err(e) = result {
                eprintln!("error: failed to filter .rez: {e}");
                std::process::exit(1);
            }
            return;
        }
        RezFormat::NotRez => {}
    }

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
            // Exact match by column name
            if keep.contains(name.as_str()) {
                return true;
            }
            // Match the base name before ':' (e.g. "response_latency:buckets")
            if name
                .split_once(':')
                .is_some_and(|(base, _)| keep.contains(base))
            {
                return true;
            }
            // Match by "metric" field metadata (Prometheus-sourced columns use
            // numeric column names with the real metric name in metadata)
            if let Some(metric) = f.metadata().get("metric") {
                if keep.contains(metric.as_str()) {
                    return true;
                }
            }
            // Keep all rezolus (agent) columns — they provide system-level
            // context and are not referenced by service KPI queries.
            if f.metadata().get("source").is_some_and(|s| s == "rezolus") {
                return true;
            }
            false
        })
        .map(|(i, _)| i)
        .collect();

    let kept_names: BTreeSet<&str> = indices
        .iter()
        .flat_map(|&i| {
            let f = schema.field(i);
            let mut names = vec![f.name().as_str()];
            if let Some(metric) = f.metadata().get("metric") {
                names.push(metric.as_str());
            }
            names
        })
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

/// Filter a `.rez` of either container, always producing a v3 one.
///
/// A v1/v2 (tar) input is upgraded first, in a staging directory, and the
/// filter then runs on the upgraded copy. Rewriting an archive is therefore
/// also how it gets modernized — which is the point: nothing needs to write
/// the tar container any more, so nothing does.
fn filter_rez_any(
    path: &Path,
    format: RezFormat,
    keep_samplers: Option<&std::collections::BTreeSet<String>>,
    keep_metrics: Option<&std::collections::BTreeSet<String>>,
    output: Option<&Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    if format != RezFormat::V2Tar {
        return filter_rez_v3(path, keep_samplers, keep_metrics, output);
    }
    let staging = tempfile::tempdir()?;
    let staged = staging.path().join("upgraded.rez");
    crate::recorder::rez_v3_rewrite::upgrade_tar_to_v3(path, &staged)?;
    println!(
        "upgraded {:?} from a v1/v2 tar archive to v3 (SQLite)",
        path
    );
    // `output` defaults to the input, so an in-place filter of a tar archive
    // replaces it with the v3 equivalent.
    filter_rez_v3(
        &staged,
        keep_samplers,
        keep_metrics,
        Some(output.unwrap_or(path)),
    )
}

/// Filter a v3 (SQLite) `.rez` by sampler and/or by metric column.
///
/// `keep_samplers` drops whole samplers by *sampler*, not by table key: under
/// V3 one sampler owns several `<sampler>/<group>` tables, and "drop cpu_usage"
/// has to mean all of its groups. Kept tables' segment BLOBs are copied
/// verbatim.
///
/// `keep_metrics` trims metric COLUMNS inside the surviving tables — the one
/// operation that re-encodes segments (`project_segment_columns`), keeping the
/// timestamp/offset/window sidecars. A surviving table with none of the kept
/// metrics is dropped entirely rather than reduced to bare sidecars.
///
/// Writing a new archive rather than issuing a `DELETE` is deliberate, but NOT
/// because a delete would strand the freed pages — these archives are created
/// `auto_vacuum=INCREMENTAL` and [`RezDb::incremental_vacuum`] hands the pages
/// back, which is exactly how hindsight retention keeps a buffer bounded. A
/// delete would shrink the file perfectly well. The reasons are:
///
/// 1. `--output` must leave the input untouched, so that mode needs a copy no
///    matter what — and a delete-based one would have to copy the WHOLE
///    archive first and then shrink it, writing 5.8 MB to produce 1.6 MB in
///    the measured case. One path serves both modes, and it is the cheaper
///    path in the one that dominates.
/// 2. In place, a copy plus an atomic rename cannot damage the original. A
///    delete mutates it, so a failure partway through leaves the operator with
///    neither the archive they had nor the one they asked for.
/// 3. It reuses hindsight's copy, so the WAL-tail handling — a table's
///    unsealed rows, and a V3 group's leading un-anchored rows that never
///    reach a segment — has one implementation rather than two.
fn filter_rez_v3(
    path: &Path,
    keep_samplers: Option<&std::collections::BTreeSet<String>>,
    keep_metrics: Option<&std::collections::BTreeSet<String>>,
    output: Option<&Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    use crate::recorder::rez::table_sampler;
    use crate::recorder::rez_sqlite::RezDb;
    use crate::recorder::rez_v3_rewrite::{copy_recordings_into, CopySpec};

    let src = RezDb::open(path)?;

    // The same guard the v2 path carries, and for the same reason: `dest`
    // defaults to the INPUT, so an unvalidated `--samplers cpu_usge` would
    // overwrite a long-lived capture with an archive holding nothing.
    let mut present: BTreeSet<String> = BTreeSet::new();
    let mut total = 0usize;
    for rec in src.read_recordings()? {
        for table in src.all_samplers(rec.id)? {
            total += 1;
            present.insert(table_sampler(&table).to_string());
        }
    }
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

    // Staged beside the destination so the rename that publishes it is atomic
    // and on the same filesystem — and so an in-place filter never leaves a
    // half-written archive where the original was.
    let dest = output.unwrap_or(path);
    let dir = dest.parent().filter(|p| !p.as_os_str().is_empty());
    let staging = match dir {
        Some(dir) => tempfile::tempdir_in(dir),
        None => tempfile::tempdir(),
    }?;
    let staged = staging.path().join("filtered.rez");

    let mut dst = RezDb::create(&staged)?;
    let mut kept = 0usize;
    dst.transaction(|tx| {
        src.read_snapshot(|src| {
            let spec = CopySpec {
                keep_samplers,
                keep_metrics,
                ..CopySpec::everything()
            };
            copy_recordings_into(src, tx, &spec)?;
            Ok(())
        })
    })?;
    for rec in dst.read_recordings()? {
        kept += dst.all_samplers(rec.id)?.len();
    }
    drop(dst);
    drop(src);

    // A metric filter can empty the archive (no table held any kept metric).
    // `dest` defaults to the input, so publishing that would overwrite a
    // capture with nothing — the same footgun the sampler guard prevents, but
    // only detectable after the projection ran.
    if kept == 0 {
        return Err(format!(
            "filtering {} kept no tables — refusing to overwrite it with an empty archive; \
             check the --metrics / --samplers names",
            path.display(),
        )
        .into());
    }
    std::fs::rename(&staged, dest)?;

    println!(
        "Filtered {:?}: kept {} of {} sampler tables",
        dest, kept, total
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
fn filter_descriptions_metadata(
    kv_meta: &mut [parquet::file::metadata::KeyValue],
    kept: &BTreeSet<&str>,
) {
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
                    subgroup: None,
                    subgroup_description: None,
                    full_width: false,
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

    // ── dispatch_format: v3 recognition ──
    //
    // `run()` calls `std::process::exit` on several branches (including the
    // v3-not-yet-supported one), so it cannot be exercised in-process; these
    // tests cover the classification `run()` matches on instead.

    /// `dispatch_format` must recognize a v3 (SQLite) `.rez` as `V3Sqlite`
    /// (routing `run()` to the v3 filter path), not
    /// `NotRez` (which would route it to the plain-parquet path and fail with
    /// an unrelated "Corrupt footer" error).
    ///
    /// Mutation check: reverting `dispatch_format` to
    /// `is_rez_path(path).unwrap_or(false)`-based classification (mapping
    /// `true` to `V2Tar` and `false` to `NotRez`, as `run()` effectively did
    /// before migrating to `detect_rez_format`) makes this fail — `is_rez_path`
    /// is a tar sniff and reports `false` for a SQLite file.
    #[test]
    fn dispatch_format_recognizes_v3_sqlite_rez() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rec.rez");
        crate::recorder::rez::recorder_tests_support::empty_v3_rez(&path);
        assert_eq!(dispatch_format(&path), RezFormat::V3Sqlite);
    }

    /// A v2 tar `.rez` still classifies as `V2Tar` (unaffected by the v3
    /// migration).
    #[test]
    fn dispatch_format_recognizes_v2_tar_rez() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rec.rez");
        crate::recorder::rez::RezRecorder::new(
            [("source".to_string(), "rezolus".to_string())]
                .into_iter()
                .collect(),
            [("source".to_string(), "rezolus".to_string())]
                .into_iter()
                .collect(),
            "rezolus".to_string(),
        )
        .finalize(&path)
        .unwrap();
        assert_eq!(dispatch_format(&path), RezFormat::V2Tar);
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

    // ── .rez table-level filtering ──────────────────────────────────────────

    use metriken::Window;
    use metriken_exposition::{Counter, Snapshot, SnapshotV2};
    use std::collections::HashMap;
    use std::time::SystemTime;

    fn counter(name: &str, sampler: &str, v: u64, w: Option<Window>) -> Counter {
        Counter::new(
            name.to_string(),
            v,
            [
                ("metric".to_string(), name.to_string()),
                ("sampler".to_string(), sampler.to_string()),
            ]
            .into_iter()
            .collect(),
        )
        .with_window(w)
    }

    fn snap(ts: u64, counters: Vec<Counter>) -> Snapshot {
        Snapshot::V2(SnapshotV2 {
            systemtime: SystemTime::UNIX_EPOCH + std::time::Duration::from_nanos(ts),
            duration: std::time::Duration::ZERO,
            metadata: HashMap::new(),
            counters,
            gauges: Vec::new(),
            histograms: Vec::new(),
        })
    }

    /// Build a 2-sampler .rez (cpu_usage + blockio_requests) at `path`.
    fn two_sampler_rez() -> (tempfile::TempDir, PathBuf) {
        use crate::recorder::rez::RezRecorder;
        let mut r = RezRecorder::new(
            [("source".to_string(), "rezolus".to_string())]
                .into_iter()
                .collect(),
            [("source".to_string(), "rezolus".to_string())]
                .into_iter()
                .collect(),
            "rezolus".to_string(),
        );
        for i in 0..3u64 {
            let ts = 1_000_000_000 * (i + 1);
            let w = Some(Window::new(ts - 50_000_000, ts));
            r.ingest(
                &snap(
                    ts,
                    vec![
                        counter("cpu_cycles", "cpu_usage", i, w),
                        counter("reads", "blockio_requests", i, w),
                    ],
                ),
                ts,
            );
        }
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("two.rez");
        r.finalize(&out).unwrap();
        (dir, out)
    }

    /// Filtering a v3 archive keeps the named samplers and drops the rest —
    /// and the survivors keep their rows. Dropping a table is easy; dropping
    /// it without taking a kept table's segments with it is the part worth
    /// asserting.
    #[test]
    fn filter_rez_v3_keeps_only_named_samplers() {
        use crate::recorder::rez::recorder_tests_support::populated_v3_rez;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("full.rez");
        populated_v3_rez(&path, "baseline", &["cpu_usage", "scheduler", "network"], 6);
        let out = dir.path().join("slim.rez");

        let keep: std::collections::BTreeSet<String> =
            ["cpu_usage".to_string()].into_iter().collect();
        filter_rez_v3(&path, Some(&keep), None, Some(&out)).unwrap();

        let db = crate::recorder::rez_sqlite::RezDb::open(&out).unwrap();
        let recordings = db.read_recordings().unwrap();
        assert_eq!(recordings.len(), 1);
        let tables = db.all_samplers(recordings[0].id).unwrap();
        assert_eq!(
            tables,
            vec!["cpu_usage".to_string()],
            "only the kept sampler survives"
        );
        assert!(
            db.total_rows(recordings[0].id, "cpu_usage").unwrap() > 0,
            "the kept sampler must keep its rows, not just its name"
        );
    }

    /// `--metrics` keeps only tables that hold a kept metric. In this fixture
    /// each sampler table carries a single metric named by its index, so
    /// keeping "0" keeps the first sampler's table and drops the rest — a
    /// table with none of the kept metrics projects to no value column and is
    /// dropped whole, and the kept table keeps its rows.
    #[test]
    fn filter_rez_v3_metrics_keeps_only_tables_with_a_kept_metric() {
        use crate::recorder::rez::recorder_tests_support::populated_v3_rez;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("full.rez");
        populated_v3_rez(&path, "baseline", &["cpu_usage", "scheduler", "network"], 6);
        let out = dir.path().join("slim.rez");

        let metrics: std::collections::BTreeSet<String> = ["0".to_string()].into_iter().collect();
        filter_rez_v3(&path, None, Some(&metrics), Some(&out)).unwrap();

        let db = crate::recorder::rez_sqlite::RezDb::open(&out).unwrap();
        let recordings = db.read_recordings().unwrap();
        assert_eq!(recordings.len(), 1);
        assert_eq!(
            db.all_samplers(recordings[0].id).unwrap(),
            vec!["cpu_usage".to_string()],
            "only the table holding metric \"0\" survives"
        );
        assert!(
            db.total_rows(recordings[0].id, "cpu_usage").unwrap() > 0,
            "the kept table keeps its rows through the re-encode"
        );
    }

    /// The output must still open and query after a column re-encode — the
    /// projected segment has to be a valid `.rez` the reader accepts, not just
    /// a smaller blob. Round-trips through `RezReader` and reads the metric.
    #[test]
    fn filter_rez_v3_metrics_output_is_readable() {
        use crate::recorder::rez::recorder_tests_support::populated_v3_rez;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("full.rez");
        populated_v3_rez(&path, "baseline", &["cpu_usage", "scheduler"], 6);
        let out = dir.path().join("slim.rez");

        let metrics: std::collections::BTreeSet<String> = ["0".to_string()].into_iter().collect();
        filter_rez_v3(&path, None, Some(&metrics), Some(&out)).unwrap();

        let pool = metriken_query::BufferPool::new(8 * 1024 * 1024);
        let readers = crate::rez_reader::RezReader::open_recordings(&out, pool).unwrap();
        assert_eq!(readers.len(), 1);
        let (_labels, reader) = &readers[0];
        assert!(
            reader.metric_metadata().contains_key("0"),
            "the kept metric is still readable after the re-encode"
        );
    }

    /// A `--metrics` value that matches nothing must not empty the archive in
    /// place — the same footgun the sampler guard prevents, but only detectable
    /// after the projection runs (kept == 0).
    #[test]
    fn filter_rez_v3_metrics_that_match_nothing_are_rejected() {
        use crate::recorder::rez::recorder_tests_support::populated_v3_rez;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("full.rez");
        populated_v3_rez(&path, "baseline", &["cpu_usage"], 4);

        let metrics: std::collections::BTreeSet<String> =
            ["not_a_metric".to_string()].into_iter().collect();
        let err = filter_rez_v3(&path, None, Some(&metrics), None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("empty"), "explains the refusal: {err}");

        // Input untouched.
        let db = crate::recorder::rez_sqlite::RezDb::open(&path).unwrap();
        let recordings = db.read_recordings().unwrap();
        assert_eq!(db.all_samplers(recordings[0].id).unwrap().len(), 1);
    }

    /// A typo'd sampler name must not be read as "keep nothing". `--output`
    /// defaults to the INPUT, so this once overwrote a long-lived capture in
    /// place with an archive holding no metrics at all.
    #[test]
    fn filter_rez_v3_rejects_an_unmatched_sampler_instead_of_emptying_the_archive() {
        use crate::recorder::rez::recorder_tests_support::populated_v3_rez;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("full.rez");
        populated_v3_rez(&path, "baseline", &["cpu_usage"], 4);

        let keep: std::collections::BTreeSet<String> =
            ["cpu_usge".to_string()].into_iter().collect();
        let err = filter_rez_v3(&path, Some(&keep), None, None)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("cpu_usge"),
            "the error must name the typo: {err}"
        );

        // And the input is untouched.
        let db = crate::recorder::rez_sqlite::RezDb::open(&path).unwrap();
        let recordings = db.read_recordings().unwrap();
        assert_eq!(db.all_samplers(recordings[0].id).unwrap().len(), 1);
    }

    /// Filtering a v1/v2 tar archive keeps the named samplers AND upgrades the
    /// container on the way out, because rewriting an archive is now also how
    /// it gets modernized.
    #[test]
    fn filter_rez_upgrades_a_tar_archive_and_keeps_only_named_samplers() {
        use crate::recorder::rez::{detect_rez_format, RezFormat};
        let (_d, path) = two_sampler_rez();
        let out = _d.path().join("slim.rez");
        let keep: std::collections::BTreeSet<String> =
            ["cpu_usage".to_string()].into_iter().collect();
        assert_eq!(detect_rez_format(&path).unwrap(), RezFormat::V2Tar);

        filter_rez_any(&path, RezFormat::V2Tar, Some(&keep), None, Some(&out)).unwrap();

        assert_eq!(
            detect_rez_format(&out).unwrap(),
            RezFormat::V3Sqlite,
            "the filtered output is v3 even though the input was a tar archive"
        );
        let db = crate::recorder::rez_sqlite::RezDb::open(&out).unwrap();
        let recordings = db.read_recordings().unwrap();
        assert_eq!(recordings.len(), 1);
        assert_eq!(
            db.all_samplers(recordings[0].id).unwrap(),
            vec!["cpu_usage".to_string()]
        );
        assert!(
            db.total_rows(recordings[0].id, "cpu_usage").unwrap() > 0,
            "the kept sampler must keep its rows across the upgrade"
        );
    }

    // `dest` defaults to the input, so an unmatched `--samplers` name used to
    // overwrite a (possibly multi-day) capture in place with an empty archive
    // and still exit Ok.
    #[test]
    fn filter_rez_rejects_a_sampler_that_is_not_present() {
        let (_d, path) = two_sampler_rez();
        let before = std::fs::read(&path).unwrap();
        let keep: std::collections::BTreeSet<String> =
            ["cpu_usge".to_string()].into_iter().collect();

        let err = filter_rez_any(
            &path,
            crate::recorder::rez::RezFormat::V2Tar,
            Some(&keep),
            None,
            None,
        )
        .expect_err("an unmatched sampler name must be an error, not an empty archive");
        let msg = err.to_string();
        assert!(
            msg.contains("cpu_usge"),
            "names the unmatched sampler: {msg}"
        );
        assert!(msg.contains("cpu_usage"), "names what is present: {msg}");
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "the input must be left untouched"
        );
    }

    /// Every `<dir>/<file>` data entry in the tar, in write order.

    // Dropping a table from a segmented archive must drop *all* of its segments
    // and keep *all* of the survivor's, and the rewritten manifest must name
    // exactly the files the tar now holds — a stale index would name segments
    // the output never wrote and fail every reader at open.
    #[test]
    fn filter_rez_drops_whole_segmented_tables_and_reindexes() {
        use crate::recorder::rez;
        use crate::recorder::rez_stream::write_segmented_rez;

        let d = tempfile::tempdir().unwrap();
        let path = write_segmented_rez(
            &d.path().join("full.rez"),
            "rezolus",
            Default::default(),
            &["cpu_usage", "blockio_requests"],
            6,
            2,
            true,
        );
        let before = rez::read_archive_bytes(&path).unwrap().1.remove(0).tables;
        assert!(
            before.iter().all(|(_, s)| s.len() == 3),
            "the input must actually be segmented"
        );
        let kept_bytes = before
            .iter()
            .find(|(s, _)| s == "cpu_usage")
            .unwrap()
            .1
            .clone();

        let out = d.path().join("slim.rez");
        let keep: std::collections::BTreeSet<String> =
            ["cpu_usage".to_string()].into_iter().collect();
        filter_rez_any(&path, rez::RezFormat::V2Tar, Some(&keep), None, Some(&out)).unwrap();

        // The output is v3, so the assertions are about its catalog rather
        // than tar entries — but the property under test is the same one: a
        // multi-segment table survives the filter with every segment intact
        // and in order, and the dropped table leaves nothing behind.
        assert_eq!(
            rez::detect_rez_format(&out).unwrap(),
            rez::RezFormat::V3Sqlite
        );
        let db = crate::recorder::rez_sqlite::RezDb::open(&out).unwrap();
        let recordings = db.read_recordings().unwrap();
        assert_eq!(
            db.all_samplers(recordings[0].id).unwrap(),
            vec!["cpu_usage".to_string()],
            "the dropped table is gone, segments and all"
        );
        let segments = db.read_segments(recordings[0].id, "cpu_usage").unwrap();
        assert_eq!(segments.len(), 3, "all 3 segments came across");
        let got: Vec<Vec<u8>> = segments.iter().map(|s| s.bytes.clone()).collect();
        assert_eq!(
            got, kept_bytes,
            "segment parquet BLOBs are carried verbatim, in order — an upgrade \
             changes the container, not the data"
        );
        assert_eq!(
            segments.iter().map(|s| s.seq).collect::<Vec<_>>(),
            vec![0, 1, 2],
            "seq is dense and starts at zero so the reader splices in order"
        );
    }
}
