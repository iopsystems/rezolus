//! Event-annotation helpers for `rezolus parquet annotate`.
//!
//! Events are stored as a JSON blob in the parquet footer under
//! [`KEY_EVENTS`](crate::parquet_metadata::KEY_EVENTS). On-disk shape:
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
use parquet::file::metadata::KeyValue;
use std::collections::BTreeMap;
use std::path::Path;

use crate::parquet_metadata::KEY_EVENTS;
use crate::viewer::{Event, Events};

/// Apply event-related operations to a parquet file in the order
/// `clear → add file → add inline`.
///
/// Returns `Ok(true)` if anything changed (and the file was rewritten),
/// `Ok(false)` if no event operations were requested.
pub(super) fn run(
    path: &Path,
    add_files: &[&Path],
    inline: &[String],
    clear: bool,
) -> Result<bool, Box<dyn std::error::Error>> {
    if !clear && add_files.is_empty() && inline.is_empty() {
        return Ok(false);
    }

    let mut events = if clear {
        Events::default()
    } else {
        read_events(path)?.unwrap_or_default()
    };

    let original_existing_ids: std::collections::HashSet<String> = events
        .events
        .iter()
        .filter_map(|e| e.id.clone())
        .filter(|id| !id.is_empty())
        .collect();

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

    write_events(path, &events)?;

    if clear {
        println!("Cleared existing events from {:?}", path);
    }
    if added > 0 {
        println!(
            "Annotated {:?} with {} event(s) ({} total{})",
            path,
            added,
            events.events.len(),
            if dropped > 0 {
                format!(", {dropped} deduped by id")
            } else {
                String::new()
            },
        );
    } else if !clear {
        println!("No events to add to {:?}", path);
    }

    // normalize() keeps the earlier entry on id collision, so warn loudly
    // when a new CLI invocation tried to overwrite an existing event.
    let new_ids: Vec<String> = add_files
        .iter()
        .filter_map(|p| read_input_file(p).ok())
        .flatten()
        .chain(inline.iter().filter_map(|s| parse_inline_event(s).ok()))
        .filter_map(|e| e.id)
        .filter(|id| !id.is_empty())
        .collect();
    for id in &new_ids {
        if original_existing_ids.contains(id) {
            eprintln!(
                "warning: event id={id:?} already existed in file; kept original (use --clear-events to replace)"
            );
        }
    }

    Ok(true)
}

/// Read the existing `events` payload from a parquet file's footer.
pub(super) fn read_events(path: &Path) -> Result<Option<Events>, Box<dyn std::error::Error>> {
    let kv_meta = super::read_file_metadata(path)?;
    let Some(raw) = kv_meta
        .iter()
        .find(|kv| kv.key == KEY_EVENTS)
        .and_then(|kv| kv.value.as_deref())
    else {
        return Ok(None);
    };
    let events: Events = serde_json::from_str(raw)
        .map_err(|e| format!("invalid existing events payload in {path:?}: {e}"))?;
    Ok(Some(events))
}

