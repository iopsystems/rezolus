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

    // `.rez` archives: drop whole per-sampler tables by --samplers (the
    // KPI-column filter no-ops on all-rezolus .rez data).
    match dispatch_format(path) {
        format @ (RezFormat::V3Sqlite | RezFormat::V2Tar) => {
            let list = args.get_one::<String>("samplers").unwrap_or_else(|| {
                eprintln!(
                    "error: filtering a .rez requires --samplers <a,b,...> (samplers to keep)"
                );
                std::process::exit(1);
            });
            let keep: std::collections::BTreeSet<String> = list
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            let output = args.get_one::<PathBuf>("output").map(|p| p.as_path());
            let result = match format {
                RezFormat::V3Sqlite => filter_rez_v3(path, &keep, output),
                _ => filter_rez(path, &keep, output),
            };
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

/// Drop whole samplers from a v3 (SQLite) `.rez`.
///
/// Filters by *sampler*, not by table key: under V3 one sampler owns several
/// `<sampler>/<group>` tables, and "drop cpu_usage" has to mean all of its
/// groups. Kept tables' segment BLOBs are copied verbatim.
///
/// Writing a new file rather than deleting in place is deliberate even though
/// SQL could `DELETE`: a delete would leave the freed pages inside the
/// original file, so the archive an operator filtered to make smaller would not
/// get smaller without a full `VACUUM` afterwards. Copying only what is kept
/// makes the output's size the point of the operation.
fn filter_rez_v3(
    path: &Path,
    keep: &std::collections::BTreeSet<String>,
    output: Option<&Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    use crate::recorder::rez::table_sampler;
    use crate::recorder::rez_sqlite::RezDb;
    use crate::recorder::rez_v3_rewrite::{copy_recordings_into, mark_copies_complete, CopySpec};

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
                keep_samplers: Some(keep),
                ..CopySpec::everything()
            };
            copy_recordings_into(src, tx, &spec)?;
            Ok(())
        })
    })?;
    mark_copies_complete(&mut dst)?;
    for rec in dst.read_recordings()? {
        kept += dst.all_samplers(rec.id)?.len();
    }
    drop(dst);
    drop(src);
    std::fs::rename(&staged, dest)?;

    println!(
        "Filtered {:?}: kept {} of {} sampler tables",
        dest, kept, total
    );
    Ok(())
}

/// Filter a v2 (tar) `.rez` archive to keep only the named per-sampler tables,
/// dropping the rest from every recording. Copies kept table bytes verbatim.
fn filter_rez(
    path: &Path,
    keep: &std::collections::BTreeSet<String>,
    output: Option<&Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    use crate::recorder::rez;
    let (manifest, recordings) = rez::read_archive_bytes(path)?;

    // A typo'd sampler name must not be read as "keep nothing". `dest` defaults
    // to the INPUT, so an unvalidated `--samplers cpu_usge` overwrote a
    // long-lived streamed capture in place with an empty manifest that
    // `RezReader` then opened quite happily, reporting no metrics.
    let present: BTreeSet<&str> = manifest
        .recordings
        .iter()
        .flat_map(|r| r.tables.iter().map(|t| t.sampler.as_str()))
        .collect();
    let unmatched: Vec<&str> = keep
        .iter()
        .map(String::as_str)
        .filter(|s| !present.contains(s))
        .collect();
    if !unmatched.is_empty() {
        return Err(format!(
            "no sampler table named {} in {}; it holds: {}",
            unmatched.join(", "),
            path.display(),
            present.iter().copied().collect::<Vec<_>>().join(", "),
        )
        .into());
    }

    // Whole tables are dropped or kept; a kept table's segments pass through
    // byte-identical.
    let mut out: Vec<rez::RecordingSegments> = Vec::new();
    let mut kept = 0usize;
    let mut total = 0usize;
    for (mut rec, rb) in manifest.recordings.into_iter().zip(recordings) {
        total += rec.tables.len();
        let mut new_tables = Vec::new();
        let mut new_bytes = Vec::new();
        for (idx, (_sampler, bytes)) in rec.tables.into_iter().zip(rb.tables) {
            if keep.contains(&idx.sampler) {
                new_tables.push(idx);
                new_bytes.push(bytes);
            }
        }
        kept += new_tables.len();
        rec.tables = new_tables;
        out.push((rec, new_bytes));
    }
    let dest = output.unwrap_or(path);
    rez::write_archive_bytes(dest, &out)?;
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
        filter_rez_v3(&path, &keep, Some(&out)).unwrap();

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
        let err = filter_rez_v3(&path, &keep, None).unwrap_err().to_string();
        assert!(
            err.contains("cpu_usge"),
            "the error must name the typo: {err}"
        );

        // And the input is untouched.
        let db = crate::recorder::rez_sqlite::RezDb::open(&path).unwrap();
        let recordings = db.read_recordings().unwrap();
        assert_eq!(db.all_samplers(recordings[0].id).unwrap().len(), 1);
    }

    #[test]
    fn filter_rez_keeps_only_named_samplers() {
        let (_d, path) = two_sampler_rez();
        let out = _d.path().join("slim.rez");
        let keep: std::collections::BTreeSet<String> =
            ["cpu_usage".to_string()].into_iter().collect();
        filter_rez(&path, &keep, Some(&out)).unwrap();
        let (m, _) = crate::recorder::rez::read_archive_bytes(&out).unwrap();
        let samplers: Vec<&str> = m.recordings[0]
            .tables
            .iter()
            .map(|t| t.sampler.as_str())
            .collect();
        assert_eq!(samplers, vec!["cpu_usage"]);
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

        let err = filter_rez(&path, &keep, None)
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
    fn tar_entry_names(path: &Path) -> Vec<String> {
        let mut names = Vec::new();
        let mut archive = tar::Archive::new(std::fs::File::open(path).unwrap());
        for entry in archive.entries().unwrap() {
            let name = entry
                .unwrap()
                .path()
                .unwrap()
                .to_string_lossy()
                .into_owned();
            if name != crate::recorder::rez::REZ_MANIFEST_NAME {
                names.push(name);
            }
        }
        names
    }

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
        filter_rez(&path, &keep, Some(&out)).unwrap();

        let (m, mut recordings) = rez::read_archive_bytes(&out).unwrap();
        let tables = recordings.remove(0).tables;
        assert_eq!(
            tables.iter().map(|(s, _)| s.as_str()).collect::<Vec<_>>(),
            vec!["cpu_usage"],
            "the dropped table is gone, segments and all"
        );
        assert_eq!(tables[0].1, kept_bytes, "all 3 segments, byte-identical");

        // The manifest index names exactly the files that were emitted.
        let idx = &m.recordings[0].tables[0];
        let expected: Vec<String> = idx
            .segment_files()
            .iter()
            .map(|f| format!("rezolus/{f}"))
            .collect();
        assert_eq!(tar_entry_names(&out), expected);
        assert_eq!(idx.files.len(), 3);
        assert!(
            !tar_entry_names(&out)
                .iter()
                .any(|n| n.contains("blockio_requests")),
            "no orphaned segment bytes from the dropped table"
        );
    }
}
