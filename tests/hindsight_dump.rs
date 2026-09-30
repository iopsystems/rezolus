//! What a hindsight dump promises while the buffer keeps recording, tested
//! against the real daemon.
//!
//! The unit tests in `src/hindsight/buffer.rs` pin the dump's snapshot
//! semantics at the container level, where the seams are reachable. These pin
//! the same properties end to end — a real `rezolus hindsight` process, a real
//! HTTP dump, a real `.rez` on disk — because the claims are about two
//! *connections* running at once (the daemon's writer and the dump's reader),
//! and a single-threaded unit test can only approximate that.
//!
//! Everything here runs at **10 Hz with two- or eight-row segments**, so a
//! seal lands every 200-800 ms and each test costs a few seconds of wall clock
//! rather than the ~7 minutes the production 4096-row policy would need to
//! close one segment. `[general] segment_rows` is what makes that
//! configurable.
//!
//! **All three trigger paths are exercised, because they were not always
//! equivalent.** `GET /dump` has always built its copy on a blocking task
//! while the recording loop kept ticking. `POST /dump/file` and the SIGHUP
//! capture used to run the dump *inside* the recording loop — the HTTP request
//! was handled by the loop's own `select!`, whose handler body awaited the
//! dump, and SIGHUP dumped in the loop body — so the tick branch was not
//! polled until the copy finished and the recording was paused for its
//! duration. `MissedTickBehavior::Skip` then *discarded* the ticks that went
//! by, so they were lost rather than late. Measured over a one-second window
//! of back-to-back dumps at 10 Hz, ten ticks due, with only
//! `src/hindsight/mod.rs` reverted: **`GET /dump` 14 ticks / 6 seals,
//! `POST /dump/file` 2 / 1, SIGHUP 1 / 1** — against **12 / 5, 13 / 6 and
//! 12 / 5** once every dump runs off the loop. That contradicted what
//! `rezolus hindsight --help` promises ("a snapshot is a consistent
//! point-in-time copy taken without pausing the recording") and scaled with
//! buffer size, so it was worst on the large buffers an incident is captured
//! from. The three tests below are one body run over each trigger in turn.
//!
//! Reading the archives directly with `rusqlite` and `parquet` rather than
//! through the crate: `rezolus` has no library target, so an integration test
//! sees the binary and the dependencies and nothing else. That turns out to be
//! the right constraint anyway — these assertions are about bytes on disk.
//!
//! Unix only, for the same reason as `record_lifecycle.rs`: the daemon is
//! killed by signal at the end of each test.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use rusqlite::{Connection, OpenFlags};

/// The scrape interval every test here runs at.
const INTERVAL: Duration = Duration::from_millis(100);

/// How many counters the stand-in agent reports when a test needs a dump to
/// cost real time — a dump of a one-counter buffer finishes in single-digit
/// milliseconds, which is below the resolution of everything worth measuring
/// about it. `sealing_continues_while_dumps_are_in_flight` is provably vacuous
/// without this; the `-wal` measurement needs it to reach a checkpoint at all.
/// The two tests that only care about row identity use a single counter and
/// run in ~2 s.
const WIDE: usize = 2000;

/// The agent's width for the SIGHUP test, whose whole-file capture is several
/// times faster than a ranged dump; see that test for the calibration.
const WIDE_SIGHUP: usize = 4000;

// ---------------------------------------------------------------------------
// The stand-in agent, as in `record_lifecycle.rs`
// ---------------------------------------------------------------------------

/// The table the fake agent's group is recorded as: `<sampler>/<group>`.
const TABLE: &str = "fake/ops";

/// Minimal stand-in for a 6.0 agent, which hindsight records over its
/// replication stream: serves `/metrics/stream` (see [`serve_stream`]) and 404s
/// the optional metadata routes, which hindsight treats as absent. Returns the
/// bound port; the accept loop is detached and dies with the test process.
fn spawn_fake_agent(width: usize) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("failed to bind the fake agent");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            // A thread per connection: a subscription holds its connection
            // for the whole run.
            std::thread::spawn(move || {
                let mut buf = [0u8; 8192];
                let Ok(n) = stream.read(&mut buf) else {
                    return;
                };
                if n == 0 {
                    return;
                }
                let req = String::from_utf8_lossy(&buf[..n]);
                let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
                if path.starts_with("/metrics/stream") {
                    serve_stream(stream, &path, width);
                    return;
                }
                let _ = stream.write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
                let _ = stream.flush();
            });
        }
    });
    port
}

fn wall_ns() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
}

/// The agent's replication stream, as `/metrics/stream` serves it: the
/// preamble and a handshake, then one rows frame per requested interval
/// carrying `width` counters (`fake_ops_0`..) in the `fake/ops` acquisition
/// group, the schema on the first row only, until hindsight hangs up. Each
/// frame's values differ, so consecutive rows are not deduped.
fn serve_stream(mut stream: std::net::TcpStream, path: &str, width: usize) {
    use dendro::replicate::{wire, Frame};
    use metriken_exposition::{GroupSchema, GroupSnapshot, MetricDesc};

    let interval = path
        .split_once("interval=")
        .map(|(_, v)| v.split('&').next().unwrap_or(v).replace("%20", " "))
        .and_then(|v| humantime::parse_duration(&v).ok())
        .unwrap_or(INTERVAL);
    let head = "HTTP/1.1 200 OK\r\n\
                Content-Type: application/vnd.rezolus.replication.v1+dendro\r\n\
                Connection: close\r\n\r\n";
    let mut bytes = head.as_bytes().to_vec();
    wire::write_preamble(&mut bytes).unwrap();
    wire::encode_frame(
        &Frame::Handshake {
            source: 0,
            uuid: Some("fake-epoch".to_string()),
            labels: Default::default(),
            metadata: Default::default(),
            clock_anchor_wall_ns: wall_ns() as i64,
            complete: false,
        },
        &mut bytes,
    )
    .unwrap();
    if stream
        .write_all(&bytes)
        .and_then(|()| stream.flush())
        .is_err()
    {
        return;
    }

    let schema = GroupSchema {
        counters: (0..width)
            .map(|i| MetricDesc {
                name: format!("0x{i}"),
                metadata: [("metric".to_string(), format!("fake_ops_{i}"))]
                    .into_iter()
                    .collect(),
            })
            .collect(),
        gauges: Vec::new(),
        histograms: Vec::new(),
    };
    for seq in 0u64.. {
        std::thread::sleep(interval);
        let ts = wall_ns();
        let group = GroupSnapshot {
            name: TABLE.to_string(),
            schema_hash: schema.hash(),
            schema: Some(Arc::new(schema.clone())),
            window: Some(metriken::Window::new(ts - 1_000_000, ts)),
            counters: (0..width).map(|i| Some(seq + 1 + i as u64)).collect(),
            gauges: Vec::new(),
            histograms: Vec::new(),
        };
        let anchor = (seq == 0).then(|| (&schema).into());
        let row = rez::wal::encode_wal_group_row(&rez::wal::wal_group_row(&group, anchor)).unwrap();
        let mut bytes = Vec::new();
        wire::encode_frame(
            &Frame::Rows {
                source: 0,
                seq,
                index_state: dendro::replicate::NO_INDEX_STATE,
                rows: vec![dendro::archive::WalRow {
                    stream: TABLE.to_string(),
                    ts: ts as i64,
                    wall_offset: 0,
                    row,
                }],
            },
            &mut bytes,
        )
        .unwrap();
        if stream
            .write_all(&bytes)
            .and_then(|()| stream.flush())
            .is_err()
        {
            return;
        }
    }
}

