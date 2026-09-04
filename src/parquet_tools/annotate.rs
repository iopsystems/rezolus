use clap::ArgMatches;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::parquet_metadata::{
    KEY_NODE, KEY_PER_SOURCE_METADATA, KEY_SERVICE_QUERIES, KEY_SOURCE, KEY_SYSTEMINFO,
};
use crate::recorder::rez::RezFormat;
use crate::viewer::{ServiceExtension, TemplateRegistry};
use metriken_query::{MetricsSource, ParquetReader};

/// Which branch `run()` takes for `path`. Dispatch is by CONTENT, not
/// extension, and recognizes both `.rez` containers. `annotate_rez` rewrites
/// through `read_archive_bytes`/`write_archive_bytes`, which only speak the
/// v2 tar container, so a v3 (SQLite) archive gets an explicit "not yet
/// supported" error in `run()` rather than silently falling through to the
/// plain-parquet path (which would fail with a confusing footer error) or a
/// misleading tar-parse error. Split out from `run()` so it is testable
/// without triggering `run()`'s `std::process::exit` on the v3/error arms.
fn dispatch_format(path: &Path) -> RezFormat {
    crate::recorder::rez::detect_rez_format(path).unwrap_or(RezFormat::NotRez)
}

pub(super) fn run(args: &ArgMatches, registry: &TemplateRegistry) {
    let path = args.get_one::<PathBuf>("FILE").unwrap();

    match dispatch_format(path) {
        format @ (RezFormat::V3Sqlite | RezFormat::V2Tar) => {
            run_rez(args, path, format);
            return;
        }
        RezFormat::NotRez => {}
    }

    let node = args.get_one::<String>("node");
    let new_source = args.get_one::<String>("source");
    let sysinfo_path = args.get_one::<PathBuf>("systeminfo");
    let overwrite = args.get_flag("overwrite");

    let event_files: Vec<&Path> = args
        .get_many::<PathBuf>("add-events")
        .map(|it| it.map(PathBuf::as_path).collect())
        .unwrap_or_default();
    let inline_events: Vec<String> = args
        .get_many::<String>("event")
        .map(|it| it.cloned().collect())
        .unwrap_or_default();
    let clear_events = args.get_flag("clear-events");
    let events_requested = clear_events || !event_files.is_empty() || !inline_events.is_empty();

    if let Some(n) = node {
        set_node_metadata(path, n).unwrap_or_else(|e| {
            eprintln!("error: failed to set node metadata: {e}");
            std::process::exit(1);
        });
        println!("Set node={:?} on {:?}", n, path);
    }

    if let Some(p) = sysinfo_path {
        run_systeminfo(path, p);
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

    if events_requested {
        super::events::run(path, &event_files, &inline_events, clear_events).unwrap_or_else(|e| {
            eprintln!("error: {e}");
            std::process::exit(1);
        });
    }

    let custom_file = args.get_one::<PathBuf>("queries");

    // If only individual edits (--node/--source/--systeminfo/events) were
    // requested, don't also auto-apply a default service template.
    if (node.is_some() || new_source.is_some() || sysinfo_path.is_some() || events_requested)
        && custom_file.is_none()
        && !args.get_flag("filter")
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

/// Embed (or replace) the `systeminfo` JSON metadata in the parquet footer.
fn run_systeminfo(path: &Path, source: &Path) {
    let json = if source.as_os_str() == "-" {
        let mut buf = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf).unwrap_or_else(|e| {
            eprintln!("error: failed to read systeminfo from stdin: {e}");
            std::process::exit(1);
        });
        buf
    } else {
        std::fs::read_to_string(source).unwrap_or_else(|e| {
            eprintln!("error: failed to read {source:?}: {e}");
            std::process::exit(1);
        })
    };

    if let Err(e) = serde_json::from_str::<serde_json::Value>(&json) {
        eprintln!("error: systeminfo is not valid JSON: {e}");
        std::process::exit(1);
    }

    annotate_systeminfo(path, &json).unwrap_or_else(|e| {
        eprintln!("error: failed to write systeminfo annotation: {e}");
        std::process::exit(1);
    });

    println!("Annotated {path:?} with systeminfo ({} bytes)", json.len());
}

fn annotate_systeminfo(path: &Path, json: &str) -> Result<(), Box<dyn std::error::Error>> {
    use parquet::file::metadata::KeyValue;

    let mut kv_meta = super::read_file_metadata(path)?;

    kv_meta.retain(|kv| kv.key != KEY_SYSTEMINFO);
    kv_meta.push(KeyValue {
        key: KEY_SYSTEMINFO.to_string(),
        value: Some(json.to_string()),
    });

    let buf = super::rewrite_parquet(path, kv_meta, None)?;
    std::fs::write(path, &buf)?;
    Ok(())
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
    let reader = match ParquetReader::open(path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("warning: could not load parquet for validation: {e}");
            return;
        }
    };

    let (start, end) = reader.time_range().unwrap_or((0.0, 0.0));
    let step = 1.0;

    let mut matched = 0;
    let mut missing_metrics = BTreeSet::new();

    for kpi in &mut ext.kpis {
        let query = kpi.effective_query();
        let has_data = match reader.query_range(&query, start, end, step) {
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

/// The manifest edits one `annotate` invocation applies to every recording of
/// a `.rez`. Both fields are optional; `run_rez` guarantees at least one is
/// present, since an annotate that changes nothing is a mistake worth an error.
struct RezAnnotation<'a> {
    /// Validated ServiceExtension JSON to embed under `KEY_SERVICE_QUERIES`
    /// (KPIs). Re-probed against each recording's own metrics.
    ext_json: Option<&'a str>,
    /// Event operations to fold into each recording's `KEY_EVENTS` payload.
    events: Option<EventOps<'a>>,
}

/// Event operations for a `.rez` annotate, applied in the same
/// `clear → add file → add inline` order as the parquet footer path.
struct EventOps<'a> {
    add_files: &'a [&'a Path],
    inline: &'a [String],
    clear: bool,
}

