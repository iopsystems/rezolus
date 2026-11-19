//! Archives written by a released rezolus, read by this one.
//!
//! `tests/fixtures/released/` holds a `.rez` and a `.dendro` that rezolus
//! 5.29.2's `record` wrote, and `golden.json`, what 5.29.2's MCP `query`
//! tool answered on each (see that directory's README). This test upgrades
//! the `.rez` with `recording upgrade`, opens the `.dendro` as it is, and
//! asks this build's `query` tool the same queries. Every point 5.29.2
//! returned must come back at the same timestamp with the same value, for
//! the same series, and each recording keeps its labels and metadata.
//!
//! The archives hold no histogram data (every histogram column is null), so
//! histogram reads are not covered here.
//!
//! The queries are ones whose meaning did not change in 6.0. `rate(x[r])`
//! with `r` longer than the interval is not among them: 5.x returned the
//! per-step rate and ignored `[r]`, where 6.0 averages over `[t - r, t]`.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_rezolus")
}

fn released() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/released")
}

/// The queries golden.json answers, as in golden.py's `QUERIES`.
const QUERIES: &[&str] = &[
    "cpu_cores",
    "rezolus_memory_usage_resident_set_size",
    "rate(cpu_usage[1s])",
    "sum by (state) (irate(cpu_usage[5s]))",
    "irate(blockio_operations[5s])",
    "irate(blockio_bytes[5s])",
    "irate(blockio_driver_errors[5s])",
    "irate(rezolus_context_switch[5s])",
    "irate(blockio_service_time[5s])",
    "irate(rezolus_memory_page_faults[5s])",
    "rate(rezolus_cpu_usage[1s])",
];

/// The fewest points a golden series holds: 5.29.2 answered with 15 points
/// for each gauge and 13 for each rate.
const MIN_POINTS: usize = 13;

type Points = BTreeMap<u64, f64>;
type Labels = BTreeMap<String, String>;
/// A series' labels other than `host` and `source`, as sorted pairs.
type Key = Vec<(String, String)>;

/// A query's series, keyed by their labels other than `host` and `source`.
/// 5.29.2's `query` left out the recording's labels; this build adds them.
fn series(results: &Value) -> BTreeMap<Key, (Labels, Points)> {
    let mut out = BTreeMap::new();
    for s in results.as_array().unwrap() {
        let labels: BTreeMap<String, String> = serde_json::from_value(s["labels"].clone()).unwrap();
        let key: Key = labels
            .iter()
            .filter(|(k, _)| *k != "host" && *k != "source")
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let points = s["points"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| {
                let t = p[0].as_f64().unwrap();
                assert_eq!(t.fract(), 0.0, "a point between whole seconds: {t}");
                (t as u64, p[1].as_f64().unwrap())
            })
            .collect();
        assert!(
            out.insert(key.clone(), (labels, points)).is_none(),
            "two series with the labels {key:?}"
        );
    }
    out
}

/// The one recording in `archive` as `recording metadata --json` lists it.
fn recording(archive: &Path) -> Value {
    let out = Command::new(bin())
        .args(["recording", "metadata", "--json", "-i"])
        .arg(archive)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let listing: Value = serde_json::from_slice(&out.stdout).unwrap();
    let recordings = listing["recordings"].as_array().unwrap();
    assert_eq!(recordings.len(), 1, "{listing}");
    recordings[0].clone()
}

/// This build's answers to `queries` on `archive`, in golden.json's shape.
fn ask(archive: &Path, queries: &[String]) -> BTreeMap<String, Value> {
    let mut child = Command::new(bin())
        .arg("mcp")
        .env_remove("REZOLUS_TEMPLATES")
        .env_remove("RUST_LOG")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    {
        let mut stdin = child.stdin.take().unwrap();
        let init =
            serde_json::json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {}});
        writeln!(stdin, "{init}").unwrap();
        writeln!(
            stdin,
            "{}",
            serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
        )
        .unwrap();
        for (i, q) in queries.iter().enumerate() {
            let call = serde_json::json!({
                "jsonrpc": "2.0", "id": i + 1, "method": "tools/call",
                "params": {"name": "query", "arguments": {"parquet_file": archive, "query": q}},
            });
            writeln!(stdin, "{call}").unwrap();
        }
    }
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8(out.stdout).unwrap();
    let responses: BTreeMap<u64, Value> = stdout
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .filter_map(|v| Some((v.get("id")?.as_u64()?, v)))
        .collect();
    let mut answers = BTreeMap::new();
    for (i, q) in queries.iter().enumerate() {
        let r = &responses[&(i as u64 + 1)];
        let text = r
            .pointer("/result/content/0/text")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("{q}: no result: {r}"));
        let matrix: Value = serde_json::from_str(text).unwrap();
        let series: Vec<Value> = matrix["result"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| {
                let points: Vec<Value> = s["values"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|p| serde_json::json!([p[0], p[1]]))
                    .collect();
                serde_json::json!({"labels": s["metric"], "points": points})
            })
            .collect();
        answers.insert(q.clone(), Value::Array(series));
    }
    answers
}

