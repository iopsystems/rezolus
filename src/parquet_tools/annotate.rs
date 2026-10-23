use std::path::{Path, PathBuf};

use ::dashboard::ServiceExtension;
use metriken_query::MetricsSource;

use super::legacy;

pub(super) fn run(host: &super::Host, args: &super::cli::AnnotateArgs) -> i32 {
    match run_inner(host, args) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

fn run_inner(host: &super::Host, args: &super::cli::AnnotateArgs) -> Result<(), String> {
    let path = &args.file;
    let output = args.output.as_deref();

    // Read, parse and check the --queries file before anything is written, so
    // a refusal leaves the file as it was whatever other edits were asked for.
    let queries = args.queries.as_deref().map(load_queries).transpose()?;

    let event_files: Vec<&Path> = args.add_events.iter().map(PathBuf::as_path).collect();
    let inline_events: &[String] = &args.event;
    let clear_events = args.clear_events;
    let events_requested = clear_events || !event_files.is_empty() || !inline_events.is_empty();

    // Flags that edited a parquet file's footer and have no meaning for a
    // .dendro archive are refused before anything is written. --undo and
    // --systeminfo have one (a recording's metadata keys) and are applied.
    let program = host.program;
    let how = host.how_labels_are_set();
    let relabel = |flag: &str| {
        format!(
            "{flag} set a key in a parquet file's footer. A .dendro recording's {} is a \
             label, set when it is recorded{how}, and 6.x does not relabel a recording; to \
             tell recordings apart in a comparison, use `{program} recording combine --ab`",
            flag.trim_start_matches("--")
        )
    };
    let table_flag = host.table_flag.flag();
    for (present, refusal) in [
        (args.source.is_some(), relabel("--source")),
        (args.node.is_some(), relabel("--node")),
        (
            args.filter,
            format!(
                "--filter dropped a parquet file's columns its KPIs do not need; run \
                 `{program} recording filter {} -o slim.dendro` instead (a parquet file \
                 with neither {table_flag} nor --metrics keeps what its KPIs need)",
                super::select::shell_word(&path.display().to_string())
            ),
        ),
    ] {
        if present {
            return Err(refusal);
        }
    }
    let undo = args.undo;
    let systeminfo = args
        .systeminfo
        .as_deref()
        .map(|p| {
            super::convert::load_json(p, super::convert::JsonShape::Any, "systeminfo")
                .map_err(|e| e.to_string())
        })
        .transpose()?;

    if queries.is_none() && !events_requested && !undo && systeminfo.is_none() {
        return Err(
            "annotating needs --queries <service-extension.json>, --undo, \
             --systeminfo <PATH> and/or event flags (--event/--add-events/--clear-events)"
                .to_string(),
        );
    }

    let target = legacy::edit_target(path, output, legacy::command_with_output)
        .map_err(|e| e.to_string())?;

    let events = events_requested.then(|| EventOps {
        add_files: &event_files,
        inline: inline_events,
        clear: clear_events,
    });

    let annotation = RezAnnotation {
        program,
        ext_json: queries.as_deref(),
        events,
        per_recording_events: None,
        per_recording_remove: None,
        undo_kpis: undo,
        systeminfo: systeminfo.as_deref(),
        // Printed once the edit is in place, after the note saying what
        // was converted.
        report: ReportSink::capture(),
    };

    let result = annotate_archive_as(target.path(), target.shown(), &annotation).and_then(|()| {
        let note = target.note();
        target.commit()?;
        Ok(note)
    });
    match result {
        Ok(note) => {
            if let Some(note) = note {
                println!("{note}");
            }
            if let Some(report) = annotation.report.captured() {
                println!("{report}");
            }
            Ok(())
        }
        Err(e) => Err(format!("failed to annotate {}: {e}", path.display())),
    }
}

/// The largest KPI template [`load_queries`] accepts: the limit the viewers
/// put on a source's `dashboard` template, 1 MiB. Every shipped template is
/// under 9 KB.
pub(crate) const MAX_QUERIES_BYTES: usize = ::dashboard::source_templates::MAX_DASHBOARD_BYTES;

/// Read a KPI template file and check it: it must be at most
/// [`MAX_QUERIES_BYTES`], parse as a [`ServiceExtension`] and name only unit
/// systems the viewer knows ([`ServiceExtension::check_unit_systems`]).
/// Returns the file's text as written, which is what goes into a recording's
/// `service_queries`. Shared with `rezolus record --endpoint
/// url,template=FILE`.
pub(crate) fn load_queries(path: &Path) -> Result<String, String> {
    use std::io::Read;
    let file =
        std::fs::File::open(path).map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    let mut content = String::new();
    file.take(MAX_QUERIES_BYTES as u64 + 1)
        .read_to_string(&mut content)
        .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    if content.len() > MAX_QUERIES_BYTES {
        return Err(format!(
            "{} is larger than {MAX_QUERIES_BYTES} bytes, the most a KPI template may be",
            path.display()
        ));
    }
    let ext: ServiceExtension = serde_json::from_str(&content)
        .map_err(|e| format!("invalid service extension JSON in {}: {e}", path.display()))?;
    ext.check_unit_systems()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(content)
}

/// The metadata edits one `annotate` invocation applies to the recordings of
/// a dendro archive. A recording is a read group of the archive's sources
/// (`RezReader::open_recordings`): an edit of a recording is written into
/// each of its sources, so a recording and its copy stay alike.
pub(crate) struct RezAnnotation<'a> {
    /// The program name refusals spell commands with (`Host::program`).
    pub(crate) program: &'a str,
    /// Validated ServiceExtension JSON to embed under `KEY_SERVICE_QUERIES`
    /// (KPIs). Re-probed against each recording's own metrics.
    pub(crate) ext_json: Option<&'a str>,
    /// Event operations to fold into each recording's `KEY_EVENTS` payload.
    pub(crate) events: Option<EventOps<'a>>,
    /// Events that differ per recording, indexed by `open_recordings` order
    /// (`recording check --annotate` writes each recording's own verdicts).
    /// Must have one entry per recording; an empty entry adds nothing to that
    /// recording.
    pub(crate) per_recording_events: Option<Vec<Vec<::dashboard::Event>>>,
    /// Events to drop per recording: `Some(filter)` removes every event
    /// matching it from each of that recording's sources, each from its own
    /// payload (a source's other events stay with it; an emptied payload
    /// drops the key), `None` leaves the recording alone. Must have one entry
    /// per recording. The MCP `remove_events` tool writes through this.
    pub(crate) per_recording_remove: Option<Vec<Option<super::events::RemoveFilter>>>,
    /// Remove the KPIs (`service_queries`) from every recording (`--undo`).
    pub(crate) undo_kpis: bool,
    /// A systeminfo JSON to store in every recording (`--systeminfo`).
    pub(crate) systeminfo: Option<&'a str>,
    /// Where the one-line report goes.
    pub(crate) report: ReportSink,
}