/// Collect the KPI and event operations from the CLI and apply them to a
/// `.rez` of either container. `--queries` and/or the event flags
/// (`--event`/`--add-events`/`--clear-events`) drive it; a `.rez` has no
/// built-in service template, so at least one must be given.
///
/// Footer-shaped flags (`--undo`/`--node`/`--source`/`--systeminfo`) have no
/// per-recording manifest analogue wired yet, so they warn rather than
/// silently no-op.
fn run_rez(args: &ArgMatches, path: &Path, format: RezFormat) {
    let queries = args.get_one::<PathBuf>("queries");
    let event_files: Vec<&Path> = args
        .get_many::<PathBuf>("add-events")
        .map(|it| it.map(PathBuf::as_path).collect())
        .unwrap_or_default();
    let inline_events: Vec<String> = args
        .get_many::<String>("event")
        .map(|it| it.cloned().collect())
        .unwrap_or_default();
    let clear_events = args.get_flag("clear-events");
    let events_requested = clear_events || !event_files.is_empty() || !inline_events.is_empty();

    for (present, label) in [
        (args.get_flag("undo"), "--undo"),
        (args.get_one::<String>("node").is_some(), "--node"),
        (args.get_one::<String>("source").is_some(), "--source"),
        (
            args.get_one::<PathBuf>("systeminfo").is_some(),
            "--systeminfo",
        ),
    ] {
        if present {
            eprintln!("warning: {label} is not supported on a .rez archive; ignoring");
        }
    }

    if queries.is_none() && !events_requested {
        eprintln!(
            "error: annotating a .rez needs --queries <service-extension.json> and/or event \
             flags (--event/--add-events/--clear-events); a .rez has no built-in service template"
        );
        std::process::exit(1);
    }

    let ext_content = queries.map(|custom| {
        let content = std::fs::read_to_string(custom).unwrap_or_else(|e| {
            eprintln!("error: failed to read {custom:?}: {e}");
            std::process::exit(1);
        });
        // parse-validate up front
        let _: ServiceExtension = serde_json::from_str(&content).unwrap_or_else(|e| {
            eprintln!("error: invalid service extension JSON: {e}");
            std::process::exit(1);
        });
        content
    });

    let events = events_requested.then(|| EventOps {
        add_files: &event_files,
        inline: &inline_events,
        clear: clear_events,
    });

    let annotation = RezAnnotation {
        ext_json: ext_content.as_deref(),
        events,
    };

    annotate_rez_any(path, format, &annotation).unwrap_or_else(|e| {
        eprintln!("error: failed to annotate .rez: {e}");
        std::process::exit(1);
    });
}

