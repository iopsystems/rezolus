//! A committed snapshot of the `rezolus recording` command line: every
//! subcommand's `-h` and `--help` text, the stdout, stderr and exit code of
//! a fixed list of invocations (successes, refusals, and `check`'s exit
//! codes 0, 1 and 2), and a `rezolus mcp` stdio session that drives the
//! write tools and `run_checks`, which share the recording tools' code.
//!
//! A change to any of them fails the test until the snapshot is regenerated.
//!
//! To regenerate after a deliberate change to the command line, run
//! `REZOLUS_UPDATE_SNAPSHOTS=1 cargo test --test recording_cli` and review
//! the diff of `tests/snapshots/`.
//!
//! Paths are written as `$TMP` (the test's scratch directory) and `$REPO`
//! (the crate root), so the snapshot does not depend on where it runs.

use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_rezolus")
}

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn snapshot_path(name: &str) -> PathBuf {
    repo().join("tests/snapshots").join(name)
}

/// Compare `actual` with the committed snapshot `name`, or write it when
/// `REZOLUS_UPDATE_SNAPSHOTS` is set.
fn assert_snapshot(name: &str, actual: &str) {
    let path = snapshot_path(name);
    if std::env::var_os("REZOLUS_UPDATE_SNAPSHOTS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "reading {}: {e}; run with REZOLUS_UPDATE_SNAPSHOTS=1 to create it",
            path.display()
        )
    });
    if expected != actual {
        let first = expected
            .lines()
            .zip(actual.lines())
            .position(|(a, b)| a != b)
            .unwrap_or_else(|| expected.lines().count().min(actual.lines().count()));
        let show = |s: &str| {
            s.lines()
                .skip(first.saturating_sub(3))
                .take(8)
                .collect::<Vec<_>>()
                .join("\n")
        };
        panic!(
            "{} differs from the command line's output, first at line {}.\n\
             --- committed\n{}\n--- now\n{}\n\
             If the change is deliberate, rerun with REZOLUS_UPDATE_SNAPSHOTS=1.",
            path.display(),
            first + 1,
            show(&expected),
            show(actual)
        );
    }
}

/// Every subcommand of `rezolus <top>`, read off its `Commands:` block.
fn subcommands(top: &str) -> Vec<String> {
    let out = Command::new(bin()).args([top, "--help"]).output().unwrap();
    let help = String::from_utf8_lossy(&out.stdout).into_owned();
    let Some((_, rest)) = help.split_once("\nCommands:\n") else {
        return Vec::new();
    };
    rest.lines()
        .take_while(|l| !l.trim().is_empty())
        .filter_map(|l| l.split_whitespace().next())
        .map(str::to_string)
        .collect()
}

/// One invocation's result, as a section of the snapshot.
fn render(args: &[String], tmp: &Path) -> String {
    let out = Command::new(bin())
        .args(args)
        .current_dir(tmp)
        .env("REZOLUS_NO_OPEN", "1")
        // A developer-mode build reads its built-in templates from this
        // directory, relative to the working directory unless set, and the
        // invocations run in a scratch directory.
        .env(
            "REZOLUS_TEMPLATES",
            repo().join("crates/dashboard/templates"),
        )
        .env_remove("RUST_LOG")
        .output()
        .unwrap_or_else(|e| panic!("running rezolus {}: {e}", args.join(" ")));
    let shown: Vec<String> = args.iter().map(|a| scrub(a, tmp)).collect();
    format!(
        "=== rezolus {}\n--- exit {}\n--- stdout\n{}--- stderr\n{}\n",
        shown.join(" "),
        out.status
            .code()
            .map_or_else(|| "signal".to_string(), |c| c.to_string()),
        scrub(&String::from_utf8_lossy(&out.stdout), tmp),
        scrub(&String::from_utf8_lossy(&out.stderr), tmp),
    )
}

