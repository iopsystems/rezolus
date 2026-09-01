//! Process-level regression tests for `rezolus record`'s stop path.
//!
//! Both properties here are only observable from outside the process — the
//! exit code a supervisor sees, and the wall time between SIGTERM and exit —
//! so they are tested by driving the real binary against a stand-in agent
//! rather than by calling into `recorder::run` (which owns `std::process::exit`).
//!
//! Unix only: the SIGTERM case has no meaning elsewhere.

#![cfg(unix)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

use metriken_exposition::{Counter, Snapshot, SnapshotV2};

/// One msgpack snapshot carrying a single counter attributed to the `fake`
/// sampler. `tick` varies the value so consecutive scrapes are not deduped
/// into a single row by the `.rez` writer.
fn snapshot_bytes(tick: u64) -> Vec<u8> {
    let mut metadata = HashMap::new();
    metadata.insert("sampler".to_string(), "fake".to_string());
    let snapshot = Snapshot::V2(SnapshotV2 {
        systemtime: SystemTime::now(),
        duration: Duration::from_millis(1),
        metadata: HashMap::new(),
        counters: vec![Counter::new("fake_ops".to_string(), tick, metadata)],
        gauges: Vec::new(),
        histograms: Vec::new(),
    });
    rmp_serde::encode::to_vec(&snapshot).expect("failed to encode the fake snapshot")
}

/// Minimal stand-in for the agent's msgpack endpoint: answers
/// `/metrics/binary` with a snapshot and 404s the optional metadata routes
/// (`/systeminfo`, `/metrics/descriptions`, `/samplers`), which the recorder
/// treats as absent. Returns the bound port; the accept loop is detached and
/// dies with the test process.
fn spawn_fake_agent() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("failed to bind the fake agent");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let mut tick = 0u64;
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = [0u8; 8192];
            let Ok(n) = stream.read(&mut buf) else {
                continue;
            };
            if n == 0 {
                continue;
            }
            let req = String::from_utf8_lossy(&buf[..n]);
            let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
            if path.starts_with("/metrics/binary") {
                tick += 1;
                let body = snapshot_bytes(tick);
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/msgpack\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&body);
            } else {
                let _ = stream.write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
            }
            let _ = stream.flush();
        }
    });
    port
}

/// A recorder that cannot create its output must exit non-zero.
///
/// Regression this guards: a failure on the output path printed an error and
/// returned without setting `recording_failed`, so `run()` fell through to
/// exit 0 — and `rezolus record -o out.rez && analyze out.rez` then succeeded
/// on whatever was already at that path.
///
/// There is no `.partial` any more: the recorder claims the output path with
/// `O_EXCL` before the first tick, so an unusable path fails at startup rather
/// than at finalize — and it must fail *loudly*. A supervisor or a
/// `record && analyze` pipeline can only see the exit code, and exiting 0 here
/// would hand the next command whatever was already at that path.
///
/// This replaces a sibling test that drove `--rez-version 2` and exercised the
/// rename of `<output>.partial` onto an unusable path. That mechanism is gone
/// with the tar writer; this covers the same regression on the path that
/// remains.
#[test]
fn v3_cannot_create_its_output_and_exits_nonzero() {
    let port = spawn_fake_agent();
    let dir = tempfile::tempdir().expect("failed to create a temp dir");

    // An existing directory: `create_new` on it cannot succeed.
    let output = dir.path().join("out.rez");
    std::fs::create_dir(&output).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_rezolus"))
        .arg("record")
        .arg("--url")
        .arg(format!("http://127.0.0.1:{port}"))
        .arg("-o")
        .arg(&output)
        .arg("--interval")
        .arg("100ms")
        .arg("--duration")
        .arg("1s")
        .output()
        .expect("failed to run rezolus record");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("failed to start the .rez recording"),
        "expected a startup failure, got stderr:\n{stderr}"
    );
    assert!(
        !out.status.success(),
        "a .rez that could not be created must not exit 0 (status {:?})\nstderr:\n{stderr}",
        out.status.code()
    );
    // No staging file is invented on the way past: v3 has none.
    assert!(!dir.path().join("out.rez.partial").exists());
}

