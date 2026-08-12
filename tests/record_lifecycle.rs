//! Process-level regression tests for `rezolus record`'s stop path.
//!
//! The exit code a supervisor sees is only observable from outside the
//! process, so these drive the real binary against a stand-in agent rather
//! than calling into `recorder::run` (which owns `std::process::exit`).

#![cfg(unix)]

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;
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

/// A failed `.rez` finalize must exit non-zero.
///
/// Regression: the finalize `Err` arm printed `error saving .rez archive` and
/// returned without setting `recording_failed`, so `run()` fell through to
/// exit 0. Because the `.partial` design deliberately never touches a
/// pre-existing output, that also left the PREVIOUS run's `out.rez` in place —
/// `rezolus record -o out.rez && analyze out.rez` succeeded on stale data.
///
/// The deterministic failure seam is the final rename: the output path is an
/// existing directory, so renaming `<output>.partial` onto it cannot succeed
/// (the same class as an ENOSPC or EACCES at the end of a long capture).
#[test]
fn failed_rez_finalize_exits_nonzero() {
    let port = spawn_fake_agent();
    let dir = tempfile::tempdir().expect("failed to create a temp dir");

    // The output path exists as a directory: `rename(file, dir)` always fails.
    let output = dir.path().join("out.rez");
    std::fs::create_dir(&output).unwrap();
    // Stand-in for a previous run's archive, to show it survives the failure.
    std::fs::write(output.join("stale"), b"previous run").unwrap();

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
        stderr.contains("error saving .rez archive"),
        "expected a finalize failure, got stderr:\n{stderr}"
    );
    assert!(
        !out.status.success(),
        "a failed .rez finalize must not exit 0 (status {:?})\nstderr:\n{stderr}",
        out.status.code()
    );
    // The recovery artifact is left behind rather than discarded.
    assert!(
        dir.path().join("out.rez.partial").exists(),
        "the .partial should survive a failed finalize as the recovery artifact"
    );
}
