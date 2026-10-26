use std::collections::BTreeMap;
use std::path::Path;

/// Describe the dendro archive `args.input` names: its recordings, their
/// labels and metadata, and each table with its cadence; JSON with `--json`.
/// Any other file is refused, one an earlier release wrote with the
/// `upgrade` command.
pub(super) fn run(
    host: &super::Host,
    args: &super::cli::MetadataArgs,
) -> Result<(), Box<dyn std::error::Error>> {
    let input = &args.input;
    if !input.exists() {
        return Err(format!("{}: no such file", input.display()).into());
    }
    ::rez::container::require_dendro(input, host.program)?;
    if args.json {
        let json = v3_json(&read_dendro_summary(input)?, "dendro");
        println!("{}", serde_json::to_string_pretty(&json)?);
    } else {
        print!("{}", describe_rez_string_at(input, host.agent)?);
    }
    Ok(())
}

/// The human-readable summary of the dendro archive at `path`. `agent`
/// names the producer in a recording's agent line when its labels carry
/// no `source`.
pub(crate) fn describe_rez_string_at(
    path: &Path,
    agent: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let recordings = read_dendro_summary(path)?;
    let header = format!("dendro archive — {} recording(s)", recordings.len());
    Ok(describe_v3_string_with(&header, &recordings, agent))
}

/// One v3 table, summarized from the catalog: no segment BLOB is read, so this
/// costs the same on a 7 MB archive and a 605 MB one.
struct V3Table {
    sampler: String,
    /// Every row a reader will see: the sealed segments' rows plus the live WAL
    /// tail, which `RezReader` materializes as the newest segment.
    rows: u64,
    segments: u64,
    /// Rows committed per tick but not yet in a segment — recoverable, and the
    /// property v2 has no equivalent of. Non-zero here is the normal state of a
    /// recording that was killed, and for a quiet table it may be *all* its rows.
    live_wal_rows: u64,
    cadence_ns: Option<u64>,
}

/// One v3 recording, summarized from the catalog.
struct V3Recording {
    /// v3 stores no `dir`: a recording is a row, not a tar directory. The
    /// display name is derived the same way `dir` itself was derived —
    /// `recording_dir_slug(&labels)` — so the viewer's per-capture name
    /// (`rez_reader.rs`, which made the same choice) and this line agree.
    name: String,
    labels: BTreeMap<String, String>,
    metadata: BTreeMap<String, String>,
    complete: bool,
    clock_anchor_wall_ns: u64,
    clock_offsets: Vec<(u64, i64)>,
    tables: Vec<V3Table>,
}

/// Summarize a dendro archive from its catalog, read-only and without reading a segment. A source is a
/// recording; a long table's occupant stream (`<table>/occupants`) is listed
/// as a table of its own, as it is stored.
fn read_dendro_summary(path: &Path) -> Result<Vec<V3Recording>, String> {
    let err = |e: dendro::Error| e.to_string();
    let db = dendro::archive::Archive::open(path).map_err(err)?;
    let ts = |t: Option<i64>| t.and_then(|t| u64::try_from(t).ok());
    let mut out = Vec::new();
    for src in db.read_sources().map_err(err)? {
        let mut tables = Vec::new();
        for stream in db.all_streams(src.id).map_err(err)? {
            let (segments, sealed) = db.segment_span(src.id, &stream).map_err(err)?;
            let live = db.live_wal_span(src.id, &stream).map_err(err)?;
            let rows = sealed.rows + live.rows;
            let first = min_opt(ts(sealed.first_ts), ts(live.first_ts));
            let last = max_opt(ts(sealed.last_ts), ts(live.last_ts));
            tables.push(V3Table {
                sampler: stream,
                rows,
                segments,
                live_wal_rows: live.rows,
                cadence_ns: match (first, last) {
                    (Some(f), Some(l)) if rows >= 2 => Some(l.saturating_sub(f) / (rows - 1)),
                    _ => None,
                },
            });
        }
        let clock_offsets = db
            .read_clock_offsets(src.id)
            .map_err(err)?
            .into_iter()
            .filter_map(|(t, o)| u64::try_from(t).ok().map(|t| (t, o)))
            .collect();
        out.push(V3Recording {
            name: metriken_storage::rez::rez::recording_dir_slug(&src.meta.labels),
            labels: src.meta.labels,
            metadata: src.meta.metadata,
            complete: src.complete,
            clock_anchor_wall_ns: u64::try_from(src.meta.clock_anchor_wall_ns).unwrap_or(0),
            clock_offsets,
            tables,
        });
    }
    Ok(out)
}