/// A blocking HTTP client. `reqwest` is built here with `rustls-no-provider`,
/// so a provider has to be installed before the first client exists — `main.rs`
/// does the same thing at startup, and an integration test does not run
/// `main`. Installing is process-wide and only takes the first call, hence the
/// discarded result.
fn http_client() -> reqwest::blocking::Client {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
    reqwest::blocking::Client::builder()
        .http1_only()
        .build()
        .expect("failed to build an HTTP client")
}

// ---------------------------------------------------------------------------
// The daemon under test
// ---------------------------------------------------------------------------

/// A running `rezolus hindsight`, its HTTP endpoint, and the buffer file it is
/// writing. Killed on drop, so a failed assertion does not leave a daemon
/// scraping in the background.
struct Hindsight {
    child: Child,
    port: u16,
    /// The rolling buffer the daemon is writing — its private staging file,
    /// taken from the daemon's own startup log rather than guessed at.
    buffer: PathBuf,
    /// Where `POST /dump/file` and a SIGHUP capture write — `[general] output`.
    output: PathBuf,
    /// Every log line the daemon has written since startup. Kept rather than
    /// discarded because the SIGHUP path has no reply channel: "capture
    /// complete" on stderr is the only signal that one finished.
    log: Arc<Mutex<Vec<String>>>,
    _dir: tempfile::TempDir,
    client: reqwest::blocking::Client,
}

impl Drop for Hindsight {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Hindsight {
    /// Start the daemon against a fresh stand-in agent and wait until it is
    /// buffering.
    fn start(segment_rows: usize, width: usize) -> Self {
        let agent = spawn_fake_agent(width);
        Self::try_start(agent, segment_rows)
            .unwrap_or_else(|why| panic!("rezolus hindsight failed to come up: {why}"))
    }

    /// [`start`](Self::start), with `Err` carrying whatever the daemon wrote to
    /// stderr, so a failure explains itself instead of just reporting that the
    /// process is gone.
    ///
    /// The daemon binds port 0 and reports the port it got. The harness used to
    /// reserve a port, release it and hand it over, and the tests run in
    /// parallel, so in that gap another test's fake agent (which binds port 0)
    /// could be given the same port. The daemon then failed to bind and exited,
    /// and `ready` reached the fake agent instead: CI saw its empty 404 fail
    /// to parse as a `/status` body. With nothing reserved there is no gap.
    fn try_start(agent: u16, segment_rows: usize) -> Result<Self, String> {
        Self::try_start_as(agent, segment_rows, "rez")
    }

    /// [`try_start`](Self::try_start) with the output's extension, which
    /// picks the buffer's container: `rez` or `dendro`.
    fn try_start_as(agent: u16, segment_rows: usize, ext: &str) -> Result<Self, String> {
        let dir = tempfile::tempdir().expect("failed to create a temp dir");
        let config = dir.path().join("hindsight.toml");
        let output = dir.path().join(format!("snapshot.{ext}"));
        std::fs::write(
            &config,
            format!(
                "[general]\n\
                 interval = \"{}ms\"\n\
                 duration = \"15m\"\n\
                 source = \"127.0.0.1:{agent}\"\n\
                 output = \"{}\"\n\
                 buffer_dir = \"{}\"\n\
                 listen = \"127.0.0.1:0\"\n\
                 segment_rows = {segment_rows}\n\
                 [log]\n\
                 level = \"info\"\n",
                INTERVAL.as_millis(),
                output.display(),
                // Pinned to this test's temp dir. `buffer_dir` defaults to
                // $STATE_DIRECTORY or /var/lib/rezolus, neither of which a
                // test may write to — and setting it here is also what keeps
                // each test's buffer isolated from every other's.
                dir.path().display(),
            ),
        )
        .expect("failed to write the hindsight config");

        let mut child = Command::new(env!("CARGO_BIN_EXE_rezolus"))
            .arg("hindsight")
            .arg(&config)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to run rezolus hindsight");

        // The daemon logs the buffer path as it enters the loop. Gating on that
        // line rather than on a sleep gives the buffer path directly. It does
        // NOT mean the HTTP endpoint is up — the daemon logs this before it
        // spawns the listener — so `ready()` below waits for that separately.
        let mut log = BufReader::new(child.stderr.take().expect("stderr was piped"));
        let deadline = Instant::now() + Duration::from_secs(60);
        let buffer = loop {
            let mut line = String::new();
            if log.read_line(&mut line).unwrap_or(0) == 0 {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("exited during startup; last line: {line:?}"));
            }
            if let Some(rest) = line.split(" in ").nth(1) {
                if line.contains("buffering") {
                    break PathBuf::from(rest.trim());
                }
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err("timed out waiting for it to start buffering".to_string());
            }
        };
        // Drain the rest so the daemon can never block on a full stderr pipe,
        // keeping the lines: `capture_via_sighup` reads completion out of them.
        let lines: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        {
            let lines = Arc::clone(&lines);
            std::thread::spawn(move || {
                let mut line = String::new();
                while log.read_line(&mut line).unwrap_or(0) > 0 {
                    lines.lock().unwrap().push(std::mem::take(&mut line));
                }
            });
        }

        let mut h = Self {
            child,
            port: 0,
            buffer,
            output,
            log: lines,
            _dir: dir,
            client: http_client(),
        };
        h.ready()?;
        Ok(h)
    }