/// Annotate a `.rez` of either container, always leaving a v3 one behind.
///
/// A v1/v2 (tar) archive is upgraded in place first: annotating is a rewrite,
/// and the tar container is no longer something this binary writes.
fn annotate_rez_any(
    path: &Path,
    format: RezFormat,
    annotation: &RezAnnotation,
) -> Result<(), Box<dyn std::error::Error>> {
    if format != RezFormat::V2Tar {
        return annotate_rez_v3(path, annotation);
    }
    let staging = tempfile::tempdir_in(
        path.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
    )?;
    let staged = staging.path().join("upgraded.rez");
    crate::recorder::rez_v3_rewrite::upgrade_tar_to_v3(path, &staged)?;
    annotate_rez_v3_at(&staged, path, annotation)?;
    // Staged beside the target so this rename is atomic and same-filesystem:
    // the original survives untouched until the annotated copy is complete.
    std::fs::rename(&staged, path)?;
    println!(
        "upgraded {:?} from a v1/v2 tar archive to v3 (SQLite)",
        path
    );
    Ok(())
}

/// Embed KPIs and/or events into every recording of a v3 (SQLite) `.rez`.
///
/// Unlike `combine` and `filter` this rewrites nothing: KPIs and events are
/// catalog columns, so annotating is an `UPDATE` per recording. No segment is
/// read or copied, which is why annotating a 4 GB archive costs the same as
/// annotating a 4 MB one.
fn annotate_rez_v3(
    path: &Path,
    annotation: &RezAnnotation,
) -> Result<(), Box<dyn std::error::Error>> {
    annotate_rez_v3_at(path, path, annotation)
}

/// `annotate_rez_v3`, with the archive being written and the name to report
/// separated. They differ when a tar archive is annotated: the work happens on
/// an upgraded staging copy, but the operator asked about — and will go on
/// using — the original path, so that is the one worth naming.
fn annotate_rez_v3_at(
    path: &Path,
    display: &Path,
    annotation: &RezAnnotation,
) -> Result<(), Box<dyn std::error::Error>> {
    use crate::parquet_metadata::KEY_EVENTS;
    use crate::recorder::rez_sqlite::RezDb;
    use crate::viewer::Events;

    let pool = metriken_query::BufferPool::new(256 * 1024 * 1024);
    let readers = crate::rez_reader::RezReader::open_recordings(path, pool)?;

    let db = RezDb::open(path)?;
    let recordings = db.read_recordings()?;
    if recordings.len() != readers.len() {
        return Err(format!(
            "{} has {} recording(s) in its catalog but {} readable — refusing to annotate a \
             partially readable archive",
            path.display(),
            recordings.len(),
            readers.len()
        )
        .into());
    }

    let mut kpis: Option<usize> = None;
    // (events remaining after the ops, whether they were cleared first) — the
    // input is identical for every recording, so the last one's counts stand
    // in for the report.
    let mut events_report: Option<(usize, bool)> = None;
    for (rec, (_labels, reader)) in recordings.iter().zip(readers) {
        let mut metadata = rec.meta.metadata.clone();

        if let Some(ext_json) = annotation.ext_json {
            let mut ext: ServiceExtension = serde_json::from_str(ext_json)?;
            // Validated per recording, not once: a KPI's query is only
            // meaningful against the metrics that recording actually holds, and
            // a multi-recording archive can hold different sets.
            validate_kpis_source(&reader, &mut ext);
            kpis = Some(ext.kpis.len());
            metadata.insert(
                crate::parquet_metadata::KEY_SERVICE_QUERIES.to_string(),
                serde_json::to_string(&ext)?,
            );
        }

        if let Some(ops) = &annotation.events {
            let existing = metadata
                .get(KEY_EVENTS)
                .map(|s| serde_json::from_str::<Events>(s))
                .transpose()
                .map_err(|e| format!("recording has an invalid events payload: {e}"))?;
            let (events, _added, _dropped) =
                super::events::apply_event_ops(existing, ops.add_files, ops.inline, ops.clear)?;
            // An empty payload drops the key rather than storing `{"events":[]}`,
            // matching the parquet footer path (`write_events`).
            if events.events.is_empty() {
                metadata.remove(KEY_EVENTS);
            } else {
                metadata.insert(KEY_EVENTS.to_string(), serde_json::to_string(&events)?);
            }
            events_report = Some((events.events.len(), ops.clear));
        }

        db.update_recording_metadata(rec.id, &metadata)?;
    }

    let mut parts: Vec<String> = Vec::new();
    if let Some(k) = kpis {
        parts.push(format!("{k} KPIs"));
    }
    if let Some((total, cleared)) = events_report {
        parts.push(if cleared {
            format!("{total} event(s) (replaced)")
        } else {
            format!("{total} event(s)")
        });
    }
    println!(
        "Annotated {:?}: embedded {} into {} recording(s)",
        display,
        parts.join(" and "),
        recordings.len()
    );
    Ok(())
}

