//! Event-annotation helpers for `rezolus recording annotate`.
//!
//! Events are stored as a JSON blob in each recording's metadata under
//! [`KEY_EVENTS`](::dashboard::keys::KEY_EVENTS) (a parquet file
//! written before 6.0 holds them in its footer). On-disk shape:
//! `{"events":[ {"timestamp":<ns>, "description":..., ...} ]}`. Every
//! event is self-describing — it carries its own optional
//! `source`/`node`/`instance` scope rather than inheriting from
//! file-level metadata — so combined files don't need a `per_source_metadata`
//! indirection for them.
//!
//! Input paths:
//! - `--add-events FILE` (also `-` for stdin): JSON or JSONL. JSON form is
//!   either `{"events":[...]}` or a bare array.
//! - `--event key=value,...`: inline shorthand, repeatable. `description`
//!   values may be quoted to embed commas.
//! - `--clear-events`: wipe existing events. Combined with `--add-events`
//!   yields "replace": clear is applied before add.
//!
//! Both file and CLI inputs accept `timestamp` as either nanoseconds (int)
//! or an RFC3339 string. `duration_ns` accepts nanoseconds (int) or a
//! humantime string (e.g. `30s`, `1m30s`).

use chrono::DateTime;
use std::collections::BTreeMap;
use std::path::Path;

use ::dashboard::keys::KEY_EVENTS;
use ::dashboard::{Event, Events};

/// Fold event operations over an existing payload, in the order
/// `clear → add file → add inline`, without any file I/O: the transform
/// `parquet_tools::annotate` applies to each recording.
///
/// `existing` is the payload already attached (ignored when `clear`). Returns
/// the normalized result plus `(added, dropped_by_id)` counts for reporting.
pub(super) fn apply_event_ops(
    existing: Option<Events>,
    add_files: &[&Path],
    inline: &[String],
    clear: bool,
) -> Result<(Events, usize, usize), Box<dyn std::error::Error>> {
    let mut events = if clear {
        Events::default()
    } else {
        existing.unwrap_or_default()
    };

    let mut added = 0usize;
    for file in add_files {
        let new_events = read_input_file(file)?;
        added += new_events.len();
        events.events.extend(new_events);
    }
    for s in inline {
        let event = parse_inline_event(s)?;
        added += 1;
        events.events.push(event);
    }

    let before = events.events.len();
    events.normalize();
    let dropped = before - events.events.len();

    Ok((events, added, dropped))
}

/// What [`append_events`] did with a batch, for the report line.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub(crate) struct AppendCounts {
    /// Events whose id was not present (or that carry no id).
    pub new: usize,
    /// `kind=check` events whose id was present with different content;
    /// the incoming copy replaced the stored one.
    pub updated: usize,
    /// Ids already present and kept as stored: identical content, or
    /// either side not a `kind=check` event (the replace rule applies only
    /// when both the stored and the incoming event are check events).
    pub unchanged: usize,
    /// Ids repeated within the batch itself; only the first is kept.
    pub duplicates: usize,
}

impl AppendCounts {
    pub fn add(&mut self, other: AppendCounts) {
        self.new += other.new;
        self.updated += other.updated;
        self.unchanged += other.unchanged;
        self.duplicates += other.duplicates;
    }

    /// Nothing in the payload changed.
    pub fn nothing_new(&self) -> bool {
        self.new == 0 && self.updated == 0
    }

    /// `2 new check event(s), 1 updated, 3 unchanged, 1 duplicate in this run`,
    /// omitting the zero terms.
    pub fn describe(&self) -> String {
        let mut s = format!("{} new check event(s)", self.new);
        if self.updated > 0 {
            s.push_str(&format!(", {} updated", self.updated));
        }
        if self.unchanged > 0 {
            s.push_str(&format!(", {} unchanged", self.unchanged));
        }
        if self.duplicates > 0 {
            s.push_str(&format!(", {} duplicate in this run", self.duplicates));
        }
        s
    }
}

pub(crate) struct Appended {
    pub events: Events,
    pub counts: AppendCounts,
}