    /// Wait for the HTTP endpoint to come up, learn its port from the
    /// daemon's log, and wait for it to answer.
    ///
    /// Separate from the startup log because the daemon binds its listener
    /// AFTER logging "buffering".
    fn ready(&mut self) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(60);
        while self.port == 0 {
            if let Some(port) = self.bound_port() {
                self.port = port;
                break;
            }
            if self
                .child
                .try_wait()
                .expect("failed to poll the daemon")
                .is_some()
            {
                return Err(format!(
                    "exited before its HTTP endpoint came up: {}",
                    self.log_text()
                ));
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "timed out waiting for its HTTP endpoint to bind: {}",
                    self.log_text()
                ));
            }
            std::thread::sleep(INTERVAL / 4);
        }
        while self.try_status().is_none() {
            if self
                .child
                .try_wait()
                .expect("failed to poll the daemon")
                .is_some()
            {
                return Err(format!(
                    "exited before its HTTP endpoint on port {} answered: {}",
                    self.port,
                    self.log_text()
                ));
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "timed out waiting for its HTTP endpoint on port {}: {}",
                    self.port,
                    self.log_text()
                ));
            }
            std::thread::sleep(INTERVAL / 4);
        }
        Ok(())
    }

    /// The port from the daemon's "HTTP endpoint listening on ADDR" line, once
    /// it has written one.
    fn bound_port(&self) -> Option<u16> {
        let log = self.log.lock().unwrap();
        log.iter().find_map(|line| {
            let addr = line.split("HTTP endpoint listening on ").nth(1)?;
            addr.trim()
                .parse::<std::net::SocketAddr>()
                .ok()
                .map(|a| a.port())
        })
    }

    /// Everything the daemon has written to stderr since startup, for failure
    /// messages.
    fn log_text(&self) -> String {
        self.log.lock().unwrap().join("").trim_end().to_string()
    }

    /// `/status`, or `None` if the endpoint could not be reached at all. A
    /// reply that arrives and does not parse is still a failure.
    fn try_status(&self) -> Option<Status> {
        let body = self
            .client
            .get(format!("http://127.0.0.1:{}/status", self.port))
            .send()
            .ok()?
            .text()
            .expect("GET /status body");
        Some(serde_json::from_str(&body).unwrap_or_else(|e| panic!("bad /status body {body}: {e}")))
    }

    fn status(&self) -> Status {
        self.try_status().expect("GET /status failed")
    }

    /// Download a dump over `GET /dump?start=0` and write it to `dest`.
    ///
    /// `?start=0` rather than a bare `/dump` on purpose: an unbounded dump is a
    /// whole-file `VACUUM INTO`, which carries the `wal` table across verbatim.
    /// A *ranged* dump is the path that selects segments and materializes the
    /// live tail into one, which is the boundary these tests are about.
    fn dump_to(&self, dest: &Path) -> Duration {
        let (elapsed, body) = self.dump_bytes();
        std::fs::write(dest, &body).expect("failed to save the dump");
        elapsed
    }

    /// `GET /dump?start=0`, checked to be a `.rez` and returned with how long
    /// the request took. [`Self::dump_to`] saves it; a test that only needs the
    /// dump to have happened can drop it, and so not spend time on a disk write
    /// that it would otherwise have to count as time between dumps.
    fn dump_bytes(&self) -> (Duration, bytes::Bytes) {
        let start = Instant::now();
        let body = self
            .client
            .get(format!("http://127.0.0.1:{}/dump?start=0", self.port))
            .send()
            .expect("GET /dump failed")
            .bytes()
            .expect("GET /dump body");
        let elapsed = start.elapsed();
        assert!(
            body.starts_with(b"SQLite format 3\0"),
            "a dump must be a v3 .rez ({} bytes)",
            body.len()
        );
        (elapsed, body)
    }

    /// Dump into the daemon's own configured output file over
    /// `POST /dump/file?start=0`, returning how long the request took.
    ///
    /// The same `?start=0` as [`Self::dump_to`], and for the same reason: it is
    /// the ranged path, which selects segments and materializes the live tail
    /// rather than copying the whole file verbatim.
    fn dump_to_file(&self) -> Duration {
        let start = Instant::now();
        let response = self
            .client
            .post(format!("http://127.0.0.1:{}/dump/file?start=0", self.port))
            .send()
            .expect("POST /dump/file failed");
        let status = response.status();
        let body = response.text().expect("POST /dump/file body");
        let elapsed = start.elapsed();
        assert!(
            status.is_success(),
            "POST /dump/file returned {status}: {body}"
        );
        let json: serde_json::Value = serde_json::from_str(&body)
            .unwrap_or_else(|e| panic!("bad /dump/file body {body}: {e}"));
        assert!(
            json.get("error").is_none_or(|e| e.is_null()),
            "POST /dump/file reported an error: {body}"
        );
        assert!(
            json["rows"].as_u64().unwrap_or(0) > 0,
            "POST /dump/file wrote an empty dump: {body}"
        );
        elapsed
    }

    /// Trigger a capture the way an operator does — `systemctl kill -sHUP` —
    /// and block until the daemon logs that it finished, returning how long
    /// that took.
    ///
    /// The completion line is the only handle on this path: SIGHUP has no
    /// caller to reply to. Waiting for it also keeps the signal semantics
    /// straight, since a second signal *during* a capture means "terminate
    /// once it is done" rather than "capture again".
    fn capture_via_sighup(&self) -> Duration {
        let from = self.log.lock().unwrap().len();
        let start = Instant::now();
        self.signal(libc::SIGHUP);
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            {
                let log = self.log.lock().unwrap();
                if log[from..].iter().any(|l| l.contains("capture complete")) {
                    return start.elapsed();
                }
                assert!(
                    Instant::now() < deadline,
                    "timed out waiting for a SIGHUP capture to complete (the \
                     daemon may have exited); log since the signal: {:?}",
                    &log[from..]
                );
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Signal the daemon, without waiting for anything to come of it.
    fn signal(&self, sig: libc::c_int) {
        // SAFETY: `kill` on a pid this process owns and has not yet reaped.
        unsafe { libc::kill(self.child.id() as libc::pid_t, sig) };
    }

    fn log_has(&self, needle: &str) -> bool {
        self.log.lock().unwrap().iter().any(|l| l.contains(needle))
    }

    fn wait_for_log(&self, needle: &str, within: Duration) {
        let deadline = Instant::now() + within;
        while !self.log_has(needle) {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {needle:?} in the daemon's log: {:?}",
                self.log.lock().unwrap()
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Wait for the daemon to exit of its own accord, failing if it does not.
    fn wait_for_exit(&mut self, within: Duration) -> std::process::ExitStatus {
        let deadline = Instant::now() + within;
        loop {
            if let Some(status) = self.child.try_wait().expect("failed to poll the daemon") {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "rezolus hindsight did not exit within {within:?}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Block until `f` holds of the daemon's status, returning it. Every wait in
    /// this file is gated on observed buffer state rather than on a duration.
    fn wait_until(&self, what: &str, f: impl Fn(&Status) -> bool) -> Status {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let status = self.status();
            if f(&status) {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}; last status: {status:?}"
            );
            std::thread::sleep(INTERVAL / 4);
        }
    }
}

#[derive(Debug, serde::Deserialize)]
struct Status {
    ticks_recorded: u64,
    rows: u64,
    tables: Vec<TableStatus>,
}

#[derive(Debug, serde::Deserialize)]
struct TableStatus {
    sampler: String,
    segments: u64,
    live_wal_rows: u64,
}

impl Status {
    fn table(&self, sampler: &str) -> &TableStatus {
        self.tables
            .iter()
            .find(|t| t.sampler == sampler)
            .unwrap_or_else(|| panic!("no {sampler} table in {self:?}"))
    }
    fn segments(&self, sampler: &str) -> u64 {
        self.tables
            .iter()
            .find(|t| t.sampler == sampler)
            .map_or(0, |t| t.segments)
    }
}

// ---------------------------------------------------------------------------
// Reading a `.rez` v3 from outside the crate
// ---------------------------------------------------------------------------

struct Segment {
    rows: u64,
    first_ts: u64,
    last_ts: u64,
    /// The `timestamp` column decoded from the segment's parquet BLOB — the
    /// actual rows, not the catalog's summary of them.
    timestamps: Vec<u64>,
}

struct Archive {
    segments: Vec<Segment>,
    /// Every row in the `wal` table, oldest first.
    wal: Vec<u64>,
}

impl Archive {
    /// Every sealed row stamp, in seq order.
    fn sealed(&self) -> Vec<u64> {
        self.segments
            .iter()
            .flat_map(|s| s.timestamps.iter().copied())
            .collect()
    }

    /// The WAL rows a reader would splice on: those past the sealed watermark.
    /// This is `RezDb::live_wal`'s rule, restated here because an integration
    /// test cannot call it.
    fn live_wal(&self) -> Vec<u64> {
        let watermark = self.segments.iter().map(|s| s.last_ts).max().unwrap_or(0);
        self.wal
            .iter()
            .copied()
            .filter(|ts| *ts > watermark)
            .collect()
    }

    /// Every row stamp a reader would see, sealed then live, in order.
    fn timeline(&self) -> Vec<u64> {
        let mut out = self.sealed();
        out.extend(self.live_wal());
        out
    }
}

/// Read one sampler's segments and WAL rows out of a `.rez` v3 file, decoding
/// each segment's parquet so the assertions can talk about rows rather than
/// about the catalog's claims about rows.
fn read_rez(path: &Path, sampler: &str) -> Archive {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)
        .unwrap_or_else(|e| panic!("failed to open {}: {e}", path.display()));

    let mut stmt = conn
        .prepare(
            "SELECT rows, first_ts, last_ts, bytes FROM segments \
             WHERE sampler = ?1 ORDER BY recording_id, seq",
        )
        .expect("failed to prepare the segment query");
    let segments: Vec<Segment> = stmt
        .query_map([sampler], |row| {
            Ok((
                row.get::<_, i64>(0)? as u64,
                row.get::<_, i64>(1)? as u64,
                row.get::<_, i64>(2)? as u64,
                row.get::<_, Vec<u8>>(3)?,
            ))
        })
        .expect("failed to query segments")
        .map(|r| {
            let (rows, first_ts, last_ts, bytes) = r.expect("failed to read a segment");
            Segment {
                rows,
                first_ts,
                last_ts,
                timestamps: parquet_timestamps(&bytes),
            }
        })
        .collect();

    let mut stmt = conn
        .prepare("SELECT ts FROM wal WHERE sampler = ?1 ORDER BY ts")
        .expect("failed to prepare the WAL query");
    let wal = stmt
        .query_map([sampler], |row| Ok(row.get::<_, i64>(0)? as u64))
        .expect("failed to query the WAL")
        .map(|r| r.expect("failed to read a WAL row"))
        .collect();

    Archive { segments, wal }
}

/// The `timestamp` column of a segment's parquet BLOB.
fn parquet_timestamps(bytes: &[u8]) -> Vec<u64> {
    use arrow::array::UInt64Array;
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

    let reader = ParquetRecordBatchReaderBuilder::try_new(bytes::Bytes::copy_from_slice(bytes))
        .expect("a segment BLOB must be parquet")
        .build()
        .expect("a segment BLOB must be readable parquet");
    let mut out = Vec::new();
    for batch in reader {
        let batch = batch.expect("a segment batch must decode");
        let schema = batch.schema();
        for i in 0..batch.num_columns() {
            if schema.field(i).name() == "timestamp" {
                let a = batch
                    .column(i)
                    .as_any()
                    .downcast_ref::<UInt64Array>()
                    .expect("timestamp is u64");
                out.extend((0..a.len()).map(|r| a.value(r)));
            }
        }
    }
    out
}

/// Assert a slice of row stamps is strictly increasing — which is also the
/// no-duplicates check, since a repeat is not an increase.
fn assert_strictly_increasing(what: &str, stamps: &[u64]) {
    if let Some(i) = (1..stamps.len()).find(|&i| stamps[i - 1] >= stamps[i]) {
        panic!(
            "{what}: row {i} does not advance ({} then {}) — a duplicate or an \
             out-of-order row at the segment/WAL seam. Full timeline: {stamps:?}",
            stamps[i - 1],
            stamps[i]
        );
    }
}

// ---------------------------------------------------------------------------
// The tests
// ---------------------------------------------------------------------------

/// A dump must not stop the recording — that is the whole reason hindsight can
/// dump at all, and why there is no eviction pause anywhere in the daemon.
///
/// The buffer's writer and a dump are two SQLite connections on one file, and
/// nothing coordinates them: no lock of ours, no quiesce, no "hold off sealing
/// while a copy is in flight". This drives dumps back to back for a second and
/// asserts the daemon ticked and sealed straight through them, at full rate.
///
/// Run once per trigger path, because the three are separate code and only one
/// of them was ever right: `GET /dump` always ran off the loop, while
/// `POST /dump/file` was awaited inside the recording loop's `select!` and the
/// SIGHUP capture ran in the loop body. Both of those held the tick branch
/// unpolled for the length of the copy, and `MissedTickBehavior::Skip`
/// *discards* what they held off rather than deferring it — so the samples
/// were gone, not late.
///
/// **The fixture is the load-bearing part**, and it was calibrated rather than
/// guessed. A dump has to be slow enough that a stalled writer would visibly
/// lose ticks: at 10 Hz the tick budget is 100 ms, so a dump that finishes in
/// 8 ms cannot cost a tick no matter what it locks — and measurably does not,
/// even under `BEGIN IMMEDIATE` or an explicit mutex between the dump and the
/// seal (both tried; both left this test green with a one-counter agent). So
/// the stand-in agent here is 2,000 counters wide and the buffer is filled to a
/// dozen segments first, which puts a dump at ~250 ms — two and a half ticks —
/// and four of them back to back cover the whole window.
///
/// Segments, not merely ticks: a tick is a WAL insert, while a seal is the
/// heavier write (a segment BLOB in its own transaction) and the one that would
/// stall first if a reader could hold the writer off.
#[test]
fn sealing_continues_while_dumps_are_in_flight() {
    // The body is dropped rather than saved: writing it to disk is time
    // between dumps, which the fixture check below counts against coverage.
    sealing_continues_through("GET /dump", 12, WIDE, |h| h.dump_bytes().0);
}

/// The same claim over `POST /dump/file` — the path that writes the daemon's
/// configured output file, and the one an operator triggers remotely.
#[test]
fn sealing_continues_while_file_dumps_are_in_flight() {
    sealing_continues_through("POST /dump/file", 12, WIDE, |h| h.dump_to_file());
}

/// The same claim over SIGHUP — `systemctl kill -sHUP rezolus-hindsight`, the
/// trigger the README documents and the one with no HTTP endpoint involved.
///
/// One capture at a time, driven off the daemon's own completion line: a second
/// signal arriving *during* a capture means "terminate when it is done", so the
/// test may not simply fire signals back to back. The gap that leaves between
/// captures is a couple of milliseconds of polling, which the fixture assertion
/// on coverage still has to clear.
///
/// A *fatter* buffer than the HTTP paths use — 60 segments rather than 12 —
/// because a SIGHUP capture takes no time range, and an unbounded dump is a
/// whole-file `VACUUM INTO` that runs several times faster than the ranged copy
/// the HTTP tests take. Calibrated against the unfixed daemon, which is the
/// only calibration worth anything here: at 12 segments a capture finished in
/// ~66 ms, inside a single 100 ms tick, and this test passed on a daemon that
/// paused; at 40 it took ~220 ms and the daemon still got 3 of the 4 seals the
/// assertion demands through the gaps; at 60 it took ~330 ms and a paused
/// daemon was not close.
///
/// That calibration was for a V2 agent, whose table carries a window pair per
/// metric. Hindsight records the stream now, and a V3 group's table has one
/// window pair for the whole group, so at 2,000 counters a capture of 60
/// segments fell to 67–77 ms and the fixture check failed. At
/// [`WIDE_SIGHUP`] = 4,000 it took 169–215 ms over five runs. At 8,000 it
/// took ~410 ms, but seals then fell behind the ticks on one run in three.
#[test]
fn sealing_continues_while_a_sighup_capture_is_in_flight() {
    sealing_continues_through("SIGHUP", 60, WIDE_SIGHUP, |h| h.capture_via_sighup());
}

/// A second signal while a capture is in flight stops the daemon **after** that
/// capture, not instead of it — the contract the daemon states in its own log
/// ("waiting for capture to complete before exiting").
///
/// This is the shutdown half of taking captures off the recording loop. The
/// capture now runs on its own task while the loop keeps ticking, so "exit when
/// it is done" is a thing the loop has to wait for rather than a statement
/// about where it already is.
#[test]
fn a_second_signal_waits_for_the_capture_then_stops_the_daemon() {
    let mut h = Hindsight::start(2, WIDE);
    // A dozen segments puts a capture at ~70 ms, so the second signal lands
    // well inside it.
    h.wait_until(
        "a buffer big enough that a capture outlasts a signal",
        |s| s.segments(TABLE) >= 12,
    );

    h.signal(libc::SIGHUP);
    // Gated on the loop having STARTED the capture, not on the signal handler
    // having asked for one: a second signal that arrives in between is a
    // request to stop with nothing yet to wait for, and the daemon rightly
    // exits without capturing.
    h.wait_for_log("capture in progress", Duration::from_secs(10));
    h.signal(libc::SIGHUP);

    let status = h.wait_for_exit(Duration::from_secs(30));
    assert!(
        status.success(),
        "rezolus hindsight exited with {status} after a capture-then-terminate \
         signal pair"
    );
    h.wait_for_log("capture complete", Duration::from_secs(5));

    // A signal-triggered capture lands on a TIMESTAMPED sibling of `output`,
    // never on `output` itself — that path holds an operator's deliberate
    // `POST /dump/file` captures, and a restart must not overwrite one. So
    // the assertion is "a capture appeared beside it", not "output grew".
    let dir = h.output.parent().expect("output has a directory");
    let stem = h.output.file_stem().unwrap().to_string_lossy().into_owned();
    let captures: Vec<_> = std::fs::read_dir(dir)
        .expect("output directory is readable")
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .map(|n| {
                    let n = n.to_string_lossy();
                    n.starts_with(&format!("{stem}-")) && n != stem
                })
                .unwrap_or(false)
        })
        .collect();
    assert_eq!(
        captures.len(),
        1,
        "expected exactly one shutdown capture beside {}, found {captures:?}",
        h.output.display()
    );
    let bytes = std::fs::metadata(&captures[0])
        .map(|m| m.len())
        .unwrap_or(0);
    assert!(
        bytes > 0,
        "the capture the second signal waited for wrote nothing to {}",
        captures[0].display()
    );
    assert!(
        std::fs::metadata(&h.output).map(|m| m.len()).unwrap_or(0) == 0,
        "the shutdown capture must not have written to the output path itself"
    );
}

/// Rows per segment in this fixture. Sealing is row-driven, so this is also
/// the ratio the claim below is stated in: every `SEGMENT_ROWS` ticks that
/// reach the buffer must produce a segment.
const SEGMENT_ROWS: u64 = 2;

fn sealing_continues_through(
    trigger: &str,
    min_segments: u64,
    width: usize,
    mut dump: impl FnMut(&Hindsight) -> Duration,
) {
    let h = Hindsight::start(SEGMENT_ROWS as usize, width);

    let before = h.wait_until("a buffer big enough to make a dump slow", |s| {
        s.segments(TABLE) >= min_segments
    });

    // Back-to-back dumps. Each one opens its own connection, takes its own read
    // mark, and copies every segment in the buffer.
    //
    // Kept up until enough ticks have gone by to say anything about them, not
    // for a fixed span: the comparison below is seals against ticks, so the
    // window has to contain enough ticks for the ratio to mean something. How
    // long that takes is the host's business — a loaded one runs the loop at a
    // fraction of its configured rate — and waiting for the ticks measures the
    // mechanism where waiting for the clock measures the machine.
    const MIN_TICKS: u64 = 8;
    let window = Duration::from_secs(1);
    let deadline = Instant::now() + Duration::from_secs(60);
    let started = Instant::now();
    let mut dumps = 0u32;
    let mut busy = Duration::ZERO;
    let mut after = h.status();
    while (started.elapsed() < window || after.ticks_recorded - before.ticks_recorded < MIN_TICKS)
        && Instant::now() < deadline
    {
        busy += dump(&h);
        dumps += 1;
        // Only once the window is up: before then the loop continues whatever
        // the tick count, and a /status request is time no dump occupies,
        // which the coverage check below counts against the fixture. CI saw
        // dumps cover 913 ms of a 1,430 ms window when every iteration made one.
        if started.elapsed() >= window {
            after = h.status();
        }
    }
    let elapsed = started.elapsed();
    let (a, b) = (before.segments(TABLE), after.segments(TABLE));
    let ticks = after.ticks_recorded - before.ticks_recorded;

    println!(
        "MEASURED {trigger}: {ticks} ticks and {} seals over {elapsed:?} \
         ({dumps} dumps, {busy:?} spent inside one, {:?} each)",
        b - a,
        busy / dumps
    );

    // Fixture first: a window that dumps did not actually occupy proves
    // nothing, and neither does one they occupied with dumps too short to
    // matter. Both are stated, because both have silently degraded once.
    assert!(
        busy >= elapsed * 3 / 4,
        "fixture: {trigger} dumps covered only {busy:?} of the {elapsed:?} \
         window, so a blocked writer would have had it mostly to itself anyway"
    );
    assert!(
        busy / dumps >= INTERVAL,
        "fixture: the average {trigger} dump took {:?}, less than the \
         {INTERVAL:?} tick period — a writer blocked for the whole of every \
         dump would still make every tick, and this test would pass on a \
         daemon that pauses",
        busy / dumps
    );

    // The claim, in ticks rather than wall-clock. What must hold is that
    // sealing keeps pace with the rows that actually arrive; how many rows
    // arrive per second is a property of the machine, and a shared CI runner
    // reaches a fraction of the configured rate. Stating it as "~5 seals in a
    // second" measures the runner, and fails on a slow one while a genuinely
    // blocked writer on a fast one could still clear the bar.
    //
    // Still sharp: were dumps blocking the writer, ticks would go on arriving
    // and segments would not follow, which is exactly what this compares. The
    // `-1` allows the one partial segment left open at the window's edge.
    let due = ticks / SEGMENT_ROWS;
    assert!(
        b - a + 1 >= due,
        "sealing fell behind the ticks it was given during the {trigger} \
         dumps: {} segments sealed ({a} -> {b}) against {ticks} ticks at \
         {SEGMENT_ROWS} rows per segment, so ~{due} are due ({dumps} dumps, \
         {busy:?} spent inside one, over {elapsed:?})",
        b - a
    );
    // And the loop kept running while the dumps did. The window above is held
    // open until this many ticks land, so falling short means they stopped
    // arriving entirely — which, given the fixture established that dumps
    // covered most of the window, is a loop that stopped for its dumps.
    assert!(
        ticks >= MIN_TICKS,
        "the scrape loop stalled during the {trigger} dumps: only {ticks} \
         ticks in {elapsed:?} at a {INTERVAL:?} interval ({dumps} dumps, \
         {busy:?} spent inside one)"
    );
}

/// A dump is the buffer's timeline **cut at its snapshot**, with whatever was
/// live in the WAL at that moment sealed into a tail segment — every row on
/// exactly one side of the cut, once in the dump and once in the source, and
/// nothing from after the cut.
///
/// Three claims, and they only mean anything together:
///
/// - **Point in time.** The buffer keeps sealing while the copy runs; segments
///   it seals afterwards are not in the dump. Asserted as a fact about the two
///   files rather than about the clock: the source is shown holding segments
///   the dump does not, all of them past the dump's last row.
/// - **No loss.** The dump is not merely a *subset* of the source truncated
///   somewhere convenient — it is the source's timeline exactly, row for row,
///   up to the cut.
/// - **No duplication.** A ranged dump copies no WAL rows; it materializes the
///   live tail into a segment (metadata rides on the first WAL row that
///   mentions a metric, so a raw slice of WAL rows would arrive without
///   labels). Those same rows are still in the source's WAL at that moment and
///   the source will later seal them into a segment of its own. Two files, two
///   representations, one row each — and neither file may hold one twice.
///
/// Timed off the buffer's own reported WAL depth rather than off the clock: the
/// dump is taken when `/status` says 1–2 rows are live, which puts the next
/// seal several ticks away. That read happens *before* the dump though, and the
/// buffer keeps ingesting in between, so the phase it establishes can be lost —
/// whether the tail really is partial is therefore checked against the dump
/// itself, and a dump that missed is redrawn rather than failed.
#[test]
fn a_dump_cuts_the_timeline_at_its_snapshot_and_seals_the_live_tail() {
    // Eight-row segments at 10 Hz: a seal every ~800 ms, so catching the buffer
    // with 1-2 live WAL rows leaves ~600 ms before the next one.
    const SEGMENT_ROWS: u64 = 8;
    // How many dumps to take before giving up on catching a partial tail.
    //
    // The WAL depth cycles 0 -> SEGMENT_ROWS every ~800 ms, and `/status` is
    // read *before* the dump, so a stall between the two requests loses the
    // phase that read established. Land in the tick where the buffer has just
    // sealed and the dump's last segment is a full one with nothing live
    // behind it — roughly one tick in eight, which is what a loaded CI runner
    // hit. Each dump is an independent draw of that phase and costs
    // single-digit milliseconds against this one-counter buffer, so redraw
    // rather than fail the run: the same reasoning as `Hindsight::start`
    // retrying the lost-port race.
    const DUMP_ATTEMPTS: usize = 5;

    let h = Hindsight::start(SEGMENT_ROWS as usize, 1);
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("dump.rez");

    let mut missed = Vec::new();
    let (dumped, live_before, live_after) = loop {
        // Wait for a buffer that has sealed AND is part-way into its next
        // segment.
        let at_dump = h.wait_until("a partly-filled WAL over sealed segments", |s| {
            s.segments(TABLE) >= 2 && (1..=2).contains(&s.table(TABLE).live_wal_rows)
        });
        let live_before = at_dump.table(TABLE).live_wal_rows;
        let segments_before = at_dump.segments(TABLE);
        h.dump_to(&dest);
        // Read /status again AFTER the dump so the live-row count at the moment
        // the snapshot was taken is BRACKETED rather than guessed. The buffer is
        // still ingesting at 10 Hz, so any fixed tolerance on `live_before` is a
        // bet on how long the dump takes — and on a loaded CI runner that bet
        // loses (observed: 1 row before, a 7-row tail).
        let after = h.status();
        let live_after = after.table(TABLE).live_wal_rows;
        // The bracket holds only if nothing sealed between the two reads.
        // Retention here is 15 minutes, so the segment count only rises: a
        // change means a seal emptied the WAL in between, and then
        // `live_after` counts rows written since that seal, not the rows the
        // dump saw. Comparing the row counts cannot tell: a seal followed by
        // new rows can leave `live_after >= live_before` (observed: 2 before,
        // 5 after, a 1-row tail). So that draw is taken again, like a dump
        // that landed on a seal boundary.
        let sealed_between = after.segments(TABLE) != segments_before;

        let dumped = read_rez(&dest, TABLE);
        // True of every dump taken here, not only the one that is kept.
        assert!(
            dumped.wal.is_empty(),
            "a ranged dump copies no WAL rows — the tail is materialized into a \
             segment instead, so that its metrics keep their labels: {:?}",
            dumped.wal
        );
        let tail_rows = dumped
            .segments
            .last()
            .expect("the dump must hold at least the tail")
            .rows;
        // Asked of the dump in hand rather than of the `/status` read that
        // preceded it. A full last segment means the snapshot landed on a seal
        // boundary with no live tail to carry, and every claim below would hold
        // vacuously — so that dump is discarded and the phase drawn again.
        if tail_rows < SEGMENT_ROWS && !sealed_between {
            break (dumped, live_before, live_after);
        }
        missed.push(if sealed_between {
            format!("{tail_rows} rows, sealed between the /status reads")
        } else {
            format!("{tail_rows} rows, a full segment")
        });
        assert!(
            missed.len() < DUMP_ATTEMPTS,
            "fixture: {DUMP_ATTEMPTS} dumps in a row were unusable — {missed:?}. \
             One is the phase race; this many means the buffer is sealing \
             between the /status reads and every snapshot the test takes"
        );
    };

    let tail = dumped
        .segments
        .last()
        .expect("the dump must hold at least the tail");
    // No seal landed between the two reads (the loop redraws when one did),
    // so the WAL only grew across the dump and the tail it captured lies
    // between them.
    assert!(
        (live_before..=live_after).contains(&tail.rows),
        "the tail must be the rows that were live in the WAL when the dump \
         was taken: /status said {live_before} live rows before the dump and \
         {live_after} after, so the tail should be in that range — it holds {}",
        tail.rows
    );
    assert_eq!(
        tail.timestamps.len(),
        tail.rows as usize,
        "the tail's parquet must hold the rows its catalog row claims"
    );

    // Once in the dump: one strictly increasing timeline across the seam, with
    // nothing repeated where the last sealed segment meets the tail.
    let dump_timeline = dumped.timeline();
    assert_strictly_increasing("the dump", &dump_timeline);
    let seam = dumped.segments[dumped.segments.len() - 2].last_ts;
    assert!(
        seam < tail.first_ts,
        "the dump's tail overlaps the segment before it: {seam} then {}",
        tail.first_ts
    );

    // Once in the source: wait for the buffer to seal the rows that were live
    // AND to seal at least one segment past them, then find the tail's rows in
    // a sealed segment and NOT in the live WAL.
    h.wait_until("the source to seal past the dump", |s| {
        s.segments(TABLE) > dumped.segments.len() as u64
    });
    let source = read_rez(&h.buffer, TABLE);
    let source_timeline = source.timeline();
    assert_strictly_increasing("the source", &source_timeline);

    let sealed = source.sealed();
    let live = source.live_wal();
    for ts in &tail.timestamps {
        assert_eq!(
            sealed.iter().filter(|s| *s == ts).count(),
            1,
            "row {ts} — live in the WAL when the dump was taken — must appear \
             exactly once among the source's sealed rows"
        );
        assert!(
            !live.contains(ts),
            "row {ts} is both sealed and still live in the source: a reader \
             would splice it in twice"
        );
    }

    // No loss either: the dump is the source's timeline exactly, row for row,
    // up to the cut. A dump that dropped the live tail — or that carried it
    // twice — fails here even though every row in it is a real row.
    let cut = *dump_timeline.last().unwrap();
    assert_eq!(
        source_timeline
            .iter()
            .copied()
            .take_while(|ts| *ts <= cut)
            .collect::<Vec<_>>(),
        dump_timeline,
        "the dump must be exactly the source's timeline up to the moment it \
         was taken — no row dropped at the boundary, none invented"
    );

    // And it stops there. The source went on recording and sealing; none of
    // that reached the dump, and the dump's file on disk did not acquire it
    // afterwards either.
    let past_the_cut: Vec<u64> = source_timeline
        .iter()
        .copied()
        .filter(|ts| *ts > cut)
        .collect();
    assert!(
        !past_the_cut.is_empty(),
        "fixture: the buffer never recorded past the dump, so there was \
         nothing for the dump to have wrongly picked up"
    );
    let sealed_after: Vec<u64> = source
        .segments
        .iter()
        .filter(|s| s.first_ts > cut)
        .map(|s| s.first_ts)
        .collect();
    assert!(
        !sealed_after.is_empty(),
        "fixture: the source sealed no whole segment after the dump's cut, so \
         'a segment sealed after the snapshot is not in the dump' is untested \
         here; source segments: {:?}",
        source
            .segments
            .iter()
            .map(|s| s.first_ts)
            .collect::<Vec<_>>()
    );
    let reread = read_rez(&dest, TABLE);
    assert_eq!(
        reread.timeline(),
        dump_timeline,
        "the dump picked up {} later rows ({} whole segments' worth) after it \
         was written — a snapshot is a file, not a window onto the buffer",
        past_the_cut.len(),
        sealed_after.len()
    );
    for s in &reread.segments {
        assert!(
            s.first_ts <= cut,
            "the dump holds a segment starting at {}, past its own cut at {cut}",
            s.first_ts
        );
    }
}

/// A dump is an ordinary `.rez`, and the ordinary tools open it.
///
/// The point of the v3 migration for hindsight: the slot ring's only consumer
/// was its own dump routine. Both front doors are checked, because they are
/// different code paths — `parquet metadata` describes the container, while
/// `mcp query` goes through `RezReader` and actually decodes the segments and
/// splices the materialized tail onto them.
#[test]
fn a_dump_opens_in_the_ordinary_rez_tools() {
    let h = Hindsight::start(2, 1);
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("dump.rez");
    // More than a second of data, and that is a property of `mcp query` rather
    // than of the dump: `run_query` walks the recording's time range at a fixed
    // 1 s step, so a sub-second recording — v3 or v2, dump or `record` output —
    // yields no sample and reports the metric as missing. Reproduced against
    // `rezolus record --duration 600ms`, so it is not something a dump does.
    let status = h.wait_until("more than a second of buffered rows", |s| {
        s.segments(TABLE) >= 3 && s.rows >= 25
    });
    h.dump_to(&dest);

    let described = Command::new(env!("CARGO_BIN_EXE_rezolus"))
        .args(["parquet", "metadata", "-i"])
        .arg(&dest)
        .output()
        .expect("failed to run rezolus parquet metadata");
    let stdout = String::from_utf8_lossy(&described.stdout);
    assert!(
        described.status.success(),
        "metadata must describe a dump\nstdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&described.stderr)
    );
    assert!(stdout.contains(".rez archive v3"), "{stdout}");
    assert!(
        stdout.contains("fake"),
        "the sampler must be listed: {stdout}"
    );
    assert!(
        !stdout.contains("not cleanly finalized"),
        "a dump is a finished artifact even though the buffer runs on: {stdout}"
    );

    // Through `RezReader`: the counter is scraped as a rising value once per
    // 100 ms tick, so its rate is ~10/s. The assertion is deliberately loose on
    // the value and strict on it resolving at all — the arithmetic is the query
    // engine's business, but "the dump's rows come back out" is this test's.
    let queried = Command::new(env!("CARGO_BIN_EXE_rezolus"))
        .args(["mcp", "query"])
        .arg(&dest)
        .arg("rate(fake_ops_0[1s])")
        .output()
        .expect("failed to run rezolus mcp query");
    let stdout = String::from_utf8_lossy(&queried.stdout);
    assert!(
        queried.status.success(),
        "a dump must be queryable\nstdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&queried.stderr)
    );
    assert!(
        stdout.contains("fake_ops_0"),
        "the query must resolve against the dump's rows: {stdout}"
    );
    assert!(
        status.rows > 0,
        "fixture: the buffer had rows to be queried"
    );
}

/// What a dump costs on disk: **while a dump holds its read mark, the `-wal`
/// sidecar cannot be recycled, so a long enough dump grows it.**
///
/// The read mark is what makes a dump consistent, and it is also what stops
/// SQLite from rewinding the write-ahead log: a checkpoint may not recycle
/// frames a reader might still need. In steady state the log reaches
/// `wal_autocheckpoint` (4 MiB, set in bytes by `apply_connection_pragmas`) and
/// then stays that size forever, because SQLite recycles it **in place** — the
/// file is the log's high-water mark, not its contents. A dump suspends the
/// recycling, so the writer appends past the mark and the file grows.
///
/// Two findings, and the first is the one an operator will meet:
///
/// - **A short dump costs little or nothing**, because it is bounded by the
///   dump's duration and often lands entirely inside the space the file
///   already has. Measured here across runs: a ~250-330 ms dump moved the
///   sidecar by between **0 and 185 KB**, depending on how much headroom the
///   log happened to have left when it started.
/// - **A long one grows it at the writer's WAL byte rate** and keeps the space
///   afterwards: on top of a 4.67 MB plateau, **+9.1 to +13.9 MB over ~2.2 s
///   of continuous dumping (3.7-6.4 MB/s)** — a 3-4x sidecar — and **+0 B** in
///   the second after the last dump returned, every run. The growth is
///   `rate x duration` and stops dead when the read mark is released.
///
/// The shape is asserted; the magnitude is reported, because it is not
/// portable — it is the writer's WAL write rate, which depends on the host,
/// the scrape payload and the seal policy. This host's numbers are in the
/// test's own output and in the commit message.
///
/// Steady state first, and it is a precondition rather than a warm-up: before
/// the log has reached the autocheckpoint threshold it grows on its own, and
/// "it grew during the dump" would mean nothing.
#[test]
fn a_dump_holds_the_wal_sidecar_open_and_it_plateaus_again_after() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    /// How long to keep dumps in flight. Only has to exceed the log's
    /// remaining headroom (up to 4 MiB at the measured 3.7-7.5 MB/s, i.e.
    /// ~1 s here); the rest is margin.
    const SUSTAINED: Duration = Duration::from_secs(2);
    /// Ceiling on how long the read mark is held while waiting for the sidecar
    /// to pass its target. Reached only when the daemon is committing far below
    /// its configured rate; a failure here means the growth never happened, not
    /// that it was merely slow.
    const SUSTAINED_MAX: Duration = Duration::from_secs(60);

    let h = Hindsight::start(2, WIDE);
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("dump.rez");
    let wal = {
        let mut p = h.buffer.clone().into_os_string();
        p.push("-wal");
        PathBuf::from(p)
    };
    let size = || std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0);

    // Enough segments that a dump outlasts a tick, then a sidecar that has
    // stopped growing on its own: SQLite is now recycling the log at every
    // autocheckpoint, which is the state a dump interrupts.
    h.wait_until("a buffer big enough to make a dump slow", |s| {
        s.segments(TABLE) >= 12
    });
    let plateau = {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let a = size();
            std::thread::sleep(Duration::from_millis(600));
            let b = size();
            if a == b && b > 0 {
                break b;
            }
            assert!(
                Instant::now() < deadline,
                "the -wal sidecar never stopped growing ({a} then {b} bytes)"
            );
        }
    };

    // Sample throughout from another thread, so "during" is a curve rather
    // than a reading taken at a lucky moment.
    let stop = Arc::new(AtomicBool::new(false));
    let samples = Arc::new(std::sync::Mutex::new(Vec::<(Duration, u64)>::new()));
    let sampler = {
        let (stop, samples, wal) = (Arc::clone(&stop), Arc::clone(&samples), wal.clone());
        let t0 = Instant::now();
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let n = std::fs::metadata(&wal).map(|m| m.len()).unwrap_or(0);
                samples.lock().unwrap().push((t0.elapsed(), n));
                std::thread::sleep(Duration::from_millis(10));
            }
        })
    };

    // One dump, for the first finding.
    let one_took = h.dump_to(&dest);
    let after_one = size();

    // Then dumps back to back, so the read mark is effectively continuous.
    //
    // Held until the sidecar has actually grown past its unpinned high-water,
    // rather than for a fixed span. What is being demonstrated is that a held
    // read mark lets committed data accumulate past the point autocheckpoint
    // would otherwise cap the file at — and how long that takes is a function
    // of how fast the daemon commits, which on a loaded machine is a fraction
    // of the configured rate. A fixed window asserts the host's throughput; a
    // target with a generous deadline asserts the mechanism.
    let before = size();
    let target = plateau + plateau / 2;
    let deadline = Instant::now() + SUSTAINED_MAX;
    let started = Instant::now();
    let (mut dumps, mut busy, mut longest_gap) = (0u32, Duration::ZERO, Duration::ZERO);
    let mut last_end = Instant::now();
    while (started.elapsed() < SUSTAINED || size() < target) && Instant::now() < deadline {
        longest_gap = longest_gap.max(last_end.elapsed());
        busy += h.dump_to(&dest);
        last_end = Instant::now();
        dumps += 1;
    }
    longest_gap = longest_gap.max(last_end.elapsed());
    let held = started.elapsed();
    let during = size();

    // Read mark released. A second of the same ticking, for the contrast.
    // Wait for the sidecar to STOP growing rather than sampling a fixed second
    // of it. Releasing the read mark lets the checkpointer recycle the log, but
    // it does not do so instantly, and on a loaded machine a one-second sample
    // can land entirely inside the catch-up and read as "still growing at the
    // dump rate" — which is the claim below inverted. Waiting for the plateau
    // asks the question the test is named for and does not race the
    // checkpointer to do it.
    /// How long the sidecar gets to stop growing once no mark is held. Generous
    /// because it covers the checkpointer's backlog on a loaded host, not
    /// because the ceiling is expected to take that long.
    const SETTLE_TIMEOUT: Duration = Duration::from_secs(30);
    let settle_deadline = Instant::now() + SETTLE_TIMEOUT;
    let mut after = size();
    let mut settled = false;
    loop {
        std::thread::sleep(Duration::from_millis(250));
        let now = size();
        if now == after {
            settled = true;
            break;
        }
        after = now;
        if Instant::now() >= settle_deadline {
            break;
        }
    }
    stop.store(true, Ordering::Relaxed);
    sampler.join().unwrap();

    let grew = during.saturating_sub(before);
    // Read once: two live `lock()` temporaries in one `println!` deadlock, the
    // guards being dropped only at the end of the statement.
    let (taken, peak) = {
        let samples = samples.lock().unwrap();
        (
            samples.len(),
            samples.iter().map(|(_, n)| *n).max().unwrap_or(0),
        )
    };
    println!(
        "MEASURED -wal at {INTERVAL:?} / {WIDE} counters:\n  \
         steady-state plateau           {plateau} B\n  \
         after one {one_took:?} dump  {after_one} B ({:+} B)\n  \
         after {held:?} of dumps  {during} B ({:+} B, {:.2} MB/s; {dumps} dumps, \
         {busy:?} inside one, longest gap {longest_gap:?})\n  \
         1 s after the last dump        {after} B ({:+} B)\n  \
         peak over {} samples           {} B",
        after_one as i64 - plateau as i64,
        grew as i64,
        (grew as f64 / 1e6) / held.as_secs_f64(),
        after as i64 - during as i64,
        taken,
        peak,
    );

    // Fixture check: was the read mark effectively continuous? Measured
    // against the length of a dump rather than as a fraction of the window,
    // because the window holds very few dumps — a run that produced this
    // assertion's last failure managed two, of ~1.3 s each. Per-iteration
    // overhead amortized over two iterations is a large share of the window
    // and none of it means the mark lapsed, so a fixed coverage ratio measures
    // the host's scheduling, which is what the rest of this test is written to
    // avoid asserting. What would actually break the fixture is a gap long
    // enough to be a lapse in its own right, and a dump is the natural scale
    // for that.
    assert!(dumps > 0, "fixture: no dump ran inside the {held:?} window");
    let average_dump = busy / dumps;
    assert!(
        longest_gap < average_dump,
        "fixture: the longest gap between dumps was {longest_gap:?}, against an \
         average dump of {average_dump:?} ({dumps} dumps, {busy:?} inside one, \
         {held:?} window) — the read mark lapsed rather than being held across \
         the window"
    );
    // Past the UNPINNED high-water, by half again. The threshold is what makes
    // this test mean anything: with the read mark removed the sidecar still
    // creeps — it went up by 4,120 B, one page, in the same window — because
    // each individual statement takes a brief mark of its own. `during >
    // before` is true of that too. What only a HELD mark can do is push the
    // file past the size autocheckpoint would otherwise cap it at, and that is
    // a factor of thousands away from a page, not a percentage.
    assert!(
        during >= plateau + plateau / 2,
        "the sidecar must grow past its unpinned high-water while a dump holds \
         the log open: plateau {plateau} B, {before} B before, {during} B \
         after {held:?} of dumps — either nothing was committed during them, \
         or the log was recycled under a read mark"
    );
    assert!(
        after >= during,
        "the sidecar shrank on its own: {during} B at the end of the dumps, \
         {after} B a second later — SQLite recycles the log in place, it does \
         not truncate the file"
    );
    // And it was the read mark that did it, not the workload: released, the
    // sidecar reaches a new ceiling and stops, where under a held mark the same
    // ticking grew it without bound.
    //
    // "It stops" is the assertion, not "it stopped within some size or some
    // second". Releasing the mark lets the checkpointer recycle the log, but it
    // has a backlog to work off while the daemon keeps committing, so the file
    // goes on growing for a while first — on a loaded host, by as much as it
    // grew under the mark. That growth is the catch-up, and bounding it means
    // asserting the host's speed. Reaching a ceiling at all is what a held mark
    // prevents, and what the test is named for.
    assert!(
        settled,
        "the sidecar never stopped growing once the dumps ended: {after} B and \
         still climbing after {:?}, against {during} B when the last mark was \
         released and {grew} B of growth under the marks",
        SETTLE_TIMEOUT
    );
}