/// Set each KPI's `available` flag by probing a `MetricsSource` (works for both
/// ParquetReader and RezReader — mirrors `validate_kpis` without the path open).
fn validate_kpis_source(reader: &dyn MetricsSource, ext: &mut ServiceExtension) {
    let (start, end) = reader.time_range().unwrap_or((0.0, 0.0));
    for kpi in &mut ext.kpis {
        let query = kpi.effective_query();
        kpi.available = match reader.query_range(&query, start, end, 1.0) {
            Ok(result) => !query_result_is_empty(&result),
            Err(_) => false,
        };
    }
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

fn query_result_is_empty(result: &metriken_query::QueryResult) -> bool {
    use metriken_query::QueryResult;
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
/// - If `source` differs and `overwrite=true`: replaces the top-level
///   `source` key. If `per_source_metadata` carries an entry keyed by
///   the old source name, that entry is renamed in place so the nested
///   structure stays consistent with the new top-level value. Other
///   `per_source_metadata` keys (e.g. `rezolus`) are untouched.
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
        .and_then(|kv| kv.value.as_deref())
        .map(str::to_string);

    match existing.as_deref() {
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

    // Rename the matching per_source_metadata sub-entry, if present, so
    // the nested structure stays in sync with the new top-level source.
    if let Some(old_source) = existing.filter(|cur| cur != value) {
        if let Some(idx) = kv_meta
            .iter()
            .position(|kv| kv.key == KEY_PER_SOURCE_METADATA)
        {
            if let Some(raw) = kv_meta[idx].value.as_deref() {
                if let Ok(mut psm) =
                    serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(raw)
                {
                    if let Some(entry) = psm.remove(&old_source) {
                        psm.insert(value.to_string(), entry);
                        kv_meta[idx].value = Some(serde_json::to_string(&psm)?);
                    }
                }
            }
        }
    }

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

    // ── dispatch_format: v3 recognition ──
    //
    // `run()` calls `std::process::exit` on several branches (including the
    // v3-not-yet-supported one), so it cannot be exercised in-process; these
    // tests cover the classification `run()` matches on instead.

    /// `dispatch_format` must recognize a v3 (SQLite) `.rez` as `V3Sqlite`
    /// (routing `run()` to the v3 annotate path), not
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
    fn set_source_overwrite_renames_per_source_metadata_key() {
        // File has `source=vllm` and a per_source_metadata entry keyed
        // by "vllm". Overwriting source to "sglang" should rename the
        // PSM entry as well so the structure stays consistent.
        let psm = r#"{"vllm":{"0":{"role":"service","instance":"0"}}}"#;
        let tmp = make_minimal_parquet(vec![("source", "vllm"), ("per_source_metadata", psm)]);
        set_source_metadata(tmp.path(), "sglang", true).unwrap();

        let kv = read_kv(tmp.path());
        let psm_str = kv
            .iter()
            .find(|(k, _)| k == "per_source_metadata")
            .map(|(_, v)| v.as_str())
            .expect("per_source_metadata should still be present");
        let parsed: serde_json::Value = serde_json::from_str(psm_str).unwrap();

        assert!(parsed.get("vllm").is_none(), "old key should be gone");
        let renamed = parsed
            .get("sglang")
            .expect("entry should have been renamed to the new source");
        assert_eq!(renamed["0"]["role"], serde_json::json!("service"));
    }

    #[test]
    fn set_source_overwrite_preserves_unrelated_per_source_keys() {
        // File has `source=vllm` and a per_source_metadata that includes
        // both "vllm" and "rezolus" entries. Overwriting source should
        // rename only the matching key.
        let psm = r#"{"vllm":{"0":{"role":"service"}},"rezolus":{"web01":{"role":"service","node":"web01"}}}"#;
        let tmp = make_minimal_parquet(vec![("source", "vllm"), ("per_source_metadata", psm)]);
        set_source_metadata(tmp.path(), "sglang", true).unwrap();

        let kv = read_kv(tmp.path());
        let psm_str = kv
            .iter()
            .find(|(k, _)| k == "per_source_metadata")
            .map(|(_, v)| v.as_str())
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(psm_str).unwrap();

        assert!(parsed.get("vllm").is_none());
        assert!(parsed.get("sglang").is_some());
        assert_eq!(
            parsed["rezolus"]["web01"]["node"],
            serde_json::json!("web01")
        );
    }

    #[test]
    fn set_source_overwrite_when_old_source_absent_from_per_source_metadata() {
        // File has `source=vllm` but the per_source_metadata payload
        // has no "vllm" key (e.g. the user sourced the file but never
        // ran a recorder that populated PSM for it). Overwriting source
        // should still update the top-level key without touching PSM.
        let psm = r#"{"rezolus":{"web01":{"role":"service"}}}"#;
        let tmp = make_minimal_parquet(vec![("source", "vllm"), ("per_source_metadata", psm)]);
        set_source_metadata(tmp.path(), "sglang", true).unwrap();

        let kv = read_kv(tmp.path());
        let psm_str = kv
            .iter()
            .find(|(k, _)| k == "per_source_metadata")
            .map(|(_, v)| v.as_str())
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(psm_str).unwrap();

        // Unrelated entry preserved; no spurious "sglang" key created
        // because the rename target didn't exist in the source file.
        assert!(parsed.get("rezolus").is_some());
        assert!(parsed.get("vllm").is_none());
        assert!(parsed.get("sglang").is_none());
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

    // ── annotate_systeminfo tests ──

    #[test]
    fn systeminfo_annotation_replaces_existing_key() {
        let tmp = make_minimal_parquet(vec![("systeminfo", "\"old\"")]);
        let new_json = r#"{"cpu":"x86_64","cores":8}"#;
        annotate_systeminfo(tmp.path(), new_json).unwrap();

        let kv = read_kv(tmp.path());
        let entries: Vec<&str> = kv
            .iter()
            .filter(|(k, _)| k == KEY_SYSTEMINFO)
            .map(|(_, v)| v.as_str())
            .collect();
        assert_eq!(entries, vec![new_json], "exactly one systeminfo entry");
    }

    // ── annotate_rez tests ──

    /// A KPIs-only annotation, the shape every pre-events test used.
    fn kpi_annotation(ext_json: &str) -> RezAnnotation<'_> {
        RezAnnotation {
            ext_json: Some(ext_json),
            events: None,
        }
    }

    /// An events-only annotation adding the given inline events.
    fn inline_events_annotation(inline: &[String]) -> RezAnnotation<'_> {
        RezAnnotation {
            ext_json: None,
            events: Some(EventOps {
                add_files: &[],
                inline,
                clear: false,
            }),
        }
    }

    /// Build a single-recording `.rez` fixture on disk; return (tempdir, path).
    fn build_one_recording_rez() -> (tempfile::TempDir, PathBuf) {
        use crate::recorder::rez::RezRecorder;
        use metriken::Window;
        use metriken_exposition::{Counter, Snapshot, SnapshotV2};
        use std::time::SystemTime;

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
            let c = Counter::new(
                "cpu_cycles".to_string(),
                i,
                [
                    ("metric".to_string(), "cpu_cycles".to_string()),
                    ("sampler".to_string(), "cpu_usage".to_string()),
                ]
                .into_iter()
                .collect(),
            )
            .with_window(w);
            let snap = Snapshot::V2(SnapshotV2 {
                systemtime: SystemTime::UNIX_EPOCH + std::time::Duration::from_nanos(ts),
                duration: std::time::Duration::ZERO,
                metadata: HashMap::new(),
                counters: vec![c],
                gauges: Vec::new(),
                histograms: Vec::new(),
            });
            r.ingest(&snap, ts);
        }
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("one.rez");
        r.finalize(&out).unwrap();
        (dir, out)
    }

    /// Annotating a v3 archive writes the KPI set into each recording's
    /// metadata column, in place, without disturbing the segments.
    #[test]
    fn annotate_rez_v3_embeds_service_queries_into_recordings() {
        use crate::recorder::rez::recorder_tests_support::populated_v3_rez;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rec.rez");
        populated_v3_rez(&path, "baseline", &["cpu_usage"], 6);
        let before = std::fs::metadata(&path).unwrap().len();

        let ext_json = r#"{"service_name":"test","kpis":[{"role":"overview","title":"Zero","query":"0","type":"gauge"}]}"#;
        annotate_rez_v3(&path, &kpi_annotation(ext_json)).unwrap();

        let db = crate::recorder::rez_sqlite::RezDb::open(&path).unwrap();
        let recordings = db.read_recordings().unwrap();
        assert_eq!(recordings.len(), 1);
        let embedded = recordings[0]
            .meta
            .metadata
            .get(crate::parquet_metadata::KEY_SERVICE_QUERIES)
            .expect("service queries must be embedded in the recording metadata");
        assert!(
            embedded.contains("test"),
            "the extension's own content must survive"
        );
        // Pre-existing metadata is preserved, not replaced wholesale.
        assert!(
            recordings[0]
                .meta
                .metadata
                .contains_key("sampling_interval_ms"),
            "annotating must merge into the recording's metadata, not overwrite it"
        );
        // Segments were never rewritten, so the file does not balloon.
        let after = std::fs::metadata(&path).unwrap().len();
        assert!(
            after <= before + 64 * 1024,
            "annotate must not rewrite segments: {before} -> {after}"
        );
    }

    #[test]
    fn annotate_rez_embeds_service_queries_into_recordings() {
        let (_d, path) = build_one_recording_rez();
        let ext_json = r#"{"service_name":"test","kpis":[{"role":"overview","title":"Cycles","query":"cpu_cycles","type":"counter"}]}"#;

        annotate_rez_any(
            &path,
            crate::recorder::rez::RezFormat::V2Tar,
            &kpi_annotation(ext_json),
        )
        .unwrap();

        // Annotating a tar archive upgrades it in place, so it reads back as
        // v3 — the KPIs land in the recording's metadata column rather than a
        // manifest.
        assert_eq!(
            crate::recorder::rez::detect_rez_format(&path).unwrap(),
            crate::recorder::rez::RezFormat::V3Sqlite
        );
        let db = crate::recorder::rez_sqlite::RezDb::open(&path).unwrap();
        let recordings = db.read_recordings().unwrap();
        assert!(recordings
            .iter()
            .all(|rec| rec.meta.metadata.contains_key(KEY_SERVICE_QUERIES)));
        // The embedded JSON round-trips to a ServiceExtension.
        let embedded = recordings[0]
            .meta
            .metadata
            .get(KEY_SERVICE_QUERIES)
            .unwrap();
        let ext: ServiceExtension = serde_json::from_str(embedded).unwrap();
        assert_eq!(ext.service_name, "test");
        assert_eq!(ext.kpis.len(), 1);
    }

    // `annotate` rewrites the archive in place, so it re-reads and re-writes
    // every segment; and it validates KPIs through `RezReader`, which on a
    // segmented archive is the splicing path. Both have to hold: the metadata
    // lands, and not one byte of table data moves.
    #[test]
    fn annotate_rez_leaves_segments_untouched() {
        use crate::recorder::rez;
        use crate::recorder::rez_stream::write_segmented_rez;

        let d = tempfile::tempdir().unwrap();
        let path = write_segmented_rez(
            &d.path().join("seg.rez"),
            "rezolus",
            Default::default(),
            &["cpu_usage"],
            6,
            2,
            true,
        );
        let before = rez::read_archive_bytes(&path).unwrap().1.remove(0).tables;
        assert_eq!(before[0].1.len(), 3, "the input must actually be segmented");

        // The KPI queries a metric the fixture really has, so `available`
        // resolves through the segmented reader rather than short-circuiting.
        let ext_json = r#"{"service_name":"seg","kpis":[{"role":"overview","title":"Ops","query":"rate(cpu_usage_ops[2s])","type":"counter"}]}"#;
        annotate_rez_any(
            &path,
            crate::recorder::rez::RezFormat::V2Tar,
            &kpi_annotation(ext_json),
        )
        .unwrap();

        let db = crate::recorder::rez_sqlite::RezDb::open(&path).unwrap();
        let recordings = db.read_recordings().unwrap();
        let segments = db.read_segments(recordings[0].id, "cpu_usage").unwrap();
        assert_eq!(
            segments.iter().map(|s| s.bytes.clone()).collect::<Vec<_>>(),
            before[0].1,
            "segment bytes pass through byte-identical — an upgrade changes the \
             container, not the data"
        );

        let embedded = recordings[0]
            .meta
            .metadata
            .get(KEY_SERVICE_QUERIES)
            .expect("the KPI metadata landed in the recording");
        let ext: ServiceExtension = serde_json::from_str(embedded).unwrap();
        assert_eq!(ext.service_name, "seg");
        assert_eq!(ext.kpis.len(), 1);
        assert!(
            ext.kpis[0].available,
            "the KPI validated against the spliced segments"
        );
    }

    /// Annotating a v3 archive with `--event`-style inline events writes the
    /// `{"events":[...]}` payload into the recording's `KEY_EVENTS` metadata,
    /// and — the point of this workstream — it round-trips through the reader's
    /// `file_metadata()`, which is exactly the map both viewer backends read.
    #[test]
    fn annotate_rez_v3_embeds_events_and_they_round_trip_through_the_reader() {
        use crate::recorder::rez::recorder_tests_support::populated_v3_rez;
        use metriken_query::MetricsSource;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rec.rez");
        populated_v3_rez(&path, "baseline", &["cpu_usage"], 6);

        let inline = vec!["timestamp=1000000000,kind=deploy,description=rollout,id=e1".to_string()];
        annotate_rez_v3(&path, &inline_events_annotation(&inline)).unwrap();

        // Stored in the catalog metadata column.
        let db = crate::recorder::rez_sqlite::RezDb::open(&path).unwrap();
        let recordings = db.read_recordings().unwrap();
        assert_eq!(recordings.len(), 1);
        let raw = recordings[0]
            .meta
            .metadata
            .get(crate::parquet_metadata::KEY_EVENTS)
            .expect("events must be embedded in the recording metadata");
        let events: crate::viewer::Events = serde_json::from_str(raw).unwrap();
        assert_eq!(events.events.len(), 1);
        assert_eq!(events.events[0].description, "rollout");
        assert_eq!(events.events[0].kind.as_deref(), Some("deploy"));

        // The reader surfaces it under `events` in `file_metadata()` — the same
        // map `init_file_mode_rez` (server) and `Viewer` (WASM) serialize for
        // the frontend's `seedEventsFromMetadata`.
        let pool = metriken_query::BufferPool::new(64 * 1024 * 1024);
        let readers = crate::rez_reader::RezReader::open_recordings(&path, pool).unwrap();
        let (_labels, reader) = &readers[0];
        let surfaced = reader
            .file_metadata()
            .get(crate::parquet_metadata::KEY_EVENTS)
            .cloned()
            .expect("the reader surfaces events in file_metadata");
        let surfaced: crate::viewer::Events = serde_json::from_str(&surfaced).unwrap();
        assert_eq!(surfaced.events[0].id.as_deref(), Some("e1"));
    }

    /// Events and KPIs can land in one invocation, and each recording keeps
    /// pre-existing metadata (both new keys are additive merges, not a replace).
    #[test]
    fn annotate_rez_v3_embeds_events_and_kpis_together() {
        use crate::recorder::rez::recorder_tests_support::populated_v3_rez;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rec.rez");
        populated_v3_rez(&path, "baseline", &["cpu_usage"], 6);

        let inline = vec!["timestamp=2000000000,description=marker".to_string()];
        let annotation = RezAnnotation {
            ext_json: Some(
                r#"{"service_name":"svc","kpis":[{"role":"overview","title":"Z","query":"0","type":"gauge"}]}"#,
            ),
            events: Some(EventOps {
                add_files: &[],
                inline: &inline,
                clear: false,
            }),
        };
        annotate_rez_v3(&path, &annotation).unwrap();

        let db = crate::recorder::rez_sqlite::RezDb::open(&path).unwrap();
        let md = &db.read_recordings().unwrap()[0].meta.metadata;
        assert!(md.contains_key(KEY_SERVICE_QUERIES), "KPIs embedded");
        assert!(
            md.contains_key(crate::parquet_metadata::KEY_EVENTS),
            "events embedded"
        );
        assert!(
            md.contains_key("sampling_interval_ms"),
            "pre-existing metadata survives an events+KPIs annotate"
        );
    }

    /// `--clear-events` with no adds drops the key entirely rather than storing
    /// an empty `{"events":[]}`, matching the parquet footer path.
    #[test]
    fn annotate_rez_v3_clear_events_removes_the_key() {
        use crate::recorder::rez::recorder_tests_support::populated_v3_rez;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rec.rez");
        populated_v3_rez(&path, "baseline", &["cpu_usage"], 6);

        let inline = vec!["timestamp=1000000000,description=x".to_string()];
        annotate_rez_v3(&path, &inline_events_annotation(&inline)).unwrap();

        let clear = RezAnnotation {
            ext_json: None,
            events: Some(EventOps {
                add_files: &[],
                inline: &[],
                clear: true,
            }),
        };
        annotate_rez_v3(&path, &clear).unwrap();

        let db = crate::recorder::rez_sqlite::RezDb::open(&path).unwrap();
        assert!(
            !db.read_recordings().unwrap()[0]
                .meta
                .metadata
                .contains_key(crate::parquet_metadata::KEY_EVENTS),
            "clearing events drops the key"
        );
    }

    #[test]
    fn systeminfo_annotation_preserves_other_metadata() {
        let tmp = make_minimal_parquet(vec![("source", "rezolus"), ("node", "web01")]);
        annotate_systeminfo(tmp.path(), r#"{"cpu":"x86_64"}"#).unwrap();

        let kv = read_kv(tmp.path());
        assert!(kv.iter().any(|(k, v)| k == "source" && v == "rezolus"));
        assert!(kv.iter().any(|(k, v)| k == "node" && v == "web01"));
    }
}