/// Add already-built events to a payload and normalize. The second half of
/// [`apply_event_ops`] for a caller that has `Event` values rather than
/// files or `--event` strings (`recording check --annotate`).
///
/// Ids decide what happens: an id repeated inside `new` keeps its first
/// occurrence; an id already in `existing` keeps the stored copy, except
/// that a `kind=check` event replaces a stored one with the same id, so a
/// violation window that grew since the last run is rewritten rather than
/// left at its old `duration_ns`. An id-less event is always added.
pub(crate) fn append_events(existing: Option<Events>, new: Vec<Event>) -> Appended {
    let mut events = existing.unwrap_or_default();
    let mut counts = AppendCounts::default();
    let mut seen_in_batch = std::collections::HashSet::new();
    for event in new {
        let Some(id) = event.id.clone().filter(|id| !id.is_empty()) else {
            counts.new += 1;
            events.events.push(event);
            continue;
        };
        if !seen_in_batch.insert(id.clone()) {
            counts.duplicates += 1;
            continue;
        }
        match events
            .events
            .iter()
            .position(|e| e.id.as_deref() == Some(id.as_str()))
        {
            None => {
                counts.new += 1;
                events.events.push(event);
            }
            Some(i) if events.events[i] == event => counts.unchanged += 1,
            // Both sides must be check events: a stored event of another
            // kind that happens to carry a `check:` id is not ours to
            // overwrite or re-kind.
            Some(i)
                if event.kind.as_deref() == Some("check")
                    && events.events[i].kind.as_deref() == Some("check") =>
            {
                events.events[i] = event;
                counts.updated += 1;
            }
            Some(_) => counts.unchanged += 1,
        }
    }
    events.normalize();
    Appended { events, counts }
}

/// What a selector-scoped write did, for the caller's own report.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct WriteReport {
    /// The recording written, as its label set.
    pub recording: Option<BTreeMap<String, String>>,
    pub counts: AppendCounts,
    /// Events the recording holds after the write.
    pub total: usize,
    /// The annotation writer's own report, when there was one, after the
    /// line saying what was converted when the input was a legacy recording.
    pub report: Option<String>,
}

/// Append `new` to the recording `selector` names in `path`, a dendro
/// archive; with `output`, in a new `.dendro` there (a legacy `.rez` or
/// parquet input needs one: it is converted there and the copy written). A
/// multi-recording archive with no selector is refused with the listing the
/// read tools give, never written to every recording: that is `recording
/// annotate`'s behavior, and an agent adding a mark it saw in one arm must
/// not stamp the other arms with it.
pub(crate) fn add_events_selected(
    path: &Path,
    output: Option<&Path>,
    selector: &crate::parquet_tools::select::RecordingSelector,
    new: Vec<Event>,
    program: &str,
) -> Result<WriteReport, Box<dyn std::error::Error>> {
    // Resolved on the input first, so a selector that names nothing is
    // refused before anything is written.
    one_target(path, selector, program)?;
    let edit = super::legacy::edit_target(path, output, output_advice)?;
    let target_path = edit.path().to_path_buf();
    let (target, total) = one_target(&target_path, selector, program)?;
    // The same append the writer applies, run here first so the counts
    // and the resulting total can be reported; the writer repeats it
    // against what is stored, which is what was just read.
    let preview = append_events(stored_events(target.reader.as_ref())?, new.clone());
    let mut per: Vec<Vec<Event>> = vec![Vec::new(); total];
    per[target.index] = new;
    let annotation = super::annotate::RezAnnotation {
        program,
        ext_json: None,
        events: None,
        per_recording_events: Some(per),
        per_recording_remove: None,
        undo_kpis: false,
        systeminfo: None,
        report: super::annotate::ReportSink::capture(),
    };
    drop(target.reader);
    super::annotate::annotate_archive_as(&target_path, edit.shown(), &annotation)?;
    let note = edit.note();
    edit.commit()?;
    Ok(WriteReport {
        recording: target.labels,
        counts: preview.counts,
        total: preview.events.events.len(),
        report: joined(note, annotation.report.captured()),
    })
}

/// How to write an MCP edit of the legacy recording at `path`: name a new
/// archive in the tool's `output` argument.
fn output_advice(suggested: &Path) -> String {
    format!(
        "pass \"output\": {:?} (a new .dendro the edit is written to)",
        suggested.display().to_string()
    )
}

/// `first` and `second`, each on its own line, when there are any.
fn joined(first: Option<String>, second: Option<String>) -> Option<String> {
    match (first, second) {
        (Some(a), Some(b)) => Some(format!("{a}\n{b}")),
        (a, b) => a.or(b),
    }
}

/// Which events `remove_events_selected` drops. Every given field must
/// match; an event matches `ids` when its id is one of them.
#[derive(Debug, Default, Clone)]
pub(crate) struct RemoveFilter {
    pub ids: Vec<String>,
    pub kind: Option<String>,
    pub source: Option<String>,
}

impl RemoveFilter {
    pub(crate) fn is_empty(&self) -> bool {
        self.ids.is_empty() && self.kind.is_none() && self.source.is_none()
    }

