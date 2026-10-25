//! A two-file comparison is one recording per file. A file holding several
//! is refused with the commands that compare two of its recordings; each
//! test runs those commands as printed, so the advice is shown to work.

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

fn rezolus(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rezolus"))
        .args(args)
        .current_dir(dir)
        .env("REZOLUS_NO_OPEN", "1")
        .output()
        .unwrap()
}

/// The commands the refusal prints, one per indented line, with the
/// program name dropped.
fn advised(stderr: &str) -> Vec<Vec<String>> {
    stderr
        .lines()
        .filter_map(|l| l.strip_prefix("  rezolus "))
        .map(|l| l.split_whitespace().map(str::to_string).collect())
        .collect()
}

/// `view` with `args` reaches the point where it serves, rather than
/// exiting with an error.
fn view_serves(dir: &Path, args: &[String]) {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut child = Command::new(env!("CARGO_BIN_EXE_rezolus"))
        .args(args)
        .arg("--listen")
        .arg(format!("127.0.0.1:{port}"))
        .current_dir(dir)
        .env("REZOLUS_NO_OPEN", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = child.stderr.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            let _ = tx.send(line);
        }
    });
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut seen = Vec::new();
    loop {
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(line) if line.contains("Use your browser to view") => break,
            Ok(line) => seen.push(line),
            Err(_) => {
                let _ = child.kill();
                panic!("view {args:?} did not serve:\n{}", seen.join("\n"));
            }
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Run what the refusal of `view first second` advises: combine, describe,
/// then view with the selectors describe gives for `baseline` and
/// `experiment` (host labels).
fn follow_the_advice(dir: &Path, first: &str, second: &str, baseline: &str, experiment: &str) {
    let out = rezolus(dir, &["view", first, second]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    let commands = advised(&stderr);
    assert_eq!(commands.len(), 3, "{stderr}");

    let combine: Vec<&str> = commands[0].iter().map(String::as_str).collect();
    assert_eq!(
        combine,
        ["recording", "combine", first, second, "-o", "both.dendro"],
        "{stderr}"
    );
    let out = rezolus(dir, &combine);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let describe: Vec<&str> = commands[1].iter().map(String::as_str).collect();
    let out = rezolus(dir, &describe);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let listing = String::from_utf8_lossy(&out.stdout).to_string();
    for host in [baseline, experiment] {
        let selector = format!("--recording host={host}");
        assert!(listing.contains(&selector), "{selector} in:\n{listing}");
    }

    let view: Vec<String> = commands[2]
        .iter()
        .scan(None, |flag: &mut Option<String>, word| {
            let out = match (flag.as_deref(), word.as_str()) {
                (Some("--baseline"), "KEY=VALUE") => format!("host={baseline}"),
                (Some("--experiment"), "KEY=VALUE") => format!("host={experiment}"),
                _ => word.clone(),
            };
            *flag = Some(word.clone());
            Some(out)
        })
        .collect();
    assert_eq!(view[0], "view", "{stderr}");
    view_serves(dir, &view);
}

#[test]
fn a_multi_recording_experiment_is_refused_with_commands_that_work() {
    let dir = tempfile::tempdir().unwrap();
    rez::fixtures::multi_recording(&dir.path().join("one.dendro"), &[("redis", "web-01")]);
    rez::fixtures::multi_recording(
        &dir.path().join("two.dendro"),
        &[("redis", "web-02"), ("valkey", "web-03")],
    );
    follow_the_advice(dir.path(), "one.dendro", "two.dendro", "web-01", "web-03");
}

#[test]
fn a_multi_recording_baseline_is_refused_with_commands_that_work() {
    let dir = tempfile::tempdir().unwrap();
    rez::fixtures::multi_recording(
        &dir.path().join("two.dendro"),
        &[("redis", "web-02"), ("valkey", "web-03")],
    );
    rez::fixtures::multi_recording(&dir.path().join("one.dendro"), &[("redis", "web-01")]);
    follow_the_advice(dir.path(), "two.dendro", "one.dendro", "web-02", "web-01");
}