/// Where an annotation's report lines go: captured, for the caller to
/// print once the edit is in place (`annotate`, after a staged edit is
/// moved to its output), to send where it chooses (`check`, whose stdout
/// may be a JSON payload), or to return (the MCP tools).
pub(crate) enum ReportSink {
    Capture(std::cell::RefCell<Vec<String>>),
}

impl ReportSink {
    pub(crate) fn capture() -> Self {
        Self::Capture(std::cell::RefCell::new(Vec::new()))
    }

    /// Every captured line, joined; `None` when nothing was reported.
    pub(crate) fn captured(&self) -> Option<String> {
        match self {
            Self::Capture(cell) => {
                let lines = cell.borrow();
                (!lines.is_empty()).then(|| lines.join("\n"))
            }
        }
    }
}

impl RezAnnotation<'_> {
    fn report(&self, line: String) {
        match &self.report {
            ReportSink::Capture(cell) => cell.borrow_mut().push(line),
        }
    }
}

/// Running totals of what `per_recording_events` did across the recordings.
#[derive(Default)]
struct CheckEventTally {
    counts: super::events::AppendCounts,
    /// Recordings that were handed at least one event.
    touched: usize,
}

/// Event operations for an annotate, applied in the order
/// `clear → add file → add inline`.
pub(crate) struct EventOps<'a> {
    pub(crate) add_files: &'a [&'a Path],
    pub(crate) inline: &'a [String],
    pub(crate) clear: bool,
}