    pub(crate) fn matches(&self, e: &Event) -> bool {
        (self.ids.is_empty()
            || e.id
                .as_deref()
                .is_some_and(|id| self.ids.iter().any(|w| w == id)))
            && self
                .kind
                .as_deref()
                .is_none_or(|k| e.kind.as_deref() == Some(k))
            && self
                .source
                .as_deref()
                .is_none_or(|s| e.source.as_deref() == Some(s))
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct RemoveReport {
    pub recording: Option<BTreeMap<String, String>>,
    pub removed: usize,
    /// Events the recording holds after the write.
    pub total: usize,
    /// The annotation writer's own report, when there was one.
    pub report: Option<String>,
}

/// Drop every event matching `filter` from the recording `selector` names,
/// in `path` or, with `output`, in a new `.dendro` there (as
/// [`add_events_selected`]). An empty filter is refused: "remove everything"
/// is `annotate --clear-events`, typed by a person.
pub(crate) fn remove_events_selected(
    path: &Path,
    output: Option<&Path>,
    selector: &crate::parquet_tools::select::RecordingSelector,
    filter: &RemoveFilter,
    program: &str,
) -> Result<RemoveReport, Box<dyn std::error::Error>> {
    if filter.is_empty() {
        return Err("remove_events needs at least one of ids, kind or source; \
                    clearing every event is `recording annotate --clear-events`"
            .into());
    }
    one_target(path, selector, program)?;
    let edit = super::legacy::edit_target(path, output, output_advice)?;
    let target_path = edit.path().to_path_buf();
    let (target, total) = one_target(&target_path, selector, program)?;
    // The recording's events as a reader shows them: every source's, as one
    // (`rez::reader`'s `group_metadata`). Each source is then filtered from
    // its own payload by `annotate_archive`, so no source is given another's
    // events. `removed` is counted after normalizing so that `removed +
    // total` is what was there before.
    let existing = stored_events(target.reader.as_ref())?.unwrap_or_default();
    let mut shown = existing.clone();
    shown.normalize();
    let before = shown.events.len();
    let remaining = shown.events.iter().filter(|e| !filter.matches(e)).count();
    let removed = before - remaining;
    let mut per: Vec<Option<RemoveFilter>> = vec![None; total];
    per[target.index] = Some(filter.clone());
    let annotation = super::annotate::RezAnnotation {
        program,
        ext_json: None,
        events: None,
        per_recording_events: None,
        per_recording_remove: Some(per),
        undo_kpis: false,
        systeminfo: None,
        report: super::annotate::ReportSink::capture(),
    };
    drop(target.reader);
    super::annotate::annotate_archive_as(&target_path, edit.shown(), &annotation)?;
    let note = edit.note();
    edit.commit()?;
    Ok(RemoveReport {
        recording: target.labels,
        removed,
        total: remaining,
        report: joined(note, annotation.report.captured()),
    })
}

/// The one recording a selector names, through the check runner's open
/// (`RezReader::open_recordings`, or a parquet file's one recording, refusing
/// a selector on it). Unlike `check`, an empty selector over a
/// multi-recording archive is an error here, with the same listing the read
/// tools give. Returns the recording and how many the archive holds.
fn one_target(
    path: &Path,
    selector: &crate::parquet_tools::select::RecordingSelector,
    program: &str,
) -> Result<(super::check::Target, usize), Box<dyn std::error::Error>> {
    let (_format, mut targets, total) = super::check::open_targets(path, selector, program)?;
    if targets.len() != 1 {
        let all: Vec<BTreeMap<String, String>> =
            targets.iter().filter_map(|t| t.labels.clone()).collect();
        return Err(format!(
            "{} holds {} recordings; name one with a recording selector:\n{}",
            path.display(),
            targets.len(),
            crate::parquet_tools::select::describe_candidates(
                &all,
                &[],
                crate::parquet_tools::select::SelectorSyntax::Json,
                program,
            )
        )
        .into());
    }
    Ok((targets.remove(0), total))
}

/// A recording's stored events, as its reader reports them.
fn stored_events(
    reader: &dyn metriken_query::MetricsSource,
) -> Result<Option<Events>, Box<dyn std::error::Error>> {
    reader
        .file_metadata()
        .get(KEY_EVENTS)
        .map(|s| serde_json::from_str::<Events>(s))
        .transpose()
        .map_err(|e| format!("recording has an invalid events payload: {e}").into())
}

/// Read events from `path`, or from stdin if `path == "-"`. Accepts:
/// - A JSON object `{"events":[...]}`
/// - A bare JSON array `[...]`
/// - A single JSON object `{...}` (one event)
/// - JSONL (one event per line) when `path` ends in `.jsonl` or when the
///   input doesn't start with `{` / `[` after trimming.
pub(super) fn read_input_file(path: &Path) -> Result<Vec<Event>, Box<dyn std::error::Error>> {
    let content = if path.as_os_str() == "-" {
        let mut buf = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)
            .map_err(|e| format!("failed to read events from stdin: {e}"))?;
        buf
    } else {
        std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read events from {path:?}: {e}"))?
    };

    let is_jsonl = path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.eq_ignore_ascii_case("jsonl") || s.eq_ignore_ascii_case("ndjson"))
        .unwrap_or(false);