/// `text` with the scratch directory and the crate root replaced by
/// placeholders. The scratch directory is replaced under both of its
/// spellings (macOS reaches `/var/folders` through `/private`).
fn scrub(text: &str, tmp: &Path) -> String {
    let mut out = text.to_string();
    let mut tmps = vec![tmp.to_path_buf()];
    if let Ok(canon) = tmp.canonicalize() {
        tmps.insert(0, canon);
    }
    for t in tmps {
        out = out.replace(&t.display().to_string(), "$TMP");
    }
    out.replace(&repo().display().to_string(), "$REPO")
}

#[test]
fn help_text_is_unchanged() {
    let mut paths: Vec<Vec<String>> = vec![vec![]];
    for top in ["recording", "mcp"] {
        paths.push(vec![top.to_string()]);
        for sub in subcommands(top) {
            if sub != "help" {
                paths.push(vec![top.to_string(), sub]);
            }
        }
    }
    // The alias reaches the same tree.
    paths.push(vec!["parquet".to_string()]);
    paths.push(vec!["parquet".to_string(), "metadata".to_string()]);

    let tmp = tempfile::tempdir().unwrap();
    let mut doc = String::new();
    for path in &paths {
        for flag in ["-h", "--help"] {
            let mut args = path.clone();
            args.push(flag.to_string());
            doc.push_str(&render(&args, tmp.path()));
        }
    }
    assert_snapshot("recording_cli_help.txt", &doc);
}

/// KPI templates for `check` and `annotate`. `queue_depth` is 3 throughout
/// the parquet fixture.
const PASSING: &str = r#"{"service_name": "simple", "kpis": [
    {"role": "q", "title": "Queue depth low", "query": "queue_depth", "type": "gauge",
     "check": {"above": 10}},
    {"role": "q", "title": "Queue depth", "query": "queue_depth", "type": "gauge"}]}"#;
const FAILING: &str = r#"{"service_name": "simple", "kpis": [
    {"role": "q", "title": "Queue depth bounded", "query": "queue_depth", "type": "gauge",
     "check": {"above": 2, "for": "2s"}},
    {"role": "q", "title": "Queue depth sane", "query": "queue_depth", "type": "gauge",
     "check": {"below": 100, "severity": "warn"}}]}"#;
const ERRORING: &str = r#"{"service_name": "simple", "kpis": [
    {"role": "r", "title": "Ghost rate", "query": "sum(rate(no_such_metric[5s]))",
     "type": "delta_counter", "check": {"above": 1}},
    {"role": "q", "title": "Queue depth bounded", "query": "queue_depth", "type": "gauge",
     "check": {"above": 2, "for": "2s"}}]}"#;
const BAD_UNIT: &str = r#"{"service_name": "simple", "kpis": [
    {"role": "load", "title": "Depth", "query": "queue_depth", "type": "gauge",
     "unit_system": "furlongs"}]}"#;
const NO_CHECKS: &str = r#"{"service_name": "simple", "kpis": [
    {"role": "q", "title": "Queue depth", "query": "queue_depth", "type": "gauge"}]}"#;

/// A raw msgpack recording, as a 5.x `record -o out.raw` wrote: one V2
/// snapshot per second, each with one counter.
fn raw_recording(count: u64) -> Vec<u8> {
    use metriken_exposition::{Counter, Snapshot, SnapshotV2};
    use std::collections::HashMap;
    use std::time::{Duration, SystemTime};

    let mut out = Vec::new();
    for i in 0..count {
        let snap = Snapshot::V2(SnapshotV2 {
            systemtime: SystemTime::UNIX_EPOCH
                + Duration::from_nanos(1_786_645_704_000_000_000 + i * 1_000_000_000),
            duration: Duration::ZERO,
            metadata: HashMap::new(),
            counters: vec![Counter::new(
                "cpu_cycles".to_string(),
                i * 100,
                [("metric".to_string(), "cpu_cycles".to_string())]
                    .into_iter()
                    .collect(),
            )],
            gauges: Vec::new(),
            histograms: Vec::new(),
            gauge_histograms: Vec::new(),
        });
        out.extend_from_slice(&Snapshot::to_msgpack(&snap).unwrap());
    }
    out
}

