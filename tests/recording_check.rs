//! Process-level tests for `rezolus recording check`.
//!
//! The exit status is the CI contract, and the events `--annotate` writes are
//! what the viewer reads back, so both are exercised through the real binary
//! against the small `simple_capture.parquet` fixture (five samples of a
//! gauge `queue_depth`, constant at 3, and a counter `http_requests_total`
//! with two label sets). A two-recording `.rez` is assembled from copies of
//! that fixture with `recording annotate --source` and `recording combine`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_rezolus")
}

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("site/viewer/data/simple_capture.parquet")
}

fn rezolus(args: &[&str]) -> Output {
    Command::new(bin())
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("running rezolus {}: {e}", args.join(" ")))
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn write_queries(dir: &Path, name: &str, kpis: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(
        &path,
        format!(r#"{{"service_name": "simple", "kpis": [{kpis}]}}"#),
    )
    .unwrap();
    path
}

/// `queue_depth` is 3 throughout, so `above 10` passes.
const PASSING: &str = r#"{"role": "q", "title": "Queue depth low", "query": "queue_depth", "type": "gauge",
    "check": {"above": 10}}"#;
/// `above 2 for 2s` is violated for the whole recording (4 evaluated points).
const FAILING: &str = r#"{"role": "q", "title": "Queue depth bounded", "query": "queue_depth", "type": "gauge",
    "check": {"above": 2, "for": "2s"}}"#;
/// Violated too, but only a warning.
const WARNING: &str = r#"{"role": "q", "title": "Queue depth sane", "query": "queue_depth", "type": "gauge",
    "check": {"below": 100, "severity": "warn"}}"#;
const MISSING_METRIC: &str = r#"{"role": "r", "title": "Ghost rate", "query": "sum(rate(no_such_metric[5s]))",
    "type": "delta_counter", "check": {"above": 1}}"#;
const CHART_ONLY: &str =
    r#"{"role": "q", "title": "Queue depth", "query": "queue_depth", "type": "gauge"}"#;

