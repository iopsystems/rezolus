//! Process-level tests for `rezolus recording annotate`'s refusal of a
//! `--queries` file with a `unit_system` the viewer does not know.
//!
//! The refusal must come before any edit: asked for `--event` (and, on a
//! parquet, `--source`) alongside the bad `--queries`, annotate exits 1 and
//! leaves the file byte for byte as it was, in each container.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn rezolus(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rezolus"))
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("running rezolus {}: {e}", args.join(" ")))
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("site/viewer/data/simple_capture.parquet")
}

/// Every file under `path` (or `path` itself), with its bytes, sorted by
/// path, so a directory-shaped archive compares as a whole.
fn snapshot(path: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![path.to_path_buf()];
    while let Some(p) = stack.pop() {
        if p.is_dir() {
            for entry in std::fs::read_dir(&p).unwrap() {
                stack.push(entry.unwrap().path());
            }
        } else {
            out.push((p.clone(), std::fs::read(&p).unwrap()));
        }
    }
    out.sort();
    out
}

fn bad_queries(dir: &Path) -> PathBuf {
    let path = dir.join("bad.json");
    std::fs::write(
        &path,
        r#"{"service_name": "simple", "kpis": [{"role": "load", "title": "Depth",
            "query": "queue_depth", "type": "gauge", "unit_system": "furlongs"}]}"#,
    )
    .unwrap();
    path
}

fn rez_from_fixture(dir: &Path) -> PathBuf {
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

/// Run annotate with the bad queries plus `extra` edits on `target`, and
/// assert exit 1, the reason on stderr, and an untouched file.
fn assert_refused_untouched(dir: &Path, target: &Path, extra: &[&str]) {
    let queries = bad_queries(dir);
    let before = snapshot(target);
    let mut args = vec![
        "recording",
        "annotate",
        target.to_str().unwrap(),
        "--queries",
        queries.to_str().unwrap(),
        "--event",
        "time=2026-01-01T00:00:00Z,kind=deploy,description=x",
    ];
    args.extend_from_slice(extra);
    let out = rezolus(&args);
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    assert!(
        stderr(&out).contains("unknown unit_system \"furlongs\""),
        "stderr: {}",
        stderr(&out)
    );
    assert!(
        snapshot(target) == before,
        "{} was modified",
        target.display()
    );
}

#[test]
fn parquet_refusal_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let parquet = dir.path().join("x.parquet");
    std::fs::copy(fixture(), &parquet).unwrap();
    assert_refused_untouched(
        dir.path(),
        &parquet,
        &["--source", "renamed", "--overwrite"],
    );
}

#[test]
fn rez_refusal_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let rez = rez_from_fixture(dir.path());
    assert_refused_untouched(dir.path(), &rez, &[]);
}

#[test]
fn dendro_refusal_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let rez = rez_from_fixture(dir.path());
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
    assert_refused_untouched(dir.path(), &dendro, &[]);
}