#[test]
fn invocations_are_unchanged() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tmp.path();
    let rez = repo().join("crates/rez/tests/fixtures/rez");
    for name in ["v2.rez", "v3.rez", "v3-live.rez", "v3-multi.rez"] {
        std::fs::copy(rez.join(name), t.join(name)).unwrap();
    }
    std::fs::copy(
        repo().join("site/viewer/data/simple_capture.parquet"),
        t.join("simple.parquet"),
    )
    .unwrap();
    std::fs::copy(
        repo().join("site/viewer/data/cachecannon.parquet"),
        t.join("cachecannon.parquet"),
    )
    .unwrap();
    for (name, body) in [
        ("pass.json", PASSING),
        ("fail.json", FAILING),
        ("error.json", ERRORING),
        ("bad-unit.json", BAD_UNIT),
        ("no-checks.json", NO_CHECKS),
        ("not-json.json", "{ not json"),
    ] {
        std::fs::write(t.join(name), body).unwrap();
    }
    // A template matched by the parquet fixture's `source`.
    std::fs::create_dir(t.join("tmpl")).unwrap();
    std::fs::write(
        t.join("tmpl/simple.json"),
        FAILING.replace("\"simple\"", "\"127.0.0.1-18651\""),
    )
    .unwrap();
    std::fs::write(t.join("garbage.raw"), b"this is not msgpack").unwrap();
    std::fs::write(t.join("rezolus.raw"), raw_recording(5)).unwrap();
    std::fs::write(t.join("sysinfo.json"), "{\"hostname\": \"web-01\"}").unwrap();
    std::fs::write(
        t.join("events.jsonl"),
        "{\"timestamp\": \"2026-01-01T00:00:00Z\", \"kind\": \"deploy\", \"description\": \"from a file\"}\n",
    )
    .unwrap();

    // Run in order: later invocations read what earlier ones wrote. Each is
    // split on whitespace; no argument here contains a space.
    let invocations: &[&str] = &[
        // clap's own refusals.
        "recording",
        "recording nonsense",
        "recording metadata",
        "recording annotate simple.parquet --undo --queries pass.json",
        "recording check simple.parquet -o x.dendro",
        // upgrade: the dendro fixtures the rest use.
        "recording upgrade simple.parquet -o simple.dendro",
        "recording upgrade v2.rez",
        "recording upgrade v3.rez -o v3.dendro",
        "recording upgrade v3-multi.rez -o multi.dendro",
        "recording upgrade v2.rez",
        "recording upgrade v2.rez -o v2b.dendro --to v3",
        "recording upgrade simple.dendro -o again.dendro",
        "recording upgrade missing.rez",
        // metadata.
        "recording metadata -i v3.rez",
        "recording metadata -i v3.rez --json",
        "recording metadata -i v2.rez",
        "recording metadata -i v3-live.rez",
        "recording metadata -i multi.dendro",
        "recording metadata -i simple.dendro --json",
        "recording metadata -i simple.parquet",
        "recording metadata -i simple.parquet --file --json",
        "recording metadata -i simple.parquet --schema",
        "recording metadata -i simple.parquet --geometry --json",
        "recording metadata -i simple.parquet --field source",
        "recording metadata -i simple.parquet --field no_such_key",
        "recording metadata -i missing.parquet",
        "parquet metadata -i v3.rez",
        // check: exit 0, 1 and 2, JSON, selectors, refusals.
        "recording check simple.parquet --queries pass.json",
        "recording check simple.parquet --queries fail.json",
        "recording check simple.parquet --queries error.json",
        "recording check simple.parquet --queries fail.json --json",
        "recording check simple.parquet --queries no-checks.json",
        "recording check simple.parquet",
        "recording check simple.parquet --queries not-json.json",
        "recording check simple.parquet --queries missing.json",
        "recording check missing.parquet --queries pass.json",
        "recording check simple.parquet --queries pass.json --recording source=x",
        "recording check simple.parquet --queries pass.json --recording nonsense",
        "recording check simple.dendro --queries fail.json",
        "recording check multi.dendro --queries pass.json",
        "recording check multi.dendro --queries pass.json --recording host=nowhere",
        "recording check simple.parquet --queries fail.json --annotate",
        "recording check simple.parquet --queries fail.json --annotate -o checked.dendro",
        "recording check simple.dendro --queries fail.json --annotate --json",
        "recording check simple.dendro --queries fail.json --annotate",
        "recording check simple.dendro --queries pass.json --annotate",
        // annotate.
        "recording annotate simple.dendro",
        "recording annotate simple.dendro --source x",
        "recording annotate simple.dendro --node x",
        "recording annotate simple.dendro --filter",
        "recording annotate simple.dendro --queries bad-unit.json",
        "recording annotate simple.dendro --queries not-json.json",
        "recording annotate simple.dendro --systeminfo missing.json",
        "recording annotate simple.dendro --systeminfo not-json.json",
        "recording annotate simple.parquet --queries pass.json",
        "recording annotate v3.rez --queries pass.json",
        "recording annotate simple.parquet --queries pass.json -o annotated.dendro",
        "recording annotate simple.dendro --queries pass.json",
        "recording annotate simple.dendro --event time=2026-01-01T00:00:00Z,kind=deploy,id=e1,description=rollout",
        "recording annotate simple.dendro --event time=2026-01-01T00:00:00Z,kind=deploy,id=e1,description=rollout",
        "recording annotate simple.dendro --event kind=deploy",
        "recording annotate simple.dendro --add-events events.jsonl",
        "recording annotate simple.dendro --clear-events",
        "recording annotate simple.dendro --systeminfo pass.json",
        "recording annotate simple.dendro --undo",
        "recording annotate simple.dendro --queries pass.json -o copy.dendro",
        "recording annotate missing.dendro --undo",
        // filter.
        "recording filter simple.dendro",
        "recording filter v3.rez --samplers a",
        "recording filter v3.rez --samplers a -o v3-a.dendro",
        "recording filter v3.rez --samplers a -o v3-a.rez",
        "recording filter v3-a.dendro --metrics mem_free -o v3-mem.dendro",
        "recording filter v3.dendro --metrics ops -o v3-ops.dendro",
        "recording filter v3-a.dendro --samplers zzz -o none.dendro",
        "recording filter simple.parquet --queries pass.json -o slim.dendro",
        "recording filter simple.parquet -o slim2.dendro",
        "recording filter v3.dendro --samplers a --queries pass.json -o v3-q.dendro",
        "recording upgrade v3.rez -o in-place.dendro",
        "recording filter in-place.dendro --samplers b",
        "recording metadata -i in-place.dendro",
        "recording metadata -i v3-a.dendro",
        "recording metadata -i v3-ops.dendro",
        // --templates replaces the built-in set for a recording without KPIs.
        "recording check simple.parquet --templates tmpl",
        "recording check simple.dendro --templates tmpl",
        "recording check simple.dendro --templates tmpl --json",
        "recording filter simple.parquet --templates tmpl -o slim3.dendro",
        // The built-in templates: the cachecannon capture carries no KPIs, so
        // filtering it keeps what the built-in cachecannon template names.
        "recording filter cachecannon.parquet -o cc-slim.dendro",
        // combine.
        "recording combine v3.dendro -o one.dendro",
        "recording combine v3.dendro simple.dendro -o c.rez",
        "recording combine v3.dendro simple.dendro -o c.parquet",
        "recording combine v3.dendro simple.dendro --pinned x -o c.dendro",
        "recording combine v3.dendro simple.dendro -o c.dendro",
        "recording combine v3.dendro simple.dendro -o c.dendro",
        "recording metadata -i c.dendro",
        "recording combine v3.dendro simple.parquet --ab baseline=v3.dendro experiment=simple.parquet -o ab.dendro",
        "recording metadata -i ab.dendro",
        "recording combine v3.dendro simple.dendro --ab baseline=v3.dendro experiment=nope -o ab2.dendro",
        "recording combine multi.dendro --ab baseline=0 experiment=1 -o ab3.dendro",
        "recording combine multi.dendro --ab baseline=0 experiment=7 -o ab4.dendro",
        "recording combine multi.dendro --ab baseline=zero experiment=1 -o ab5.dendro",
        "recording combine ab.dendro ab3.dendro -o two-arms-each.dendro",
        "recording check ab.dendro --queries pass.json",
        // convert.
        "recording convert garbage.raw",
        "recording convert garbage.raw -o out.parquet",
        "recording convert missing.raw",
        "recording convert garbage.raw --interval 0ms",
        "recording convert garbage.raw -m novalue",
        "recording convert v2.rez",
        "recording convert v3.rez",
        "recording convert simple.dendro",
        "recording convert simple.parquet",
        "recording convert rezolus.raw",
        "recording convert rezolus.raw",
        "recording convert rezolus.raw --force --systeminfo sysinfo.json",
        "recording convert rezolus.raw -o tagged.dendro -m source=llm-perf --interval 250ms",
        "recording metadata -i rezolus.dendro",
        "recording metadata -i tagged.dendro",
        // snapshot.
        "recording snapshot v3-live.rez -o snap.dendro",
        "recording snapshot v3-live.rez -o snap.rez",
        "recording snapshot snap.dendro -o snap2.dendro",
        "recording snapshot snap.dendro -o snap2.dendro",
        "recording snapshot simple.parquet -o snap3.dendro",
        "recording metadata -i snap2.dendro",
        // mcp shares the selector.
        "mcp describe-recording multi.dendro",
        "mcp describe-recording multi.dendro --recording host=nowhere",
        "mcp describe-recording simple.parquet --recording source=x",
    ];
    let mut doc = String::new();
    for line in invocations {
        let args: Vec<String> = line.split_whitespace().map(str::to_string).collect();
        doc.push_str(&render(&args, t));
    }
    assert_snapshot("recording_cli_invocations.txt", &doc);
}