/// A clean `.rez` recording is a v3 (SQLite) archive by default, and
/// `parquet metadata` describes it as one.
///
/// This is the end-to-end check that the default actually changed: the format
/// is decided inside `run()`, which owns `std::process::exit`, so the only
/// place it is observable is the file the real binary leaves behind.
#[test]
fn a_default_rez_recording_is_v3_and_describes_itself() {
    let port = spawn_fake_agent();
    let dir = tempfile::tempdir().expect("failed to create a temp dir");
    let output = dir.path().join("out.rez");

    let out = Command::new(env!("CARGO_BIN_EXE_rezolus"))
        .arg("record")
        .arg("--url")
        .arg(format!("http://127.0.0.1:{port}"))
        .arg("-o")
        .arg(&output)
        .arg("--interval")
        .arg("100ms")
        .arg("--duration")
        .arg("1s")
        .output()
        .expect("failed to run rezolus record");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "a clean recording must exit 0 (status {:?})\nstderr:\n{stderr}",
        out.status.code()
    );

    // SQLite's file header — the container, checked without linking the crate.
    let head = std::fs::read(&output).expect("the recording should exist");
    assert!(
        head.starts_with(b"SQLite format 3\0"),
        "the default .rez must be the v3 SQLite container"
    );
    assert!(
        !dir.path().join("out.rez.partial").exists(),
        "v3 stages nothing"
    );

    let described = Command::new(env!("CARGO_BIN_EXE_rezolus"))
        .arg("parquet")
        .arg("metadata")
        .arg("-i")
        .arg(&output)
        .output()
        .expect("failed to run rezolus parquet metadata");
    let stdout = String::from_utf8_lossy(&described.stdout);
    assert!(
        described.status.success(),
        "metadata must describe a v3 archive\nstdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&described.stderr)
    );
    assert!(stdout.contains(".rez archive v3"), "{stdout}");
    assert!(
        stdout.contains("fake"),
        "the recorded sampler must be listed: {stdout}"
    );
    assert!(
        !stdout.contains("not cleanly finalized"),
        "a clean stop finalizes: {stdout}"
    );
}

/// SIGTERM → exit must not be bounded by `--interval`.
///
/// Regression: `STATE` was only re-read at the top of the recording loop and
/// `interval.tick()` was an uninterruptible await, so a clean stop cost up to
/// one full interval (measured: 27.2s at `--interval 30s`). Docker's default
/// stop grace is 10s, so `docker stop` SIGKILLed the recorder before it ever
/// noticed, dropping every unsealed `.rez` segment.
#[test]
fn sigterm_exits_promptly_at_a_long_interval() {
    let port = spawn_fake_agent();
    let dir = tempfile::tempdir().expect("failed to create a temp dir");

    let mut child = Command::new(env!("CARGO_BIN_EXE_rezolus"))
        .arg("record")
        .arg("--url")
        .arg(format!("http://127.0.0.1:{port}"))
        .arg("-o")
        .arg(dir.path().join("out.rez"))
        // Long enough that the first tick cannot land during the test: any
        // prompt exit is the shutdown path, not a coincidental tick.
        .arg("--interval")
        .arg("30s")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to run rezolus record");

    // Gate on the recorder's own "entering the loop" log rather than a sleep:
    // signalling before `ctrlc::set_handler` runs would kill the process by the
    // default SIGTERM disposition and pass this test for the wrong reason.
    let mut log = BufReader::new(child.stderr.take().expect("stderr was piped"));
    let startup_deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let mut line = String::new();
        assert!(
            log.read_line(&mut line).unwrap_or(0) > 0,
            "rezolus record exited during startup"
        );
        if line.contains("recording metrics") {
            break;
        }
        assert!(
            Instant::now() < startup_deadline,
            "timed out waiting for rezolus record to start recording"
        );
    }
    // Drain the rest so the child can never block on a full stderr pipe.
    std::thread::spawn(move || {
        let mut sink = Vec::new();
        let _ = log.read_to_end(&mut sink);
    });
    // Settle into the tick wait, which is the await this test is about.
    std::thread::sleep(Duration::from_millis(250));

    let sent = Instant::now();
    // SAFETY: `kill` on a pid this process owns and has not yet reaped.
    unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };

    // Poll rather than `wait()` so a regression fails the assertion instead of
    // hanging the suite for a full interval.
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        match child.try_wait().expect("failed to poll rezolus record") {
            Some(status) => break status,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("rezolus record did not exit within 20s of SIGTERM");
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    let elapsed = sent.elapsed();

    // A signalled death is the default disposition, i.e. the handler never ran
    // — that is a fast exit, but not the clean stop this test asserts.
    assert!(
        status.code().is_some(),
        "rezolus record was killed by a signal ({status:?}); the clean-stop path did not run"
    );
    // Comfortably inside docker's 10s default grace, and far below the 30s
    // interval that used to bound this.
    assert!(
        elapsed < Duration::from_secs(5),
        "SIGTERM -> exit took {elapsed:?}; it must not be bounded by --interval"
    );
}