    parse_events_payload(&content, is_jsonl)
        .map_err(|e| format!("failed to parse events from {path:?}: {e}").into())
}

fn parse_events_payload(content: &str, force_jsonl: bool) -> Result<Vec<Event>, String> {
    let trimmed = content.trim_start();
    if force_jsonl || !(trimmed.starts_with('{') || trimmed.starts_with('[')) {
        return parse_jsonl(content);
    }

    if trimmed.starts_with('{') && looks_like_jsonl(content) {
        return parse_jsonl(content);
    }

    let mut value: serde_json::Value =
        serde_json::from_str(content).map_err(|e| format!("invalid JSON: {e}"))?;

    let array = match &mut value {
        serde_json::Value::Object(obj) => obj
            .remove("events")
            .ok_or_else(|| "expected an `events` array in object form".to_string())?,
        serde_json::Value::Array(_) => value,
        _ => return Err("expected an object, array, or JSONL".into()),
    };

    let serde_json::Value::Array(items) = array else {
        return Err("`events` must be an array".into());
    };

    items
        .into_iter()
        .enumerate()
        .map(|(i, v)| value_to_event(v).map_err(|e| format!("event #{i}: {e}")))
        .collect()
}

fn parse_jsonl(content: &str) -> Result<Vec<Event>, String> {
    let mut out = Vec::new();
    for (i, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let v: serde_json::Value =
            serde_json::from_str(line).map_err(|e| format!("line {}: invalid JSON: {e}", i + 1))?;
        let event = value_to_event(v).map_err(|e| format!("line {}: {e}", i + 1))?;
        out.push(event);
    }
    Ok(out)
}

/// Heuristic: JSONL when we see `}` followed by a newline followed by `{`
/// at the top level. We don't do brace-counting; misidentified inputs
/// surface as JSON errors with line numbers, which is fine for a CLI tool.
fn looks_like_jsonl(content: &str) -> bool {
    let mut lines = content.lines().filter(|l| !l.trim().is_empty());
    let Some(first) = lines.next() else {
        return false;
    };
    let trimmed_first = first.trim_start();
    if !trimmed_first.starts_with('{') {
        return false;
    }
    lines
        .next()
        .map(|l| l.trim_start().starts_with('{'))
        .unwrap_or(false)
}

/// Convert a `serde_json::Value` representing a single event into our
/// canonical [`Event`], applying timestamp/duration normalization for the
/// human-friendly input forms (RFC3339 timestamps, humantime durations).
fn value_to_event(mut v: serde_json::Value) -> Result<Event, String> {
    let serde_json::Value::Object(map) = &mut v else {
        return Err("event must be a JSON object".into());
    };

    if let Some(ts) = map.get_mut("timestamp") {
        normalize_timestamp(ts)?;
    }
    if let Some(d) = map.get_mut("duration_ns") {
        normalize_duration(d)?;
    }
    if let Some(d) = map.remove("duration") {
        if !map.contains_key("duration_ns") {
            let mut d = d;
            normalize_duration(&mut d)?;
            map.insert("duration_ns".into(), d);
        }
    }

    serde_json::from_value::<Event>(v).map_err(|e| e.to_string())
}

fn normalize_timestamp(v: &mut serde_json::Value) -> Result<(), String> {
    match v {
        serde_json::Value::Number(_) => Ok(()),
        serde_json::Value::String(s) => {
            let ns = parse_timestamp_str(s)?;
            *v = serde_json::Value::Number(ns.into());
            Ok(())
        }
        _ => Err("timestamp must be a number (ns) or an RFC3339 string".into()),
    }
}

fn normalize_duration(v: &mut serde_json::Value) -> Result<(), String> {
    match v {
        serde_json::Value::Number(_) => Ok(()),
        serde_json::Value::String(s) => {
            let ns = parse_duration_str(s)?;
            *v = serde_json::Value::Number(ns.into());
            Ok(())
        }
        serde_json::Value::Null => Ok(()),
        _ => Err("duration must be a number (ns) or a humantime string like \"30s\"".into()),
    }
}