/// Every golden point is in `got` with the same value; `got` may add one
/// point a second before the golden series' first and one a second after its
/// last, and no others.
fn assert_matches(archive: &str, query: &str, golden: &Value, got: &Value) {
    let (want, have) = (series(golden), series(got));
    assert_eq!(
        want.keys().collect::<Vec<_>>(),
        have.keys().collect::<Vec<_>>(),
        "{archive}: {query}: series"
    );
    for (key, (_, want)) in &want {
        let (labels, have) = &have[key];
        // A 5.x recording keeps its `source` and `host` labels in 6.0, on
        // every series an aggregation has not reduced (those keep `sampler`).
        if labels.contains_key("sampler") {
            assert_eq!(
                labels.get("source").map(String::as_str),
                Some("rezolus"),
                "{archive}: {query}: {labels:?}"
            );
            assert_eq!(
                labels.get("host").map(String::as_str),
                Some("macbook.localdomain"),
                "{archive}: {query}: {labels:?}"
            );
        }
        assert!(
            want.len() >= MIN_POINTS,
            "{archive}: {query}: {key:?}: golden has {} points",
            want.len()
        );
        let first = *want.keys().next().unwrap();
        let last = *want.keys().next_back().unwrap();
        for (t, w) in want {
            let h = have
                .get(t)
                .unwrap_or_else(|| panic!("{archive}: {query}: {key:?}: no point at {t}"));
            let scale = w.abs().max(h.abs()).max(1e-12);
            assert!(
                (w - h).abs() / scale <= 1e-9,
                "{archive}: {query}: {key:?} at {t}: 5.29.2 {w}, now {h}"
            );
        }
        for t in have.keys().filter(|t| !want.contains_key(t)) {
            assert!(
                *t + 1 == first || *t == last + 1,
                "{archive}: {query}: {key:?}: extra point at {t}"
            );
        }
    }
}

#[test]
fn a_released_recording_answers_as_its_release_did() {
    let golden: Value =
        serde_json::from_str(&std::fs::read_to_string(released().join("golden.json")).unwrap())
            .unwrap();
    assert_eq!(golden["written_by"], "rezolus 5.29.2");
    let tmp = tempfile::tempdir().unwrap();
    for name in ["v5.29.2.rez", "v5.29.2.dendro"] {
        std::fs::copy(released().join(name), tmp.path().join(name)).unwrap();
    }

    let upgraded = tmp.path().join("upgraded.dendro");
    let out = Command::new(bin())
        .args(["recording", "upgrade"])
        .arg(tmp.path().join("v5.29.2.rez"))
        .arg("-o")
        .arg(&upgraded)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");

    // A `.dendro` from 5.29.2 opens as it is.
    for (archive, path) in [
        ("v5.29.2.rez", upgraded),
        ("v5.29.2.dendro", tmp.path().join("v5.29.2.dendro")),
    ] {
        let r = recording(&path);
        assert_eq!(r["complete"], true, "{archive}: {r}");
        assert_eq!(r["labels"]["host"], "macbook.localdomain", "{archive}: {r}");
        assert_eq!(r["labels"]["source"], "rezolus", "{archive}: {r}");
        assert_eq!(r["metadata"]["version"], "5.29.2", "{archive}: {r}");
        let systeminfo: Value =
            serde_json::from_str(r["metadata"]["systeminfo"].as_str().unwrap()).unwrap();
        assert_eq!(systeminfo["hostname"], "macbook.localdomain", "{archive}");
        assert!(
            r["metadata"]["descriptions"].as_str().unwrap().len() > 1000,
            "{archive}: descriptions"
        );

        let want = golden["archives"][archive].as_object().unwrap();
        let mut queries: Vec<String> = want.keys().cloned().collect();
        let mut expected: Vec<String> = QUERIES.iter().map(|q| q.to_string()).collect();
        queries.sort();
        expected.sort();
        assert_eq!(queries, expected, "{archive}: golden.json's queries");
        let got = ask(&path, &queries);
        for q in &queries {
            assert!(
                !want[q].as_array().unwrap().is_empty(),
                "{archive}: {q}: golden is empty"
            );
            assert_matches(archive, q, &want[q], &got[q]);
        }
    }
}