#[test]
fn a_passing_check_exits_zero_and_says_pass() {
    let dir = tempfile::tempdir().unwrap();
    let queries = write_queries(dir.path(), "q.json", &[PASSING, CHART_ONLY].join(","));
    let out = rezolus(&[
        "recording",
        "check",
        fixture().to_str().unwrap(),
        "--queries",
        queries.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.contains("PASS          Queue depth low  above 10\n"),
        "{text}"
    );
    // The chart-only KPI is not a check and is not listed.
    assert!(!text.contains("Queue depth  "), "{text}");
    assert!(
        text.contains("1 check: 1 passed, 0 failed, 0 warned, 0 indeterminate, 0 errors"),
        "{text}"
    );
}

#[test]
fn a_failing_check_exits_one_and_a_warning_does_not() {
    let dir = tempfile::tempdir().unwrap();
    let queries = write_queries(dir.path(), "q.json", &[PASSING, FAILING, WARNING].join(","));
    let out = rezolus(&[
        "recording",
        "check",
        fixture().to_str().unwrap(),
        "--queries",
        queries.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.contains("FAIL          Queue depth bounded  above 2 for 2s  ["),
        "{text}"
    );
    assert!(text.contains(", 1 window]"), "{text}");
    assert!(
        text.contains("WARN          Queue depth sane  below 100  ["),
        "{text}"
    );
    assert!(
        text.contains("3 checks: 1 passed, 1 failed, 1 warned, 0 indeterminate, 0 errors"),
        "{text}"
    );

    // A warning alone is exit 0.
    let queries = write_queries(dir.path(), "w.json", WARNING);
    let out = rezolus(&[
        "recording",
        "check",
        fixture().to_str().unwrap(),
        "--queries",
        queries.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
}

#[test]
fn a_missing_metric_is_an_error_and_exits_two() {
    let dir = tempfile::tempdir().unwrap();
    // With both an error and a failure in one run, the exit status is 2.
    let queries = write_queries(dir.path(), "q.json", &[FAILING, MISSING_METRIC].join(","));
    let out = rezolus(&[
        "recording",
        "check",
        fixture().to_str().unwrap(),
        "--queries",
        queries.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.contains("ERROR         Ghost rate  above 1  error: "),
        "{text}"
    );
    assert!(text.contains("no_such_metric"), "{text}");
    assert!(text.contains("1 failed"), "{text}");
    assert!(text.contains("1 error\n"), "{text}");
}

#[test]
fn json_output_is_an_array_of_verdicts() {
    let dir = tempfile::tempdir().unwrap();
    let queries = write_queries(dir.path(), "q.json", &[PASSING, FAILING].join(","));
    let out = rezolus(&[
        "recording",
        "check",
        fixture().to_str().unwrap(),
        "--queries",
        queries.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    let v: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("stdout is JSON");
    let results = v.as_array().expect("an array");
    assert_eq!(results.len(), 2);

    assert_eq!(results[0]["title"], "Queue depth low");
    assert_eq!(results[0]["status"], "pass");
    assert_eq!(results[0]["check"]["above"], 10.0);
    assert_eq!(results[0]["check"]["severity"], "fail");
    assert_eq!(results[0]["windows"].as_array().unwrap().len(), 0);
    assert!(
        results[0].get("recording").is_none(),
        "a parquet file has no labels"
    );

    assert_eq!(results[1]["status"], "fail");
    assert_eq!(results[1]["check"]["for"], "2s");
    let windows = results[1]["windows"].as_array().unwrap();
    assert_eq!(windows.len(), 1);
    let w = &windows[0];
    assert!(w["start"].as_str().unwrap().ends_with('Z'));
    assert!(w["start_ns"].as_u64().is_some());
    assert!(w["duration_ns"].as_u64().unwrap() >= 2_000_000_000);
    assert_eq!(w["points"], 4);
}

fn events_of_parquet(path: &Path) -> Vec<serde_json::Value> {
    let out = rezolus(&[
        "recording",
        "metadata",
        "-i",
        path.to_str().unwrap(),
        "--field",
        "events",
    ]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let v: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("events JSON");
    v["events"].as_array().expect("events array").clone()
}

#[test]
fn annotate_writes_check_events_once_per_window() {
    let dir = tempfile::tempdir().unwrap();
    let copy = dir.path().join("copy.parquet");
    std::fs::copy(fixture(), &copy).unwrap();
    let queries = write_queries(dir.path(), "q.json", &[PASSING, FAILING, WARNING].join(","));
    let args = [
        "recording",
        "check",
        copy.to_str().unwrap(),
        "--queries",
        queries.to_str().unwrap(),
        "--annotate",
    ];

    let out = rezolus(&args);
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    assert!(
        stdout(&out).contains("2 new check event(s)"),
        "{}",
        stdout(&out)
    );

    let events = events_of_parquet(&copy);
    assert_eq!(
        events.len(),
        2,
        "one event per FAIL/WARN window: {events:?}"
    );
    for e in &events {
        assert_eq!(e["kind"], "check");
        assert!(e["duration_ns"].as_u64().unwrap() > 0, "{e}");
        assert!(e["id"].as_str().unwrap().starts_with("check:"), "{e}");
        let details = e["details"].as_str().unwrap();
        // Line 1 is the condition; line 2 is the check as JSON, the record
        // of what this verdict was evaluated against.
        let (_, json) = details.split_once('\n').expect("two lines");
        let check: serde_json::Value = serde_json::from_str(json).unwrap();
        assert!(check.get("above").is_some() || check.get("below").is_some());
    }
    let fail = events
        .iter()
        .find(|e| e["description"] == "Queue depth bounded")
        .expect("the failing check's event");
    assert_eq!(fail["labels"]["severity"], "fail");
    assert!(fail["details"]
        .as_str()
        .unwrap()
        .starts_with("Queue depth bounded above 2 for 2s: fail\n"));

    // Same checks again: same ids, nothing added.
    let out = rezolus(&args);
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    assert!(
        stdout(&out).contains("nothing new; 2 check event(s) already present"),
        "{}",
        stdout(&out)
    );
    assert_eq!(events_of_parquet(&copy).len(), 2);

    // With --json the annotation report leaves stdout to the array.
    let out = rezolus(&[
        "recording",
        "check",
        copy.to_str().unwrap(),
        "--queries",
        queries.to_str().unwrap(),
        "--annotate",
        "--json",
    ]);
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    let v: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("stdout is only JSON");
    assert_eq!(v.as_array().unwrap().len(), 3);
    assert!(stderr(&out).contains("nothing new"), "{}", stderr(&out));
}

#[test]
fn two_checks_with_one_title_write_two_events() {
    let dir = tempfile::tempdir().unwrap();
    let copy = dir.path().join("copy.parquet");
    std::fs::copy(fixture(), &copy).unwrap();
    // Same title, same condition, same window, different queries: two checks.
    let queries = write_queries(
        dir.path(),
        "q.json",
        r#"{"role": "q", "title": "Dup title", "query": "queue_depth", "type": "gauge", "check": {"above": 2}},
           {"role": "q", "title": "Dup title", "query": "queue_depth * 1", "type": "gauge", "check": {"above": 2}}"#,
    );
    let out = rezolus(&[
        "recording",
        "check",
        copy.to_str().unwrap(),
        "--queries",
        queries.to_str().unwrap(),
        "--annotate",
    ]);
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    assert!(
        stdout(&out).contains("2 new check event(s)"),
        "{}",
        stdout(&out)
    );
    let events = events_of_parquet(&copy);
    assert_eq!(events.len(), 2, "{events:?}");
    assert_ne!(events[0]["id"], events[1]["id"]);
}

/// A two-recording `.rez` from two copies of the fixture with different
/// `source` labels.
fn two_recording_rez(dir: &Path) -> PathBuf {
    let a = dir.join("a.parquet");
    let b = dir.join("b.parquet");
    std::fs::copy(fixture(), &a).unwrap();
    std::fs::copy(fixture(), &b).unwrap();
    let out = rezolus(&[
        "recording",
        "annotate",
        b.to_str().unwrap(),
        "--source",
        "other",
        "--overwrite",
    ]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let rez = dir.join("ab.rez");
    let out = rezolus(&[
        "recording",
        "combine",
        a.to_str().unwrap(),
        b.to_str().unwrap(),
        "-o",
        rez.to_str().unwrap(),
    ]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    rez
}

fn rez_recordings(path: &Path) -> Vec<serde_json::Value> {
    let out = rezolus(&[
        "recording",
        "metadata",
        "-i",
        path.to_str().unwrap(),
        "--json",
    ]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let v: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("manifest JSON");
    v["recordings"].as_array().expect("recordings").clone()
}

#[test]
fn a_multi_recording_rez_is_checked_whole_or_by_selector() {
    let dir = tempfile::tempdir().unwrap();
    let rez = two_recording_rez(dir.path());
    let queries = write_queries(dir.path(), "q.json", &[PASSING, FAILING].join(","));

    // No selector: every recording, each line prefixed with its labels.
    let out = rezolus(&[
        "recording",
        "check",
        rez.to_str().unwrap(),
        "--queries",
        queries.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.contains("[source=other] FAIL          Queue depth bounded"),
        "{text}"
    );
    assert!(
        text.contains("[source=127.0.0.1-18651] PASS          Queue depth low"),
        "{text}"
    );
    assert!(text.contains("4 checks: 2 passed, 2 failed"), "{text}");

    // A selector that names nothing lists the recordings and exits 2.
    let out = rezolus(&[
        "recording",
        "check",
        rez.to_str().unwrap(),
        "--queries",
        queries.to_str().unwrap(),
        "--recording",
        "source=nope",
    ]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        stderr(&out).contains("--recording source=other"),
        "{}",
        stderr(&out)
    );

    // A selector plus --annotate writes into that recording only.
    let out = rezolus(&[
        "recording",
        "check",
        rez.to_str().unwrap(),
        "--queries",
        queries.to_str().unwrap(),
        "--recording",
        "source=other",
        "--annotate",
    ]);
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    let text = stdout(&out);
    assert!(!text.contains("source=127.0.0.1-18651"), "{text}");
    assert!(text.contains("1 new check event(s)"), "{text}");
    assert!(text.contains("into 1 of 2 recording(s)"), "{text}");

    let recordings = rez_recordings(&rez);
    assert_eq!(recordings.len(), 2);
    for rec in &recordings {
        let events = rec["metadata"].get("events").and_then(|s| s.as_str());
        if rec["labels"]["source"] == "other" {
            let payload: serde_json::Value =
                serde_json::from_str(events.expect("the selected recording has events")).unwrap();
            let events = payload["events"].as_array().unwrap();
            assert_eq!(events.len(), 1);
            assert_eq!(events[0]["kind"], "check");
            assert!(events[0]["duration_ns"].as_u64().unwrap() > 0);
        } else {
            assert!(events.is_none(), "the other recording is untouched: {rec}");
        }
    }

    // JSON output on a .rez names the recording each verdict belongs to.
    let out = rezolus(&[
        "recording",
        "check",
        rez.to_str().unwrap(),
        "--queries",
        queries.to_str().unwrap(),
        "--json",
    ]);
    let v: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
    let sources: Vec<&str> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["recording"]["source"].as_str().unwrap())
        .collect();
    assert_eq!(
        sources,
        ["127.0.0.1-18651", "127.0.0.1-18651", "other", "other"]
    );
}

#[test]
fn a_recording_without_checks_exits_zero_with_a_note() {
    let out = rezolus(&["recording", "check", fixture().to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert!(
        stderr(&out).contains("no checks to run"),
        "{}",
        stderr(&out)
    );
    assert!(stdout(&out).is_empty());
}

#[test]
fn an_invalid_check_in_the_queries_file_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let queries = write_queries(
        dir.path(),
        "q.json",
        r#"{"role": "q", "title": "t", "query": "queue_depth", "type": "gauge", "check": {"above": 1, "below": 2}}"#,
    );
    let out = rezolus(&[
        "recording",
        "check",
        fixture().to_str().unwrap(),
        "--queries",
        queries.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("exactly one"), "{}", stderr(&out));
}

/// A dendro archive takes `check --annotate` as a `.rez` does: the events go
/// into the selected source's metadata, and `recording metadata --json`
/// reads them back out of the dendro catalog.
#[test]
fn check_annotates_a_dendro_archive() {
    let dir = tempfile::tempdir().unwrap();
    let rez = two_recording_rez(dir.path());
    let dendro = dir.path().join("ab.dendro");
    let out = rezolus(&[
        "recording",
        "upgrade",
        "--to",
        "dendro",
        rez.to_str().unwrap(),
        "-o",
        dendro.to_str().unwrap(),
    ]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let queries = write_queries(dir.path(), "q.json", &[PASSING, FAILING].join(","));

    let out = rezolus(&[
        "recording",
        "check",
        dendro.to_str().unwrap(),
        "--queries",
        queries.to_str().unwrap(),
        "--recording",
        "source=other",
        "--annotate",
    ]);
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    assert!(
        stdout(&out).contains("1 new check event(s)"),
        "{}",
        stdout(&out)
    );

    let recordings = rez_recordings(&dendro);
    assert_eq!(recordings.len(), 2);
    for rec in &recordings {
        let events = rec["metadata"].get("events").and_then(|s| s.as_str());
        if rec["labels"]["source"] == "other" {
            let payload: serde_json::Value =
                serde_json::from_str(events.expect("the selected source has events")).unwrap();
            assert_eq!(payload["events"].as_array().unwrap().len(), 1);
        } else {
            assert!(events.is_none(), "the other source is untouched: {rec}");
        }
    }
}