/// Parse a timestamp string. Accepts RFC3339 (with or without timezone;
/// naive strings are treated as UTC), the short `YYYY-MM-DDTHH:MMZ`
/// "seconds-omitted" form, and bare integer strings (interpreted as
/// nanoseconds).
pub(crate) fn parse_timestamp_str(s: &str) -> Result<u64, String> {
    if let Ok(n) = s.parse::<u64>() {
        return Ok(n);
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        let ns = dt
            .timestamp_nanos_opt()
            .ok_or_else(|| format!("timestamp {s:?} is out of range"))?;
        if ns < 0 {
            return Err(format!("timestamp {s:?} predates the Unix epoch"));
        }
        return Ok(ns as u64);
    }
    if let Some(patched) = try_patch_seconds(s) {
        if let Ok(dt) = DateTime::parse_from_rfc3339(&patched) {
            let ns = dt
                .timestamp_nanos_opt()
                .ok_or_else(|| format!("timestamp {s:?} is out of range"))?;
            return Ok(ns as u64);
        }
    }
    for fmt in &["%Y-%m-%dT%H:%M:%S", "%Y-%m-%dT%H:%M"] {
        if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(s, fmt) {
            let ns = dt
                .and_utc()
                .timestamp_nanos_opt()
                .ok_or_else(|| format!("timestamp {s:?} is out of range"))?;
            return Ok(ns as u64);
        }
    }
    Err(format!("could not parse {s:?} as RFC3339 or ns integer"))
}

/// If `s` is an RFC3339-ish string whose time field omits seconds
/// (e.g. `2026-05-12T15:23Z`), return a version with `:00` inserted before
/// the timezone designator. Returns `None` otherwise — including when the
/// time already has seconds.
fn try_patch_seconds(s: &str) -> Option<String> {
    let t_pos = s.find('T')?;
    let after_t = &s[t_pos + 1..];
    let tz_offset_in_after_t = after_t.find(['Z', '+', '-'])?;
    let time_part = &after_t[..tz_offset_in_after_t];
    if time_part.matches(':').count() != 1 {
        return None;
    }
    let mut out = String::with_capacity(s.len() + 3);
    out.push_str(&s[..t_pos + 1 + tz_offset_in_after_t]);
    out.push_str(":00");
    out.push_str(&after_t[tz_offset_in_after_t..]);
    Some(out)
}

/// Parse a duration string. Accepts bare integer strings (ns) and
/// humantime strings like `30s`, `1m30s`, `2h`.
pub(crate) fn parse_duration_str(s: &str) -> Result<u64, String> {
    if let Ok(n) = s.parse::<u64>() {
        return Ok(n);
    }
    let d = humantime::parse_duration(s).map_err(|e| format!("invalid duration {s:?}: {e}"))?;
    u64::try_from(d.as_nanos()).map_err(|_| format!("duration {s:?} overflows u64 ns"))
}

/// Parse a single inline `--event key=value,key=value` shorthand.
///
/// Quoting: a value may be wrapped in double quotes (`description="foo, bar"`)
/// to embed commas. Backslash escapes are supported inside quoted strings.
/// Unknown keys are rejected so typos surface immediately.
fn parse_inline_event(spec: &str) -> Result<Event, String> {
    let pairs = split_kv_pairs(spec)?;
    let mut map = serde_json::Map::new();
    let mut labels = BTreeMap::<String, String>::new();

    for (key, value) in pairs {
        match key.as_str() {
            "time" | "timestamp" => {
                map.insert("timestamp".into(), serde_json::Value::String(value));
            }
            "description" | "desc" => {
                map.insert("description".into(), serde_json::Value::String(value));
            }
            "kind" => {
                map.insert("kind".into(), serde_json::Value::String(value));
            }
            "details" => {
                map.insert("details".into(), serde_json::Value::String(value));
            }
            "source" => {
                map.insert("source".into(), serde_json::Value::String(value));
            }
            "node" => {
                map.insert("node".into(), serde_json::Value::String(value));
            }
            "instance" => {
                map.insert("instance".into(), serde_json::Value::String(value));
            }
            "id" => {
                map.insert("id".into(), serde_json::Value::String(value));
            }
            "duration" | "duration_ns" => {
                map.insert("duration_ns".into(), serde_json::Value::String(value));
            }
            other => {
                if let Some(label) = other.strip_prefix("label.") {
                    labels.insert(label.to_string(), value);
                } else {
                    return Err(format!("unknown event key {other:?}"));
                }
            }
        }
    }

    if !labels.is_empty() {
        map.insert(
            "labels".into(),
            serde_json::to_value(&labels).map_err(|e| e.to_string())?,
        );
    }

    if !map.contains_key("timestamp") {
        return Err("event is missing required `time=`/`timestamp=`".into());
    }
    if !map.contains_key("description") {
        return Err("event is missing required `description=`".into());
    }

    value_to_event(serde_json::Value::Object(map))
}