/// Embed KPIs and/or events into the recordings of the dendro archive at
/// `path`. A legacy recording is refused: an edit of one is written to a new
/// `.dendro` (`legacy::edit_target`).
///
/// Unlike `combine` and `filter` this rewrites nothing: KPIs and events are
/// catalog columns, so annotating is an `UPDATE` per source. No segment is
/// read or copied, which is why annotating a 4 GB archive costs the same as
/// annotating a 4 MB one. A dendro archive a writer still holds cannot be
/// opened for writing, and says so.
#[cfg(test)]
pub(crate) fn annotate_archive(
    path: &Path,
    annotation: &RezAnnotation,
) -> Result<(), Box<dyn std::error::Error>> {
    annotate_archive_as(path, path, annotation)
}

/// [`annotate_archive`] of the archive at `path`, reported as `shown`: the
/// output a staged edit is moved to (`legacy::EditTarget`).
pub(crate) fn annotate_archive_as(
    path: &Path,
    shown: &Path,
    annotation: &RezAnnotation,
) -> Result<(), Box<dyn std::error::Error>> {
    use ::dashboard::keys::KEY_EVENTS;
    use ::dashboard::Events;

    let container = legacy::Container::of(path);
    if container != legacy::Container::Dendro {
        return Err(format!(
            "{} is a {}, which 6.x does not change; convert it with `{} recording \
             upgrade {}` and annotate the .dendro",
            path.display(),
            container.name(),
            annotation.program,
            crate::parquet_tools::select::shell_word(&path.display().to_string())
        )
        .into());
    }

    let pool = metriken_storage::BufferPool::new(256 * 1024 * 1024);
    let readers = ::rez::reader::RezReader::open_recordings(path, pool)?;
    // Validated per recording, not once: a KPI's query is only meaningful
    // against the metrics that recording actually holds, and a
    // multi-recording archive can hold different sets. All of it before the
    // archive is opened for writing: a reader opened earlier refuses to read
    // tables once a writer has opened the file.
    let mut kpis: Option<usize> = None;
    let ext_jsons: Vec<Option<String>> = readers
        .iter()
        .map(
            |(_, reader)| -> Result<Option<String>, Box<dyn std::error::Error>> {
                let Some(json) = annotation.ext_json else {
                    return Ok(None);
                };
                let mut ext: ServiceExtension = serde_json::from_str(json)?;
                validate_kpis_source(reader, &mut ext);
                kpis = Some(ext.kpis.len());
                Ok(Some(serde_json::to_string(&ext)?))
            },
        )
        .collect::<Result<_, _>>()?;

    // Each recording's sources, by catalog position.
    let members: Vec<Vec<usize>> = readers
        .iter()
        .map(|(_, r)| r.sources().iter().map(|s| s.catalog_index).collect())
        .collect();
    let recordings = readers.len();
    // A reader that has queried holds the file, and the writer refuses a
    // file in use.
    drop(readers);

    let mut db = dendro::archive::ArchiveMut::open(path)?;
    let sources = db.read_sources()?;
    let covered: usize = members.iter().map(Vec::len).sum();
    if covered != sources.len() || members.iter().flatten().any(|&i| i >= sources.len()) {
        return Err(format!(
            "{} has {} source(s) in its catalog but {} readable — refusing to annotate a \
             partially readable archive",
            path.display(),
            sources.len(),
            covered
        )
        .into());
    }

    if let Some(per) = annotation
        .per_recording_events
        .as_ref()
        .filter(|per| per.len() != recordings)
    {
        return Err(format!(
            "{} has {} recording(s) but events were built for {}; the archive changed \
             between evaluation and annotation",
            path.display(),
            recordings,
            per.len()
        )
        .into());
    }

    if let Some(per) = annotation
        .per_recording_remove
        .as_ref()
        .filter(|per| per.len() != recordings)
    {
        return Err(format!(
            "{} has {} recording(s) but removals were built for {}; the archive \
             changed between the read and the write",
            path.display(),
            recordings,
            per.len()
        )
        .into());
    }

    // Recordings events were removed from, and the event count they hold
    // afterwards (each recording's sources' events as one, as a reader shows
    // them).
    let mut replaced: Option<(usize, usize)> = None;
    // (events remaining after the ops, whether they were cleared first) — the
    // input is identical for every recording, so the last one's counts stand
    // in for the report.
    let mut events_report: Option<(usize, bool)> = None;
    let mut per_recording_report: Option<CheckEventTally> = None;
    for (idx, ext_json) in ext_jsons.iter().enumerate() {
        let mut touched = false;
        let mut kept_per_source: Vec<Events> = Vec::new();
        for (member_n, &position) in members[idx].iter().enumerate() {
            let source = &sources[position];
            let mut metadata = source.meta.metadata.clone();

            if let Some(ext_json) = &ext_json {
                metadata.insert(
                    ::dashboard::keys::KEY_SERVICE_QUERIES.to_string(),
                    ext_json.clone(),
                );
            }
            if annotation.undo_kpis {
                metadata.remove(::dashboard::keys::KEY_SERVICE_QUERIES);
            }
            if let Some(systeminfo) = annotation.systeminfo {
                metadata.insert(
                    ::dashboard::keys::KEY_SYSTEMINFO.to_string(),
                    systeminfo.to_string(),
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
                // An empty payload drops the key rather than storing
                // `{"events":[]}`.
                if events.events.is_empty() {
                    metadata.remove(KEY_EVENTS);
                } else {
                    metadata.insert(KEY_EVENTS.to_string(), serde_json::to_string(&events)?);
                }
                events_report = Some((events.events.len(), ops.clear));
            }

            if let Some(per) = &annotation.per_recording_events {
                let new = per[idx].clone();
                touched |= !new.is_empty();
                let existing = metadata
                    .get(KEY_EVENTS)
                    .map(|s| serde_json::from_str::<Events>(s))
                    .transpose()
                    .map_err(|e| format!("recording has an invalid events payload: {e}"))?;
                let appended = super::events::append_events(existing, new);
                if appended.events.events.is_empty() {
                    metadata.remove(KEY_EVENTS);
                } else {
                    metadata.insert(
                        KEY_EVENTS.to_string(),
                        serde_json::to_string(&appended.events)?,
                    );
                }
                // Counted once per recording, against its sources' events
                // as one (what a reader shows), not one source's.
                if member_n == 0 {
                    let shown = members[idx]
                        .iter()
                        .filter_map(|&p| sources[p].meta.metadata.get(KEY_EVENTS))
                        .map(|s| serde_json::from_str::<Events>(s))
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(|e| format!("recording has an invalid events payload: {e}"))?;
                    let counts =
                        super::events::append_events(Some(Events::union(shown)), per[idx].clone())
                            .counts;
                    let tally = per_recording_report.get_or_insert_with(Default::default);
                    tally.counts.add(counts);
                }
            }

            if let Some(filter) = annotation
                .per_recording_remove
                .as_ref()
                .and_then(|per| per[idx].as_ref())
            {
                let existing = metadata
                    .get(KEY_EVENTS)
                    .map(|s| serde_json::from_str::<Events>(s))
                    .transpose()
                    .map_err(|e| format!("recording has an invalid events payload: {e}"))?;
                if let Some(existing) = existing {
                    let mut kept = Events::new(
                        existing
                            .events
                            .into_iter()
                            .filter(|e| !filter.matches(e))
                            .collect(),
                    );
                    kept.normalize();
                    if kept.events.is_empty() {
                        metadata.remove(KEY_EVENTS);
                    } else {
                        metadata.insert(KEY_EVENTS.to_string(), serde_json::to_string(&kept)?);
                    }
                    kept_per_source.push(kept);
                }
                if member_n == 0 {
                    replaced.get_or_insert((0, 0)).0 += 1;
                }
            }

            db.update_source_metadata(source.id, &metadata)?;
        }
        if touched {
            if let Some(tally) = per_recording_report.as_mut() {
                tally.touched += 1;
            }
        }
        if let Some(r) = replaced.as_mut().filter(|_| {
            annotation
                .per_recording_remove
                .as_ref()
                .is_some_and(|per| per[idx].is_some())
        }) {
            r.1 += Events::union(kept_per_source).events.len();
        }
    }

    if let Some((n, total)) = replaced {
        annotation.report(format!(
            "Annotated {:?}: removed events from {n} of {recordings} recording(s) ({total} event(s) remain)",
            shown
        ));
        return Ok(());
    }

    // Unlike the KPI and event flags, per-recording events are not applied
    // to every recording (`check --recording` writes into the one it
    // evaluated), so when they are the only edit, say how many were touched.
    let only_check_events = annotation.ext_json.is_none() && annotation.events.is_none();
    if let Some(tally) = per_recording_report
        .as_ref()
        .filter(|t| only_check_events && t.counts.nothing_new())
    {
        annotation.report(format!(
            "Annotated {:?}: nothing new; {} check event(s) already present in {} of {recordings} recording(s)",
            shown, tally.counts.unchanged, tally.touched,
        ));
        return Ok(());
    }

    let mut parts: Vec<String> = Vec::new();
    if let Some(k) = kpis {
        parts.push(format!("{k} KPIs"));
    }
    if annotation.systeminfo.is_some() {
        parts.push("systeminfo".to_string());
    }
    if let Some((total, cleared)) = events_report {
        parts.push(if cleared {
            format!("{total} event(s) (replaced)")
        } else {
            format!("{total} event(s)")
        });
    }
    if let Some(tally) = &per_recording_report {
        parts.push(tally.counts.describe());
    }
    let scope = match &per_recording_report {
        Some(tally) if only_check_events => {
            format!("{} of {recordings} recording(s)", tally.touched)
        }
        _ => format!("{recordings} recording(s)"),
    };
    let mut done: Vec<String> = Vec::new();
    if annotation.undo_kpis {
        done.push(format!("removed the KPIs of {scope}"));
    }
    if !parts.is_empty() {
        done.push(format!("embedded {} into {scope}", parts.join(" and ")));
    }
    annotation.report(format!("Annotated {:?}: {}", shown, done.join("; ")));
    Ok(())
}

/// Set each KPI's `available` flag by probing a `MetricsSource`.
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

fn query_result_is_empty(result: &metriken_query::QueryResult) -> bool {
    use metriken_query::QueryResult;
    match result {
        QueryResult::Vector { result } => result.is_empty(),
        QueryResult::Matrix { result } => result.is_empty(),
        QueryResult::Scalar { .. } => false,
        QueryResult::HistogramHeatmap { result } => result.data.is_empty(),
    }
}

/// Each recording's metadata, as the archive's first source of it holds it.
#[cfg(test)]
pub(crate) fn source_metadata(path: &Path) -> Vec<std::collections::BTreeMap<String, String>> {
    dendro::archive::Archive::open(path)
        .unwrap()
        .read_sources()
        .unwrap()
        .into_iter()
        .map(|s| s.meta.metadata)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::dashboard::keys::{KEY_EVENTS, KEY_SERVICE_QUERIES};

    /// A template over [`super::MAX_QUERIES_BYTES`] is refused, however
    /// valid, and one at the limit is read.
    #[test]
    fn load_queries_refuses_a_template_over_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let body = r#"{"service_name":"s","kpis":[]}"#;
        let write = |name: &str, len: usize| {
            let path = dir.path().join(name);
            // Trailing whitespace keeps the JSON valid at any length.
            std::fs::write(&path, format!("{body}{}", " ".repeat(len - body.len()))).unwrap();
            path
        };
        let at = write("at.json", super::MAX_QUERIES_BYTES);
        assert_eq!(
            super::load_queries(&at).unwrap().len(),
            super::MAX_QUERIES_BYTES
        );
        let over = write("over.json", super::MAX_QUERIES_BYTES + 1);
        let err = super::load_queries(&over).unwrap_err();
        assert!(err.contains("larger than 1048576 bytes"), "{err}");
        assert!(err.contains("over.json"), "{err}");
    }

    /// A KPIs-only annotation.
    fn kpi_annotation(ext_json: &str) -> RezAnnotation<'_> {
        RezAnnotation {
            program: "rezolus",
            ext_json: Some(ext_json),
            events: None,
            per_recording_events: None,
            per_recording_remove: None,
            undo_kpis: false,
            systeminfo: None,
            report: ReportSink::capture(),
        }
    }

    /// An events-only annotation adding the given inline events.
    fn inline_events_annotation(inline: &[String]) -> RezAnnotation<'_> {
        RezAnnotation {
            program: "rezolus",
            ext_json: None,
            events: Some(EventOps {
                add_files: &[],
                inline,
                clear: false,
            }),
            per_recording_events: None,
            per_recording_remove: None,
            undo_kpis: false,
            systeminfo: None,
            report: ReportSink::capture(),
        }
    }

    /// A finalized one-source dendro archive: three rows of `cpu_cycles`,
    /// with `sampling_interval_ms` in its metadata.
    fn one_recording(path: &Path) {
        let rows = (0..3u64)
            .map(|i| {
                let ts = 1_000_000_000 * (i + 1);
                let w = Some(::rez::window::Window::new(ts - 50_000_000, ts));
                (
                    ::rez::fixtures::snap_counters(
                        ts,
                        vec![::rez::fixtures::counter("cpu_cycles", "cpu_usage", i, w)],
                    ),
                    ts,
                )
            })
            .collect();
        let source = ::rez::fixtures::Source {
            labels: ::rez::fixtures::map(&[("source", "rezolus")]),
            metadata: ::rez::fixtures::map(&[("sampling_interval_ms", "1000")]),
            rows,
        };
        ::rez::fixtures::write_dendro(path, &[source], Some(2), ::rez::fixtures::Finish::Finalized);
    }

    /// Annotating writes the KPI set into each recording's metadata, in
    /// place, without disturbing the segments, and the KPI is validated
    /// against the recording's rows.
    #[test]
    fn annotate_embeds_service_queries_into_recordings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rec.dendro");
        one_recording(&path);
        let segments = || ::rez::fixtures::segment_counts(&path);
        let before = segments();
        assert!(before[0]["cpu_usage"] > 1, "the input must be segmented");

        let ext_json = r#"{"service_name":"test","kpis":[{"role":"overview","title":"Cycles","query":"rate(cpu_cycles[2s])","type":"counter"}]}"#;
        annotate_archive(&path, &kpi_annotation(ext_json)).unwrap();

        let metadata = source_metadata(&path);
        assert_eq!(metadata.len(), 1);
        let ext: ServiceExtension = serde_json::from_str(
            metadata[0]
                .get(KEY_SERVICE_QUERIES)
                .expect("service queries must be embedded in the recording metadata"),
        )
        .unwrap();
        assert_eq!(ext.service_name, "test");
        assert!(ext.kpis[0].available, "the KPI validated against the rows");
        assert_eq!(
            metadata[0].get("sampling_interval_ms").map(String::as_str),
            Some("1000"),
            "annotating merges into the recording's metadata"
        );
        assert_eq!(segments(), before, "no segment is rewritten");
    }

    /// Inline events are written into `KEY_EVENTS` and round-trip through
    /// the reader's `file_metadata()`, the map both viewer backends read.
    #[test]
    fn annotate_embeds_events_and_they_round_trip_through_the_reader() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rec.dendro");
        one_recording(&path);

        let inline = vec!["timestamp=1000000000,kind=deploy,description=rollout,id=e1".to_string()];
        annotate_archive(&path, &inline_events_annotation(&inline)).unwrap();

        let events: ::dashboard::Events =
            serde_json::from_str(&source_metadata(&path)[0][KEY_EVENTS]).unwrap();
        assert_eq!(events.events.len(), 1);
        assert_eq!(events.events[0].description, "rollout");
        assert_eq!(events.events[0].kind.as_deref(), Some("deploy"));

        let pool = metriken_storage::BufferPool::new(64 * 1024 * 1024);
        let readers = ::rez::reader::RezReader::open_recordings(&path, pool).unwrap();
        let surfaced: ::dashboard::Events =
            serde_json::from_str(&readers[0].1.file_metadata()[KEY_EVENTS]).unwrap();
        assert_eq!(surfaced.events[0].id.as_deref(), Some("e1"));
    }

    /// Events and KPIs land in one invocation, metadata already there is
    /// kept, and clearing drops the key.
    #[test]
    fn annotate_embeds_events_and_kpis_together_and_clears() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rec.dendro");
        crate::dendro_copy::fixtures::recorded(&path, 6, true);
        let before = source_metadata(&path)[0].clone();

        let inline = vec!["timestamp=1700000002000000000,description=marker".to_string()];
        let annotation = RezAnnotation {
            program: "rezolus",
            ext_json: Some(
                r#"{"service_name":"svc","kpis":[{"role":"overview","title":"Z","query":"0","type":"gauge"}]}"#,
            ),
            events: Some(EventOps {
                add_files: &[],
                inline: &inline,
                clear: false,
            }),
            per_recording_events: None,
            per_recording_remove: None,
            undo_kpis: false,
            systeminfo: None,
            report: ReportSink::capture(),
        };
        annotate_archive(&path, &annotation).unwrap();
        let md = source_metadata(&path)[0].clone();
        assert!(md.contains_key(KEY_SERVICE_QUERIES), "KPIs embedded");
        let events: ::dashboard::Events = serde_json::from_str(&md[KEY_EVENTS]).unwrap();
        assert_eq!(events.events.len(), 1);
        for (k, v) in &before {
            assert_eq!(md.get(k), Some(v), "{k} survives");
        }

        let clear = RezAnnotation {
            program: "rezolus",
            ext_json: None,
            events: Some(EventOps {
                add_files: &[],
                inline: &[],
                clear: true,
            }),
            per_recording_events: None,
            per_recording_remove: None,
            undo_kpis: false,
            systeminfo: None,
            report: ReportSink::capture(),
        };
        annotate_archive(&path, &clear).unwrap();
        assert!(!source_metadata(&path)[0].contains_key(KEY_EVENTS));
    }

    /// A recording and its copy are one recording: an event added to it is
    /// written into both sources, and reported once.
    #[test]
    fn annotate_writes_a_recording_into_each_of_its_sources() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.dendro");
        crate::dendro_copy::fixtures::recorded(&a, 6, true);
        let copy = dir.path().join("copy.dendro");
        crate::parquet_tools::snapshot_rez(&a, &copy).unwrap();
        let both = dir.path().join("both.dendro");
        crate::parquet_tools::combine::run_combine_for_test(&[a, copy], &both).unwrap();
        assert_eq!(source_metadata(&both).len(), 2, "two sources");

        let inline = vec!["timestamp=1700000002000000000,description=marker".to_string()];
        let annotation = RezAnnotation {
            program: "rezolus",
            report: ReportSink::capture(),
            ..inline_events_annotation(&inline)
        };
        annotate_archive(&both, &annotation).unwrap();
        for md in source_metadata(&both) {
            let events: ::dashboard::Events = serde_json::from_str(&md[KEY_EVENTS]).unwrap();
            assert_eq!(events.events.len(), 1);
        }
        let report = annotation.report.captured().unwrap();
        assert!(report.contains("into 1 recording(s)"), "{report}");
    }

    /// A legacy recording is not changed; `annotate_archive` refuses it,
    /// pointing at `recording upgrade`.
    #[test]
    fn annotate_archive_refuses_a_legacy_recording() {
        let dir = tempfile::tempdir().unwrap();
        let path = ::rez::fixtures::committed_rez("v3.rez", dir.path());
        let before = std::fs::read(&path).unwrap();
        let inline = vec!["timestamp=1000000000,description=x".to_string()];
        let err = annotate_archive(&path, &inline_events_annotation(&inline))
            .unwrap_err()
            .to_string();
        assert!(err.contains("recording upgrade"), "{err}");
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}
