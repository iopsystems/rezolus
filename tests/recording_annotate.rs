//! Process-level tests for `rezolus recording annotate`.
//!
//! A `--queries` file with a `unit_system` the viewer does not know is
//! refused before any edit: asked for `--event` alongside it, annotate exits
//! 1 and leaves the file byte for byte as it was, in each container. A
//! legacy recording (`.rez`, `.parquet`) is never changed: without `-o` the
//! edit is refused, naming the command to run; with `-o` it goes to a new
//! `.dendro`.

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

/// A copy in `dir` of the committed `.rez` `name`.
fn committed_rez(dir: &Path, name: &str) -> PathBuf {
    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("crates/rez/tests/fixtures/rez")
        .join(name);
    let dest = dir.join(name);
    std::fs::copy(src, &dest).unwrap();
    dest
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

/// A `.dendro` converted from a copy of the parquet fixture.
fn dendro_from_fixture(dir: &Path) -> PathBuf {
    let parquet = dir.join("simple.parquet");
    std::fs::copy(fixture(), &parquet).unwrap();
    let dendro = dir.join("simple.dendro");
    let out = rezolus(&[
        "recording",
        "upgrade",
        parquet.to_str().unwrap(),
        "-o",
        dendro.to_str().unwrap(),
    ]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    dendro
}

/// Run annotate with the bad queries plus `extra` arguments on `target`, and
/// assert exit 1, the reason on stderr, and an untouched file.
fn assert_refused_untouched(dir: &Path, target: &Path, extra: &[&str]) {
    let queries = bad_queries(dir);
    let before = std::fs::read(target).unwrap();
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
        std::fs::read(target).unwrap() == before,
        "{} was modified",
        target.display()
    );
}

#[test]
fn parquet_refusal_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let parquet = dir.path().join("x.parquet");
    std::fs::copy(fixture(), &parquet).unwrap();
    let out = dir.path().join("x.dendro");
    assert_refused_untouched(dir.path(), &parquet, &["-o", out.to_str().unwrap()]);
    assert!(!out.exists());
}

#[test]
fn rez_refusal_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let rez = committed_rez(dir.path(), "v3.rez");
    assert_refused_untouched(dir.path(), &rez, &[]);
}

#[test]
fn dendro_refusal_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let dendro = dendro_from_fixture(dir.path());
    assert_refused_untouched(dir.path(), &dendro, &[]);
}

/// A legacy recording without `-o` is refused, naming the command to run,
/// and is not changed; with `-o` the event goes into a new `.dendro`, and
/// the original is still not changed.
#[test]
fn a_legacy_recording_is_annotated_into_a_new_dendro() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["v3.rez", "v2.rez", "simple_capture.parquet"] {
        let input = if name.ends_with(".parquet") {
            let p = dir.path().join(name);
            std::fs::copy(fixture(), &p).unwrap();
            p
        } else {
            committed_rez(dir.path(), name)
        };
        let before = std::fs::read(&input).unwrap();
        let event = "time=2026-01-01T00:00:00Z,kind=deploy,description=x";
        let out = rezolus(&[
            "recording",
            "annotate",
            input.to_str().unwrap(),
            "--event",
            event,
        ]);
        assert_eq!(out.status.code(), Some(1), "{name}: {}", stderr(&out));
        let advice = format!("-o {}", input.with_extension("dendro").display());
        assert!(stderr(&out).contains(&advice), "{name}: {}", stderr(&out));

        let dendro = dir.path().join(format!("{name}.annotated.dendro"));
        let out = rezolus(&[
            "recording",
            "annotate",
            input.to_str().unwrap(),
            "--event",
            event,
            "-o",
            dendro.to_str().unwrap(),
        ]);
        assert!(out.status.success(), "{name}: {}", stderr(&out));
        let describe = rezolus(&["recording", "metadata", "-i", dendro.to_str().unwrap()]);
        assert!(describe.status.success(), "{name}: {}", stderr(&describe));
        assert_eq!(std::fs::read(&input).unwrap(), before, "{name} changed");
    }
}