/// A `.dendro` output keeps the buffer as a dendro archive, and its dumps
/// are dendro archives that the ordinary readers open, finished and
/// queryable, while the buffer runs on.
#[test]
fn a_dendro_buffer_dumps_a_dendro_archive() {
    use metriken_archive::Catalog;

    let agent = spawn_fake_agent(1);
    let h = Hindsight::try_start_as(agent, 2, "dendro")
        .unwrap_or_else(|why| panic!("rezolus hindsight failed to come up: {why}"));
    assert_eq!(
        h.buffer.extension().and_then(|e| e.to_str()),
        Some("dendro"),
        "the buffer is a dendro archive: {}",
        h.buffer.display()
    );
    h.wait_until("more than a second of buffered rows", |s| {
        s.segments(TABLE) >= 3 && s.rows >= 25
    });
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("dump.dendro");
    h.dump_to(&dest);

    let catalog =
        metriken_archive::DendroCatalog::open(&dest).expect("the dump is a dendro archive");
    let sources = catalog.sources().unwrap();
    assert_eq!(sources.len(), 1);
    assert!(sources[0].complete, "a dump is a finished artifact");

    let queried = Command::new(env!("CARGO_BIN_EXE_rezolus"))
        .args(["mcp", "query"])
        .arg(&dest)
        .arg("rate(fake_ops_0[1s])")
        .output()
        .expect("failed to run rezolus mcp query");
    let stdout = String::from_utf8_lossy(&queried.stdout);
    assert!(
        queried.status.success() && stdout.contains("fake_ops_0"),
        "a dump must be queryable\nstdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&queried.stderr)
    );
}