fn write_events(path: &Path, events: &Events) -> Result<(), Box<dyn std::error::Error>> {
    let mut kv_meta = super::read_file_metadata(path)?;
    kv_meta.retain(|kv| kv.key != KEY_EVENTS);
    if !events.events.is_empty() {
        kv_meta.push(KeyValue {
            key: KEY_EVENTS.to_string(),
            value: Some(serde_json::to_string(events)?),
        });
    }
    let buf = super::rewrite_parquet(path, kv_meta, None)?;
    std::fs::write(path, &buf)?;
    Ok(())
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
pub(super) fn parse_timestamp_str(s: &str) -> Result<u64, String> {
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
fn parse_duration_str(s: &str) -> Result<u64, String> {
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

    // ── end-to-end tests against a real parquet file ──

    use arrow::array::UInt64Array;
    use arrow::datatypes::{DataType, Field, Schema};
    use arrow::record_batch::RecordBatch;
    use parquet::arrow::ArrowWriter;
    use parquet::file::metadata::KeyValue;
    use parquet::file::properties::WriterProperties;
    use std::sync::Arc;
    use tempfile::NamedTempFile;

    fn make_minimal_parquet(initial_kv: Vec<(&str, &str)>) -> NamedTempFile {
        let ts_field = Field::new("timestamp", DataType::UInt64, false);
        let schema = Arc::new(Schema::new(vec![ts_field]));
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![Arc::new(UInt64Array::from(vec![1u64, 2, 3]))],
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
        let mut writer = ArrowWriter::try_new(
            std::fs::File::create(tmp.path()).unwrap(),
            schema,
            Some(props),
        )
        .unwrap();
        writer.write(&batch).unwrap();
        writer.close().unwrap();
        tmp
    }

    fn write_events_file(events_json: &str) -> NamedTempFile {
        let tmp = NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), events_json).unwrap();
        tmp
    }

    #[test]
    fn run_adds_events_from_file() {
        let parquet = make_minimal_parquet(vec![("source", "rezolus")]);
        let events_file =
            write_events_file(r#"{"events":[{"timestamp":1,"description":"restart"}]}"#);

        let changed = run(parquet.path(), &[events_file.path()], &[], false).unwrap();
        assert!(changed);

        let stored = read_events(parquet.path()).unwrap().unwrap();
        assert_eq!(stored.events.len(), 1);
        assert_eq!(stored.events[0].description, "restart");
    }

    #[test]
    fn run_appends_to_existing_events() {
        let parquet = make_minimal_parquet(vec![("source", "rezolus")]);
        let first = write_events_file(r#"[{"timestamp":1,"description":"a"}]"#);
        run(parquet.path(), &[first.path()], &[], false).unwrap();

        let second = write_events_file(r#"[{"timestamp":2,"description":"b"}]"#);
        run(parquet.path(), &[second.path()], &[], false).unwrap();

        let stored = read_events(parquet.path()).unwrap().unwrap();
        assert_eq!(stored.events.len(), 2);
        assert_eq!(stored.events[0].description, "a");
        assert_eq!(stored.events[1].description, "b");
    }

    #[test]
    fn run_clear_then_add_replaces_events() {
        let parquet = make_minimal_parquet(vec![("source", "rezolus")]);
        let first = write_events_file(r#"[{"timestamp":1,"description":"old"}]"#);
        run(parquet.path(), &[first.path()], &[], false).unwrap();

        let second = write_events_file(r#"[{"timestamp":2,"description":"new"}]"#);
        run(parquet.path(), &[second.path()], &[], true).unwrap();

        let stored = read_events(parquet.path()).unwrap().unwrap();
        assert_eq!(stored.events.len(), 1);
        assert_eq!(stored.events[0].description, "new");
    }

    #[test]
    fn run_clear_only_wipes_events() {
        let parquet = make_minimal_parquet(vec![("source", "rezolus")]);
        let first = write_events_file(r#"[{"timestamp":1,"description":"old"}]"#);
        run(parquet.path(), &[first.path()], &[], false).unwrap();

        run(parquet.path(), &[], &[], true).unwrap();

        // Cleared: no key in metadata, read returns None.
        assert!(read_events(parquet.path()).unwrap().is_none());
    }

    #[test]
    fn run_inline_events_merge_with_file() {
        let parquet = make_minimal_parquet(vec![("source", "rezolus")]);
        let file = write_events_file(r#"[{"timestamp":1,"description":"file-event"}]"#);
        run(
            parquet.path(),
            &[file.path()],
            &["time=2,description=inline-event".to_string()],
            false,
        )
        .unwrap();

        let stored = read_events(parquet.path()).unwrap().unwrap();
        assert_eq!(stored.events.len(), 2);
        // Sorted by timestamp
        assert_eq!(stored.events[0].description, "file-event");
        assert_eq!(stored.events[1].description, "inline-event");
    }

    #[test]
    fn run_returns_false_when_no_event_args() {
        let parquet = make_minimal_parquet(vec![("source", "rezolus")]);
        let changed = run(parquet.path(), &[], &[], false).unwrap();
        assert!(!changed);
    }

    #[test]
    fn run_preserves_other_metadata() {
        let parquet = make_minimal_parquet(vec![("source", "rezolus"), ("node", "web01")]);
        let file = write_events_file(r#"[{"timestamp":1,"description":"d"}]"#);
        run(parquet.path(), &[file.path()], &[], false).unwrap();

        let kv = crate::parquet_tools::read_file_metadata(parquet.path()).unwrap();
        assert!(kv
            .iter()
            .any(|kv| kv.key == "source" && kv.value.as_deref() == Some("rezolus")));
        assert!(kv
            .iter()
            .any(|kv| kv.key == "node" && kv.value.as_deref() == Some("web01")));
    }

    #[test]
    fn run_dedupes_by_id_keeping_earlier() {
        let parquet = make_minimal_parquet(vec![("source", "rezolus")]);
        let first = write_events_file(r#"[{"timestamp":1,"description":"orig","id":"dup"}]"#);
        run(parquet.path(), &[first.path()], &[], false).unwrap();

        let second = write_events_file(r#"[{"timestamp":2,"description":"new","id":"dup"}]"#);
        run(parquet.path(), &[second.path()], &[], false).unwrap();

        let stored = read_events(parquet.path()).unwrap().unwrap();
        assert_eq!(stored.events.len(), 1);
        // Earlier entry won (matches the "use --clear-events to replace" hint).
        assert_eq!(stored.events[0].description, "orig");
    }
}