fn min_opt(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (x, y) => x.or(y),
    }
}

fn max_opt(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (x, y) => x.or(y),
    }
}

/// The summary of `recordings` under the given first line.
fn describe_v3_string_with(header: &str, recordings: &[V3Recording], agent: &str) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let _ = writeln!(out, "{header}");
    for rec in recordings {
        let labels: Vec<String> = rec.labels.iter().map(|(k, v)| format!("{k}={v}")).collect();
        let _ = writeln!(out, "  recording {} [{}]", rec.name, labels.join(", "));
        // Deliberately NOT v2's wording. v2 recovered a `.partial` up to its
        // last checkpoint, and everything after it was gone; v3 commits every
        // tick to the WAL, so the loss is bounded by one sampling interval for
        // every table — including one that never sealed a segment.
        if !rec.complete {
            let _ = writeln!(
                out,
                "    ! not cleanly finalized — recovered from its write-ahead log; \
                 at most the final tick is missing"
            );
        }
        if let Some(line) = agent_line(&rec.labels, &rec.metadata, agent) {
            let _ = writeln!(out, "    {line}");
        }
        if let Some(line) = clock_line_of(Some(rec.clock_anchor_wall_ns), &rec.clock_offsets) {
            let _ = writeln!(out, "    {line}");
        }
        for t in &rec.tables {
            let cadence = t
                .cadence_ns
                .map(|ns| format!("{:.3}s", ns as f64 / 1e9))
                .unwrap_or_else(|| "—".to_string());
            // Segment counts are worth printing whenever they are not the
            // unremarkable single segment — including 0, which is a table whose
            // every row is still in the WAL.
            let detail = match (t.segments, t.live_wal_rows) {
                (_, 0) if t.segments == 1 => String::new(),
                (n, 0) => format!("  ({n} segments)"),
                (n, live) => format!("  ({n} segments, {live} unsealed in WAL)"),
            };
            let _ = writeln!(
                out,
                "    {:<24} {:>5} rows  ~{}{}",
                t.sampler, t.rows, cadence, detail
            );
        }
    }
    out
}