/// A `--duration` window still takes every sample it used to.
///
/// Guards the branch order in the tick `select!`: the stop deadline was added
/// as a second wake-up source, and if it outranked the tick then a window that
/// is a whole number of intervals — the common case — would lose its final
/// sample and could even finish with nothing recorded.
#[test]
fn a_whole_number_of_intervals_still_records_and_stops_on_time() {
    let port = spawn_fake_agent();
    let dir = tempfile::tempdir().expect("failed to create a temp dir");
    let output = dir.path().join("out.rez");

    let started = Instant::now();
    let out = Command::new(env!("CARGO_BIN_EXE_rezolus"))
        .arg("record")
        .arg("--url")
        .arg(format!("http://127.0.0.1:{port}"))
        .arg("-o")
        .arg(&output)
        .arg("--interval")
        .arg("200ms")
        .arg("--duration")
        .arg("2s")
        .output()
        .expect("failed to run rezolus record");
    let elapsed = started.elapsed();

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "a clean --duration recording must exit 0 (status {:?})\nstderr:\n{stderr}",
        out.status.code()
    );
    assert!(output.exists(), "the .rez archive should have been written");
    assert!(
        elapsed < Duration::from_secs(10),
        "--duration 2s took {elapsed:?}"
    );
}

/// `rezolus view` must refuse a recording selector it cannot resolve, with a
/// message and a non-zero exit — never a panic, and never by falling back to
/// some other recording.
///
/// End-to-end through the real binary because that is where the two failure
/// modes live: `Config::try_from` errors used to reach `main` as an
/// `.expect()` (a backtrace in answer to a typo), and a resolution failure
/// exits from inside `init_file_mode_rez`, which in-process tests cannot
/// observe. It records a genuine two-recording archive first, so the listing
/// it prints is one a real capture produces.
///
/// Only the refusal paths are driven: a selector that RESOLVES starts a web
/// server and blocks, which is covered by the in-process tests in
/// `src/viewer/mod.rs`.
#[test]
fn view_refuses_a_recording_selector_it_cannot_resolve() {
    let a = spawn_fake_agent();
    let b = spawn_fake_agent();
    let dir = tempfile::tempdir().expect("failed to create a temp dir");
    let output = dir.path().join("fleet.rez");

    let out = Command::new(env!("CARGO_BIN_EXE_rezolus"))
        .arg("record")
        .arg("--endpoint")
        .arg(format!("http://127.0.0.1:{a},source=redis"))
        .arg("--endpoint")
        .arg(format!("http://127.0.0.1:{b},source=valkey"))
        .arg("-o")
        .arg(&output)
        .arg("--interval")
        .arg("100ms")
        .arg("--duration")
        .arg("1s")
        .output()
        .expect("failed to run rezolus record");
    assert!(
        out.status.success(),
        "recording two endpoints must exit 0\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let view = |args: &[&str]| -> (Option<i32>, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_rezolus"))
            .arg("view")
            .arg(&output)
            .args(args)
            .output()
            .expect("failed to run rezolus view");
        (
            out.status.code(),
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        )
    };

    // A selector that names nothing: refused, listing what the archive holds.
    let (code, text) = view(&["--baseline", "source=nope"]);
    assert_eq!(code, Some(1), "a dead selector must exit non-zero: {text}");
    assert!(
        text.contains("source=redis") && text.contains("source=valkey"),
        "the refusal must list the archive's recordings: {text}"
    );
    assert!(
        !text.contains("panicked"),
        "a dead selector is a user error, not a crash: {text}"
    );

    // A malformed pair: refused before anything is opened, by name.
    let (code, text) = view(&["--baseline", "redis"]);
    assert_eq!(code, Some(2), "a malformed pair must exit non-zero: {text}");
    assert!(
        text.contains("--baseline") && text.contains("key=value"),
        "the message must say what the flag expects: {text}"
    );
    assert!(
        !text.contains("panicked"),
        "a typo must not produce a backtrace: {text}"
    );
}