/// Split `--event` payload `key=value,key=value` honoring double-quoted
/// values (so values may contain commas and `=`). Returns key/value pairs
/// in source order.
fn split_kv_pairs(spec: &str) -> Result<Vec<(String, String)>, String> {
    let mut pairs = Vec::new();
    let mut chars = spec.chars().peekable();

    loop {
        while matches!(chars.peek(), Some(c) if c.is_whitespace() || *c == ',') {
            chars.next();
        }
        if chars.peek().is_none() {
            break;
        }

        let mut key = String::new();
        while let Some(&c) = chars.peek() {
            if c == '=' || c == ',' {
                break;
            }
            key.push(c);
            chars.next();
        }
        let key = key.trim().to_string();
        if key.is_empty() {
            return Err("empty event key".into());
        }
        if chars.next() != Some('=') {
            return Err(format!("event key {key:?} is missing '='"));
        }

        let mut value = String::new();
        if chars.peek() == Some(&'"') {
            chars.next();
            loop {
                match chars.next() {
                    Some('\\') => match chars.next() {
                        Some(c) => value.push(c),
                        None => return Err("trailing backslash in event value".into()),
                    },
                    Some('"') => break,
                    Some(c) => value.push(c),
                    None => return Err(format!("unterminated quoted value for key {key:?}")),
                }
            }
        } else {
            while let Some(&c) = chars.peek() {
                if c == ',' {
                    break;
                }
                value.push(c);
                chars.next();
            }
        }

        pairs.push((key, value.trim().to_string()));
    }

    Ok(pairs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_object_payload() {
        let json = r#"{"events":[{"timestamp":1,"description":"a"}]}"#;
        let events = parse_events_payload(json, false).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].description, "a");
    }

    #[test]
    fn parses_bare_array_payload() {
        let json = r#"[{"timestamp":1,"description":"a"},{"timestamp":2,"description":"b"}]"#;
        let events = parse_events_payload(json, false).unwrap();
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn parses_jsonl_payload() {
        let json =
            "{\"timestamp\":1,\"description\":\"a\"}\n{\"timestamp\":2,\"description\":\"b\"}";
        let events = parse_events_payload(json, true).unwrap();
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn detects_jsonl_without_extension() {
        let json =
            "{\"timestamp\":1,\"description\":\"a\"}\n{\"timestamp\":2,\"description\":\"b\"}";
        // Heuristic kicks in because two top-level `{...}` lines appear.
        let events = parse_events_payload(json, false).unwrap();
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn parses_rfc3339_timestamp() {
        let json = r#"[{"timestamp":"2026-05-12T15:23:00Z","description":"restart"}]"#;
        let events = parse_events_payload(json, false).unwrap();
        // 2026-05-12T15:23:00Z in nanos
        let expected = DateTime::parse_from_rfc3339("2026-05-12T15:23:00Z")
            .unwrap()
            .timestamp_nanos_opt()
            .unwrap() as u64;
        assert_eq!(events[0].timestamp, expected);
    }

    #[test]
    fn parses_humantime_duration() {
        let json = r#"[{"timestamp":1,"description":"d","duration":"30s"}]"#;
        let events = parse_events_payload(json, false).unwrap();
        assert_eq!(events[0].duration_ns, Some(30_000_000_000));
    }

    #[test]
    fn parses_duration_ns_int() {
        let json = r#"[{"timestamp":1,"description":"d","duration_ns":42}]"#;
        let events = parse_events_payload(json, false).unwrap();
        assert_eq!(events[0].duration_ns, Some(42));
    }

    #[test]
    fn rejects_missing_required_fields() {
        let err = parse_events_payload(r#"[{"timestamp":1}]"#, false).unwrap_err();
        assert!(err.contains("description"), "got: {err}");
    }

    #[test]
    fn parses_inline_event_minimal() {
        let e = parse_inline_event("time=1700000000000000000,description=restart").unwrap();
        assert_eq!(e.timestamp, 1_700_000_000_000_000_000);
        assert_eq!(e.description, "restart");
    }

    #[test]
    fn parses_inline_event_rfc3339() {
        let e = parse_inline_event(
            r#"time=2026-05-12T15:23:00Z,kind=restart,description="vllm restart""#,
        )
        .unwrap();
        assert_eq!(e.kind.as_deref(), Some("restart"));
        assert_eq!(e.description, "vllm restart");
    }

    #[test]
    fn parses_inline_event_quoted_description_with_comma() {
        let e = parse_inline_event(r#"time=1,description="hello, world""#).unwrap();
        assert_eq!(e.description, "hello, world");
    }

    #[test]
    fn parses_inline_event_with_labels() {
        let e =
            parse_inline_event("time=1,description=d,label.reason=OOM,label.deployer=ci").unwrap();
        assert_eq!(e.labels.get("reason").map(|s| s.as_str()), Some("OOM"));
        assert_eq!(e.labels.get("deployer").map(|s| s.as_str()), Some("ci"));
    }

    #[test]
    fn rejects_unknown_inline_key() {
        let err = parse_inline_event("time=1,description=d,whoops=x").unwrap_err();
        assert!(err.contains("whoops"), "got: {err}");
    }

    #[test]
    fn rejects_inline_without_description() {
        let err = parse_inline_event("time=1").unwrap_err();
        assert!(err.contains("description"), "got: {err}");
    }

    #[test]
    fn parse_timestamp_str_accepts_int() {
        assert_eq!(parse_timestamp_str("123").unwrap(), 123);
    }

    #[test]
    fn parse_timestamp_str_accepts_naive_iso() {
        // Naive datetimes are treated as UTC.
        let got = parse_timestamp_str("2026-05-12T15:23:00").unwrap();
        let expected = DateTime::parse_from_rfc3339("2026-05-12T15:23:00Z")
            .unwrap()
            .timestamp_nanos_opt()
            .unwrap() as u64;
        assert_eq!(got, expected);
    }

    #[test]
    fn parse_timestamp_str_rejects_garbage() {
        assert!(parse_timestamp_str("not a date").is_err());
    }

    #[test]
    fn parse_timestamp_str_accepts_seconds_omitted_rfc3339() {
        let with = parse_timestamp_str("2026-05-12T15:23:00Z").unwrap();
        assert_eq!(parse_timestamp_str("2026-05-12T15:23Z").unwrap(), with);
        // Also works with an offset suffix.
        let with_offset = parse_timestamp_str("2026-05-12T15:23:00+02:00").unwrap();
        assert_eq!(
            parse_timestamp_str("2026-05-12T15:23+02:00").unwrap(),
            with_offset
        );
    }

    #[test]
    fn parse_timestamp_str_naive_seconds_omitted() {
        let with = parse_timestamp_str("2026-05-12T15:23:00").unwrap();
        assert_eq!(parse_timestamp_str("2026-05-12T15:23").unwrap(), with);
    }

    #[test]
    fn inline_event_accepts_duration_in_both_spellings() {
        // Humantime form, the one the help text advertises.
        let e = parse_inline_event("time=1,description=warmup,duration=30s").unwrap();
        assert_eq!(e.duration_ns, Some(30_000_000_000));
        // Raw nanoseconds, the JSON field name.
        let e = parse_inline_event("time=1,description=warmup,duration_ns=1500").unwrap();
        assert_eq!(e.duration_ns, Some(1500));
        // A point event carries no duration at all.
        let e = parse_inline_event("time=1,description=point").unwrap();
        assert_eq!(e.duration_ns, None);
    }

    fn check_event(id: &str, ts: u64, duration: u64) -> Event {
        Event {
            timestamp: ts,
            description: "c".into(),
            kind: Some("check".into()),
            details: None,
            source: None,
            node: None,
            instance: None,
            labels: BTreeMap::new(),
            duration_ns: Some(duration),
            id: Some(id.into()),
            chart_id: None,
        }
    }

    #[test]
    fn append_events_counts_new_unchanged_and_batch_duplicates() {
        let existing = Events::new(vec![check_event("a", 1, 10)]);
        let appended = append_events(
            Some(existing),
            vec![
                check_event("a", 1, 10), // identical to the stored one
                check_event("b", 2, 10), // new
                check_event("b", 2, 10), // repeated inside the batch
            ],
        );
        assert_eq!(
            appended.counts,
            AppendCounts {
                new: 1,
                updated: 0,
                unchanged: 1,
                duplicates: 1
            }
        );
        assert_eq!(appended.events.events.len(), 2);
        assert!(appended
            .counts
            .describe()
            .contains("1 duplicate in this run"));
    }

    #[test]
    fn append_events_replaces_a_check_event_that_grew() {
        let existing = Events::new(vec![check_event("a", 1, 10)]);
        let appended = append_events(Some(existing), vec![check_event("a", 1, 25)]);
        assert_eq!(appended.counts.updated, 1);
        assert_eq!(appended.counts.new, 0);
        assert_eq!(appended.events.events.len(), 1);
        assert_eq!(appended.events.events[0].duration_ns, Some(25));
        assert!(!appended.counts.nothing_new());
    }

    #[test]
    fn append_events_does_not_overwrite_a_stored_non_check_event_with_a_check() {
        // A deploy event that carries a `check:` id (however it got one) is
        // not replaced or re-kinded by an incoming check event.
        let mut stored = check_event("check:abc", 1, 10);
        stored.kind = Some("deploy".into());
        stored.description = "rollout".into();
        let incoming = check_event("check:abc", 1, 25);
        let appended = append_events(Some(Events::new(vec![stored])), vec![incoming]);
        assert_eq!(appended.counts.unchanged, 1);
        assert_eq!(appended.counts.updated, 0);
        assert_eq!(appended.events.events.len(), 1);
        assert_eq!(appended.events.events[0].kind.as_deref(), Some("deploy"));
        assert_eq!(appended.events.events[0].description, "rollout");
        assert_eq!(appended.events.events[0].duration_ns, Some(10));
    }

    #[test]
    fn append_events_keeps_a_stored_event_that_is_not_a_check() {
        let mut stored = check_event("a", 1, 10);
        stored.kind = Some("deploy".into());
        let mut incoming = check_event("a", 1, 25);
        incoming.kind = Some("deploy".into());
        let appended = append_events(Some(Events::new(vec![stored])), vec![incoming]);
        assert_eq!(appended.counts.unchanged, 1);
        assert_eq!(appended.events.events[0].duration_ns, Some(10));
        assert!(appended.counts.nothing_new());
    }

    /// A recording of two sources (a source and its snapshot, each annotated
    /// alone) shows both sources' events, and removing one event drops it
    /// from each source's own payload: the copy keeps the event only it
    /// holds.
    #[test]
    fn a_recordings_sources_keep_their_own_events() {
        use ::rez::fixtures::{counter, map, snap_counters, write_dendro, Finish, Source, ANCHOR};
        use metriken_query::MetricsSource;
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.dendro");
        let rows = (0..4u64)
            .map(|i| {
                let ts = ANCHOR + i * 1_000_000_000;
                let w = Some(::rez::window::Window::new(ts - 50_000_000, ts));
                (snap_counters(ts, vec![counter("ops", "s", i, w)]), ts)
            })
            .collect();
        let source = Source {
            labels: map(&[("source", "rezolus")]),
            metadata: map(&[]),
            rows,
        };
        write_dendro(&a, &[source], None, Finish::Finalized);
        let any = crate::parquet_tools::select::RecordingSelector::default();
        let event = |id: &str| {
            parse_inline_event(&format!("time={},id={id},description={id}", ANCHOR + 1)).unwrap()
        };
        add_events_selected(&a, None, &any, vec![event("E1")], "rezolus").unwrap();
        let copy = dir.path().join("copy.dendro");
        crate::parquet_tools::snapshot_rez(&a, &copy).unwrap();
        add_events_selected(&copy, None, &any, vec![event("E2")], "rezolus").unwrap();
        let grp = dir.path().join("grp.dendro");
        crate::parquet_tools::combine::run_combine_for_test(&[a, copy], &grp).unwrap();

        let shown = |path: &Path| -> Vec<String> {
            let readers = ::rez::reader::RezReader::open_recordings(
                path,
                metriken_storage::BufferPool::new(8 << 20),
            )
            .unwrap();
            assert_eq!(readers.len(), 1, "a source and its copy are one recording");
            readers[0]
                .1
                .file_metadata()
                .get(::dashboard::keys::KEY_EVENTS)
                .map(|s| serde_json::from_str::<Events>(s).unwrap().events)
                .unwrap_or_default()
                .into_iter()
                .filter_map(|e| e.id)
                .collect()
        };
        assert_eq!(shown(&grp), ["E1", "E2"]);

        let filter = RemoveFilter {
            ids: vec!["E1".to_string()],
            ..Default::default()
        };
        let report = remove_events_selected(&grp, None, &any, &filter, "rezolus").unwrap();
        assert_eq!((report.removed, report.total), (1, 1));
        assert_eq!(shown(&grp), ["E2"]);
        let per_source: Vec<Vec<String>> = crate::parquet_tools::annotate::source_metadata(&grp)
            .iter()
            .map(|m| {
                m.get(::dashboard::keys::KEY_EVENTS)
                    .map(|s| serde_json::from_str::<Events>(s).unwrap().events)
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|e| e.id)
                    .collect()
            })
            .collect();
        assert_eq!(per_source, [vec![], vec!["E2".to_string()]]);
    }
}