/// `--json` for a v3 archive: the same facts the text form renders. v2 dumps
/// its manifest verbatim; v3 has no manifest document, so this is its analogue.
///
/// A dendro archive gives the same facts with `container` `"dendro"` and no
/// `.rez` `version`.
fn v3_json(recordings: &[V3Recording], container: &str) -> serde_json::Value {
    serde_json::json!({
        "version": (container == "sqlite").then_some(3),
        "container": container,
        "recordings": recordings.iter().map(|rec| serde_json::json!({
            "name": rec.name,
            "labels": rec.labels,
            "metadata": rec.metadata,
            "complete": rec.complete,
            "clock_anchor_wall_ns": rec.clock_anchor_wall_ns,
            "clock_offsets": rec.clock_offsets,
            "tables": rec.tables.iter().map(|t| serde_json::json!({
                "sampler": t.sampler,
                "rows": t.rows,
                "segments": t.segments,
                "live_wal_rows": t.live_wal_rows,
                "cadence_ns": t.cadence_ns,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    })
}

/// The recording's agent line: which build of its producer wrote it, named
/// by the recording's `source` label, else `agent` (`Host::agent`).
///
/// Printed because "which agent wrote this?" is the first question when two
/// recordings of the same host disagree, and a summary that describes the
/// machine in detail while omitting the thing that read it leaves that
/// unanswerable (issue #1195). Absent for recordings written before the
/// recorder captured it, and for a Prometheus source, which has no agent.
fn agent_line(
    labels: &BTreeMap<String, String>,
    metadata: &BTreeMap<String, String>,
    agent: &str,
) -> Option<String> {
    let version = metadata.get(::dashboard::keys::KEY_VERSION)?;
    if version.is_empty() {
        return None;
    }
    let source = labels
        .get(::dashboard::keys::KEY_SOURCE)
        .map_or(agent, String::as_str);
    Some(format!("agent {source} {version}"))
}

/// The recording's clock line: the wall-clock anchor its row timestamps are
/// offset from, plus the newest drift observation, read from the catalog
/// without decoding a segment.
///
/// The observation reported is the newest by *timestamp*, not the last element:
/// the series is a per-batch maximum, so an age-sealed slow sampler can append
/// a timestamp older than one a fast-sampler batch already contributed.
fn clock_line_of(anchor: Option<u64>, offsets: &[(u64, i64)]) -> Option<String> {
    let anchor = anchor?;
    let when = i64::try_from(anchor)
        .map(|ns| {
            chrono::DateTime::from_timestamp_nanos(ns)
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        })
        .unwrap_or_else(|_| format!("{anchor}ns"));
    let mut line = format!("clock anchor {when}");
    if let Some(&(_, offset)) = offsets.iter().max_by_key(|&&(ts, _)| ts) {
        let _ = std::fmt::Write::write_fmt(
            &mut line,
            format_args!(
                ", latest offset {:+.3}ms ({} observations)",
                offset as f64 / 1e6,
                offsets.len()
            ),
        );
    }
    Some(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A dendro archive's agent line names its `source` label, else the
    /// host's agent, and is absent without a captured version.
    #[test]
    fn the_agent_line_names_the_recordings_source() {
        let version: BTreeMap<String, String> = [(
            ::dashboard::keys::KEY_VERSION.to_string(),
            "5.20.0".to_string(),
        )]
        .into_iter()
        .collect();
        let labels: BTreeMap<String, String> = [("source".to_string(), "web".to_string())]
            .into_iter()
            .collect();
        assert_eq!(
            agent_line(&labels, &version, "rezolus").as_deref(),
            Some("agent web 5.20.0")
        );
        assert_eq!(
            agent_line(&BTreeMap::new(), &version, "rezolus").as_deref(),
            Some("agent rezolus 5.20.0")
        );
        assert_eq!(agent_line(&labels, &BTreeMap::new(), "rezolus"), None);
    }

    /// A dendro archive is described from its catalog like a v3 `.rez`: its
    /// sources as recordings, every stream as a table (a long table's
    /// occupant stream included), with rows and segments.
    #[test]
    fn a_dendro_archive_is_described_from_its_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rec.dendro");
        crate::dendro_copy::fixtures::recorded(&path, 10, true);

        let text = describe_rez_string_at(&path, "rezolus").unwrap();
        assert!(
            text.starts_with("dendro archive — 1 recording(s)"),
            "{text}"
        );
        assert!(
            text.contains("recording rezolus [source=rezolus]"),
            "{text}"
        );
        assert!(text.contains("threads/tasks "), "{text}");
        assert!(text.contains("threads/tasks/occupants"), "{text}");
        assert!(!text.contains("not cleanly finalized"), "{text}");

        let v = v3_json(&read_dendro_summary(&path).unwrap(), "dendro");
        assert_eq!(v["container"], "dendro");
        assert!(v["version"].is_null());
        let tables = v["recordings"][0]["tables"].as_array().unwrap();
        let tasks = tables
            .iter()
            .find(|t| t["sampler"] == "threads/tasks")
            .unwrap();
        // The catalog counts ticks, the WAL rows a segment consumed, not a
        // long table's rows per thread: ten, as a `.rez` table would say.
        assert_eq!(tasks["rows"], 10);
    }

    /// A `.rez` is refused, naming the upgrade.
    #[test]
    fn a_rez_is_refused_with_the_upgrade() {
        let dir = tempfile::tempdir().unwrap();
        let path = ::rez::fixtures::committed_rez("v3.rez", dir.path());
        let err = ::rez::container::require_dendro(&path, "tool").unwrap_err();
        assert!(err.contains("tool recording upgrade"), "{err}");
    }
}