/// A `rezolus mcp --allow-mutating` stdio session over the parquet fixture
/// converted to a dendro archive: the read tools the write tools sit beside,
/// then `add_event`, `run_checks` with `annotate`, and `remove_events`,
/// including their refusals. Each response is one JSON line; the snapshot
/// holds them pretty-printed, after the request that produced them.
#[test]
fn mcp_session_is_unchanged() {
    use std::io::Write;
    use std::process::Stdio;

    let tmp = tempfile::tempdir().unwrap();
    let t = tmp.path();
    std::fs::copy(
        repo().join("site/viewer/data/simple_capture.parquet"),
        t.join("simple.parquet"),
    )
    .unwrap();
    std::fs::copy(
        repo().join("crates/rez/tests/fixtures/rez/v3-multi.rez"),
        t.join("v3-multi.rez"),
    )
    .unwrap();
    for args in [
        "recording upgrade simple.parquet -o simple.dendro",
        "recording upgrade v3-multi.rez -o multi.dendro",
    ] {
        let out = Command::new(bin())
            .args(args.split_whitespace())
            .current_dir(t)
            .output()
            .unwrap();
        assert!(out.status.success(), "{args}: {out:?}");
    }

    let failing: serde_json::Value = serde_json::from_str(FAILING).unwrap();
    let call = |id: u64, name: &str, arguments: serde_json::Value| {
        serde_json::json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {"name": name, "arguments": arguments},
        })
    };
    let requests = vec![
        serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
        serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        call(
            3,
            "describe_recording",
            serde_json::json!({"parquet_file": "simple.dendro"}),
        ),
        call(
            4,
            "describe_recording",
            serde_json::json!({"parquet_file": "multi.dendro"}),
        ),
        call(
            5,
            "query",
            serde_json::json!({"parquet_file": "simple.dendro", "query": "queue_depth"}),
        ),
        call(
            6,
            "add_event",
            serde_json::json!({"parquet_file": "simple.dendro", "id": "e1", "kind": "deploy",
                "timestamp": "2026-07-04T05:27:15Z", "duration": "2s", "description": "rollout"}),
        ),
        call(
            7,
            "add_event",
            serde_json::json!({"parquet_file": "simple.dendro", "id": "e1",
                "timestamp": "2026-07-04T05:27:15Z", "description": "rollout"}),
        ),
        call(
            8,
            "add_event",
            serde_json::json!({"parquet_file": "simple.dendro", "timestamp": "1783142835",
                "description": "digits"}),
        ),
        call(
            9,
            "add_event",
            serde_json::json!({"parquet_file": "simple.parquet", "id": "e2",
                "timestamp": "2026-07-04T05:27:15Z", "description": "legacy"}),
        ),
        call(
            10,
            "add_event",
            serde_json::json!({"parquet_file": "simple.parquet", "output": "events.dendro",
                "id": "e2", "timestamp": "2026-07-04T05:27:15Z", "description": "legacy"}),
        ),
        call(
            11,
            "add_event",
            serde_json::json!({"parquet_file": "multi.dendro", "id": "e3",
                "timestamp": 1700000001, "description": "which"}),
        ),
        call(
            12,
            "run_checks",
            serde_json::json!({"parquet_file": "simple.dendro", "queries": failing,
                "annotate": true}),
        ),
        call(
            13,
            "run_checks",
            serde_json::json!({"parquet_file": "simple.dendro", "queries": failing,
                "annotate": true}),
        ),
        call(
            14,
            "run_checks",
            serde_json::json!({"parquet_file": "multi.dendro", "recording": {"host": "nowhere"}}),
        ),
        call(
            15,
            "run_checks",
            serde_json::json!({"parquet_file": "simple.dendro"}),
        ),
        call(
            16,
            "remove_events",
            serde_json::json!({"parquet_file": "simple.dendro", "ids": ["e1"]}),
        ),
        call(
            17,
            "remove_events",
            serde_json::json!({"parquet_file": "simple.dendro"}),
        ),
        call(
            18,
            "remove_events",
            serde_json::json!({"parquet_file": "simple.dendro", "kind": "check"}),
        ),
        call(
            19,
            "describe_recording",
            serde_json::json!({"parquet_file": "simple.dendro"}),
        ),
    ];

    let mut child = Command::new(bin())
        .args(["mcp", "--allow-mutating"])
        .current_dir(t)
        .env_remove("REZOLUS_TEMPLATES")
        .env_remove("RUST_LOG")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    // Dispatch is serial, so the responses come back in request order.
    {
        let mut stdin = child.stdin.take().unwrap();
        for r in &requests {
            writeln!(stdin, "{r}").unwrap();
        }
    }
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8(out.stdout).unwrap();
    let mut responses = stdout.lines();

    let mut doc = String::new();
    for r in &requests {
        doc.push_str(&format!("=== request\n{r}\n"));
        if r.get("id").is_none() {
            continue;
        }
        let line = responses.next().expect("a response per request with an id");
        let mut v: serde_json::Value = serde_json::from_str(line).unwrap();
        if let Some(info) = v.pointer_mut("/result/serverInfo/version") {
            *info = serde_json::json!("$VERSION");
        }
        // A tool's text that is JSON is shown as JSON, its keys sorted: a
        // query result's label order is a hash map's.
        if let Some(content) = v
            .pointer_mut("/result/content")
            .and_then(|c| c.as_array_mut())
        {
            for item in content {
                let parsed = item
                    .get("text")
                    .and_then(|t| t.as_str())
                    .and_then(|t| serde_json::from_str::<serde_json::Value>(t).ok())
                    .filter(|j| j.is_object() || j.is_array());
                if let Some(json) = parsed {
                    item["text"] = sorted(json);
                }
            }
        }
        doc.push_str(&format!(
            "--- response\n{}\n",
            serde_json::to_string_pretty(&v).unwrap()
        ));
    }
    assert!(responses.next().is_none(), "more responses than requests");
    assert_snapshot("recording_cli_mcp.txt", &scrub(&doc, t));
}

/// `v` with every object's keys in sorted order.
fn sorted(v: serde_json::Value) -> serde_json::Value {
    match v {
        serde_json::Value::Object(map) => {
            let ordered: std::collections::BTreeMap<String, serde_json::Value> =
                map.into_iter().map(|(k, v)| (k, sorted(v))).collect();
            serde_json::Value::Object(ordered.into_iter().collect())
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.into_iter().map(sorted).collect())
        }
        other => other,
    }
}
