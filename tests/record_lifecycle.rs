//! Process-level regression tests for `rezolus record`'s stop path.
//!
//! Both properties here are only observable from outside the process — the
//! exit code a supervisor sees, and the wall time between SIGTERM and exit —
//! so they are tested by driving the real binary against a stand-in agent
//! rather than by calling into `recorder::run` (which owns `std::process::exit`).
//!
//! Unix only: the SIGTERM case has no meaning elsewhere.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

/// Minimal stand-in for a stream producer: serves a replication stream on
/// `/metrics/stream` (see [`serve_stream`]) and 404s everything else,
/// `/metrics/binary` and the agent's metadata routes (`/systeminfo`,
/// `/metrics/descriptions`, `/status`) included. The recorder asks for the
/// stream first and records it. Returns the bound port; the accept loop is
/// detached and dies with the test process.
fn spawn_fake_agent() -> u16 {
    spawn_agent(Streams::Always, None)
}

/// Whether and how a fake agent serves `/metrics/stream`.
#[derive(Clone, Copy)]
enum Streams {
    /// 404, as an agent before 5.21.0.
    Never,
    /// A stream on every subscription.
    Always,
    /// One subscription gets this many frames and is then closed; every
    /// later subscription gets a 404, as a proxy that stopped routing the
    /// path would answer.
    OnceThen404(u64),
    /// One subscription gets this many frames and is then closed, as when the
    /// agent restarts; every later one streams under a new producer epoch,
    /// and `/` then reports a new version.
    Restarts(u64),
}

/// The epoch and `/` version the restarting agent reports before and after.
const BEFORE_RESTART: (&str, &str) = ("fake-epoch", "6.0.0");
const AFTER_RESTART: (&str, &str) = ("fake-epoch-2", "6.0.1");

/// A 6.0-version agent whose stream closes after `frames` frames, and whose
/// stream route answers 404 to the reconnect. Reports itself as 6.0.0 on
/// `/`.
fn spawn_fake_agent_that_stops_streaming(frames: u64) -> u16 {
    spawn_agent(Streams::OnceThen404(frames), Some("6.0.0"))
}

/// A stand-in for an agent older than `/metrics/stream` (5.21.0): it scrapes
/// like any other, answers `/` with the `Rezolus <version> Agent` banner, and
/// 404s the stream route and `/status`.
fn spawn_fake_agent_without_stream(version: &'static str) -> u16 {
    spawn_agent(Streams::Never, Some(version))
}

/// As [`spawn_fake_agent_without_stream`], but nothing listens on the port
/// until `ready` returns true: an agent that comes up after the recording
/// started.
fn spawn_fake_agent_without_stream_when(
    version: &'static str,
    ready: impl Fn() -> bool + Send + 'static,
) -> u16 {
    // Reserve a free port, release it, and bind it again once `ready`.
    // Another process could take it in between; on a test host that is
    // unlikely enough not to matter.
    let port = TcpListener::bind("127.0.0.1:0")
        .expect("failed to reserve a port")
        .local_addr()
        .unwrap()
        .port();
    std::thread::spawn(move || {
        while !ready() {
            std::thread::sleep(Duration::from_millis(20));
        }
        let listener =
            TcpListener::bind(("127.0.0.1", port)).expect("failed to bind the late agent");
        serve_agent(
            listener,
            Streams::Never,
            Some(version),
            Names::Metriken,
            Fixture::default(),
        );
    });
    port
}

fn spawn_agent(streams: Streams, banner: Option<&'static str>) -> u16 {
    spawn_agent_with_names(streams, banner, Names::Metriken)
}

/// Which names a fake agent serves its stream under.
#[derive(Clone, Copy)]
enum Names {
    /// `application/vnd.metriken.replication.v1+dendro` and `x-metriken-*`,
    /// as a current agent serves.
    Metriken,
    /// `application/vnd.rezolus.replication.v1+dendro` and `x-rezolus-*`, as
    /// agents from 5.21.0 through 6.0.0-alpha.44 serve.
    Legacy,
}

impl Names {
    /// The content type and the update-floor, frame-interval and layout
    /// header names.
    fn get(self) -> [&'static str; 4] {
        match self {
            Names::Metriken => [
                "application/vnd.metriken.replication.v1+dendro",
                "x-metriken-update-floor",
                "x-metriken-frame-interval",
                "x-metriken-layout",
            ],
            Names::Legacy => [
                "application/vnd.rezolus.replication.v1+dendro",
                "x-rezolus-update-floor",
                "x-rezolus-frame-interval",
                "x-rezolus-layout",
            ],
        }
    }
}

fn spawn_agent_with_names(streams: Streams, banner: Option<&'static str>, names: Names) -> u16 {
    spawn_fixture(streams, banner, names, Fixture::default())
}

fn spawn_fixture(
    streams: Streams,
    banner: Option<&'static str>,
    names: Names,
    fixture: Fixture,
) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("failed to bind the fake agent");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || serve_agent(listener, streams, banner, names, fixture));
    port
}

/// What a fake producer says about itself, and whether it is up yet.
#[derive(Clone, Default)]
struct Fixture {
    /// The handshake's labels.
    labels: &'static [(&'static str, &'static str)],
    /// The handshake's metadata.
    metadata: &'static [(&'static str, &'static str)],
    /// While this returns false, every request is answered 503, as a proxy
    /// in front of a producer that is not up yet answers.
    up: Option<std::sync::Arc<dyn Fn() -> bool + Send + Sync>>,
    /// Served at `/systeminfo` as JSON; a 404 when `None`.
    systeminfo: Option<&'static str>,
    /// `/status`, `/systeminfo` and `/metrics/descriptions` accept the
    /// request and never answer, so the recorder's metadata read times out.
    hang_metadata: bool,
    /// Created when `/systeminfo` or `/metrics/descriptions` is requested,
    /// so a test can end the run on that event instead of on a timer. Only
    /// the metadata read after the stream opens asks for those; the probe of
    /// an endpoint that cannot stream yet asks for `/status`.
    on_hang: Option<std::path::PathBuf>,
}

/// The fake agent's accept loop; see [`spawn_fake_agent`].
fn serve_agent(
    listener: TcpListener,
    streams: Streams,
    banner: Option<&'static str>,
    names: Names,
    fixture: Fixture,
) {
    let subscriptions = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let subscriptions = subscriptions.clone();
            let fixture = fixture.clone();
            // A thread per connection: a stream subscription holds its
            // connection for the whole run.
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
                if fixture.up.as_ref().is_some_and(|up| !up()) {
                    let _ = stream.write_all(
                        b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\
                          Connection: close\r\n\r\n",
                    );
                } else if path.starts_with("/metrics/stream")
                    && match streams {
                        Streams::Never => false,
                        Streams::Always => true,
                        Streams::OnceThen404(_) => {
                            subscriptions.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0
                        }
                        Streams::Restarts(_) => true,
                    }
                {
                    let (frames, epoch) = match streams {
                        Streams::OnceThen404(n) => (Some(n), BEFORE_RESTART.0),
                        Streams::Restarts(n) => {
                            if subscriptions.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0
                            {
                                (Some(n), BEFORE_RESTART.0)
                            } else {
                                (None, AFTER_RESTART.0)
                            }
                        }
                        _ => (None, BEFORE_RESTART.0),
                    };
                    serve_stream(stream, &path, frames, epoch, names, &fixture);
                    return;
                } else if fixture.hang_metadata
                    && matches!(
                        path.as_str(),
                        "/status" | "/systeminfo" | "/metrics/descriptions"
                    )
                {
                    if let (Some(flag), "/systeminfo" | "/metrics/descriptions") =
                        (&fixture.on_hang, path.as_str())
                    {
                        let _ = std::fs::write(flag, b"");
                    }
                    std::thread::sleep(Duration::from_secs(60));
                    return;
                } else if let (Some(body), "/systeminfo") = (fixture.systeminfo, path.as_str()) {
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(head.as_bytes());
                    let _ = stream.write_all(body.as_bytes());
                } else if let (Some(version), "/") = (banner, path.as_str()) {
                    let restarted = matches!(streams, Streams::Restarts(_))
                        && subscriptions.load(std::sync::atomic::Ordering::SeqCst) > 1;
                    let version = if restarted { AFTER_RESTART.1 } else { version };
                    let body = format!("Rezolus {version} Agent\n");
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(head.as_bytes());
                    let _ = stream.write_all(body.as_bytes());
                } else {
                    let _ = stream.write_all(
                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    );
                }
                let _ = stream.flush();
            });
        }
    }
}

fn wall_ns() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
}

/// The agent's replication stream, as `/metrics/stream` serves it: the
/// preamble and a handshake, then one rows frame per requested interval carrying one
/// counter in the `fake/ops` acquisition group, until the recorder hangs up or
/// `frames` have been sent. The content type and headers are `names`'.
fn serve_stream(
    mut stream: std::net::TcpStream,
    path: &str,
    frames: Option<u64>,
    epoch: &str,
    names: Names,
    fixture: &Fixture,
) {
    use dendro::replicate::{wire, Frame};
    use metriken_exposition::{GroupSchema, GroupSnapshot, MetricDesc};

    // The interval the recorder asked for (`?interval=`, a humantime
    // duration), as the agent honours it; 100ms when absent.
    let interval = path
        .split_once("interval=")
        .map(|(_, v)| v.split('&').next().unwrap_or(v).replace("%20", " "))
        .and_then(|v| humantime::parse_duration(&v).ok())
        .unwrap_or(Duration::from_millis(100));
    let [content_type, update_floor, frame_interval, layout] = names.get();
    let interval_text = humantime::format_duration(interval).to_string();
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\n\
         {update_floor}: 100ms\r\n{frame_interval}: {interval_text}\r\n\
         {layout}: wide\r\nConnection: close\r\n\r\n"
    );
    let mut bytes = head.as_bytes().to_vec();
    wire::write_preamble(&mut bytes).unwrap();
    let anchor = wall_ns();
    wire::encode_frame(
        &Frame::Handshake {
            source: 0,
            uuid: Some(epoch.to_string()),
            labels: fixture
                .labels
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            metadata: fixture
                .metadata
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            clock_anchor_wall_ns: anchor as i64,
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
        counters: vec![MetricDesc {
            name: "0x0".to_string(),
            metadata: [("metric".to_string(), "fake_ops".to_string())]
                .into_iter()
                .collect(),
        }],
        gauges: Vec::new(),
        histograms: Vec::new(),
    };
    for seq in 0u64.. {
        if frames.is_some_and(|n| seq >= n) {
            // Closing the connection is how the stream ends.
            return;
        }
        std::thread::sleep(interval);
        let ts = wall_ns();
        let group = GroupSnapshot {
            name: "fake/ops".to_string(),
            schema_hash: schema.hash(),
            schema: Some(std::sync::Arc::new(schema.clone())),
            window: Some(metriken::Window::new(ts - 1_000_000, ts)),
            counters: vec![Some(seq * 10)],
            gauges: Vec::new(),
            histograms: Vec::new(),
        };
        let row = rez::wal::encode_wal_group_row(&rez::wal::wal_group_row(
            &group,
            Some((&schema).into()),
        ))
        .unwrap();
        let mut bytes = Vec::new();
        wire::encode_frame(
            &Frame::Rows {
                source: 0,
                seq,
                index_state: dendro::replicate::NO_INDEX_STATE,
                rows: vec![dendro::archive::WalRow {
                    stream: "fake/ops".to_string(),
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

/// Minimal stand-in for a Prometheus exporter. Answers `/metrics` with text
/// exposition and 404s everything else, so the recorder's probe classifies it
/// as prometheus rather than msgpack.
fn spawn_fake_exporter() -> u16 {
    spawn_fake_exporter_named("http_requests_total")
}

/// As `spawn_fake_exporter`, with the counter's name chosen by the caller so
/// two exporters in one run are telling apart by what they expose.
fn spawn_fake_exporter_named(counter: &'static str) -> u16 {
    spawn_fake_exporter_counted(counter).0
}

/// As `spawn_fake_exporter_named`, also returning how many requests it has
/// answered, so a test can tell when the recorder is past startup (the
/// startup probe is the first request, the first tick's scrape the second).
fn spawn_fake_exporter_counted(
    counter: &'static str,
) -> (u16, std::sync::Arc<std::sync::atomic::AtomicU64>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("failed to bind the fake exporter");
    let port = listener.local_addr().unwrap().port();
    let hits = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let served = hits.clone();
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
            tick += 1;
            let body = format!(
                "# HELP {counter} Total requests.\n\
                 # TYPE {counter} counter\n\
                 {counter}{{code=\"200\"}} {tick}\n\
                 # TYPE queue_depth gauge\n\
                 queue_depth {}\n",
                tick * 2
            );
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain; version=0.0.4\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(body.as_bytes());
            let _ = stream.flush();
            served.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    });
    (port, hits)
}

/// `-o out.dendro` writes a dendro archive through metriken-storage's writer,
/// end to end through the binary: a Prometheus endpoint, converted to one
/// acquisition group per scrape, read back by the same reader `view` and
/// `mcp` use.
#[test]
fn a_prometheus_endpoint_records_into_a_dendro() {
    let port = spawn_fake_exporter();
    let dir = tempfile::tempdir().expect("failed to create a temp dir");
    let output = dir.path().join("prom.dendro");

    let out = Command::new(env!("CARGO_BIN_EXE_rezolus"))
        .arg("record")
        .arg("--endpoint")
        .arg(format!("http://127.0.0.1:{port}/metrics,source=svc"))
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
        "recording to .dendro must exit 0 (status {:?})\nstderr:\n{stderr}",
        out.status.code()
    );
    assert_eq!(
        metriken_storage::DendroCatalog::is_archive(&output),
        Ok(true),
        "a .dendro output is a dendro archive"
    );

    let described = Command::new(env!("CARGO_BIN_EXE_rezolus"))
        .arg("recording")
        .arg("metadata")
        .arg("-i")
        .arg(&output)
        .output()
        .expect("failed to run rezolus recording metadata");
    let stdout = String::from_utf8_lossy(&described.stdout);
    assert!(described.status.success(), "{stdout}");
    // The table is the acquisition group, keyed `<sampler>/<group>`.
    assert!(
        stdout.contains("prometheus/scrape"),
        "the scrape must land in its own acquisition group table: {stdout}"
    );
    assert!(
        stdout.contains("source=svc"),
        "the recording keeps its source label: {stdout}"
    );

    let described = Command::new(env!("CARGO_BIN_EXE_rezolus"))
        .arg("mcp")
        .arg("describe-metrics")
        .arg(&output)
        .output()
        .expect("failed to run rezolus mcp describe-metrics");
    let stdout = String::from_utf8_lossy(&described.stdout);
    assert!(described.status.success(), "{stdout}");
    for metric in ["http_requests_total", "queue_depth"] {
        assert!(
            stdout.contains(metric),
            "{metric} must be readable back out: {stdout}"
        );
    }
}

/// A Prometheus endpoint's `# HELP` lines land in its recording's
/// `descriptions` metadata, the key an agent recording fills from
/// `/metrics/descriptions`. A metric without a HELP line has no entry.
#[test]
fn a_prometheus_endpoints_help_text_is_the_recordings_descriptions() {
    use metriken_storage::Catalog;

    let port = spawn_fake_exporter_named("widgets_total");
    let dir = tempfile::tempdir().expect("failed to create a temp dir");
    let output = dir.path().join("help.dendro");

    let out = Command::new(env!("CARGO_BIN_EXE_rezolus"))
        .arg("record")
        .arg("--endpoint")
        .arg(format!("http://127.0.0.1:{port}/metrics,source=svc"))
        .arg("-o")
        .arg(&output)
        .arg("--interval")
        .arg("100ms")
        .arg("--duration")
        .arg("1s")
        .output()
        .expect("failed to run rezolus record");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");

    let catalog = metriken_storage::DendroCatalog::open(&output).expect("the archive opens");
    let sources = catalog.sources().expect("the catalog reads");
    assert_eq!(sources.len(), 1, "{sources:?}");
    let json = sources[0]
        .metadata
        .get("descriptions")
        .unwrap_or_else(|| panic!("no descriptions: {:?}", sources[0].metadata));
    let descriptions: std::collections::BTreeMap<String, String> =
        serde_json::from_str(json).expect("descriptions is a JSON object of strings");
    assert_eq!(
        descriptions,
        std::collections::BTreeMap::from([(
            "widgets_total".to_string(),
            "Total requests.".to_string()
        )]),
        "queue_depth has no HELP line and so no entry"
    );
}

/// A scrape's acquisition window is on the same clock as the row that holds
/// it.
///
/// The archive stores a window as an OFFSET from its row's `ts`
/// (`window_offset_columns` subtracts one from the other), so the two have to
/// come from one clock. The window used to be a raw wall reading while the row
/// was anchored, which made that offset carry the wall-versus-anchor
/// divergence — the very quantity `wall_offset` exists to record — instead of
/// the read's position within the tick. And the window is what `rate()` prices
/// its uncertainty band from.
///
/// End to end through the real binary, because the two clocks are read in
/// different layers — `scrape_one` and the tick loop — and a unit test on
/// either one cannot see them disagree.
#[test]
fn a_scrape_window_is_on_the_same_clock_as_its_row() {
    let port = spawn_fake_exporter();
    let dir = tempfile::tempdir().expect("failed to create a temp dir");
    let output = dir.path().join("window.dendro");

    let out = Command::new(env!("CARGO_BIN_EXE_rezolus"))
        .arg("record")
        .arg("--endpoint")
        .arg(format!("http://127.0.0.1:{port}/metrics,source=svc"))
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
        "the run must exit 0\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Read the SEALED segment, not the WAL: finalize seals and prunes, and the
    // sealed columns are what a consumer actually reads. `:window_begin` is
    // the offset itself — the archive stores it relative to the row's `ts`, so
    // the number under test is the number on disk.
    use arrow::array::Array;
    use metriken_storage::Catalog;

    let catalog = metriken_storage::DendroCatalog::open(&output).expect("the archive opens");
    let id = catalog.sources().expect("the catalog reads")[0].id;
    let segments = catalog
        .segment_meta(id, "prometheus/scrape")
        .expect("the scrape's segments read back");
    assert!(!segments.is_empty(), "the run sealed at least one segment");

    let mut checked = 0usize;
    for (seq, _) in &segments {
        let bytes = catalog
            .segment_bytes(id, "prometheus/scrape", *seq)
            .expect("the segment's bytes read back")
            .expect("a sealed segment has bytes");
        let reader = parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(
            bytes::Bytes::from(bytes),
        )
        .expect("the segment is parquet")
        .build()
        .expect("the segment reads");
        for batch in reader {
            let batch = batch.expect("a record batch");
            let col = batch
                .column_by_name(":window_begin")
                .expect("a group table carries a table-level window");
            let offsets = col
                .as_any()
                .downcast_ref::<arrow::array::Int64Array>()
                .expect(":window_begin is an i64 offset from the row's ts");
            for i in 0..offsets.len() {
                if offsets.is_null(i) {
                    continue;
                }
                let offset = offsets.value(i);
                // The window brackets the round trip, so it sits within a
                // scrape's distance of the row's own stamp. A window on a
                // different clock is out by the divergence between the two,
                // which is unbounded and grows for as long as the recording
                // runs — a whole second is generous for a localhost scrape and
                // still far tighter than any real divergence.
                assert!(
                    offset.abs() < 1_000_000_000,
                    "a window beginning {offset} ns from its row's ts is not on \
                     the row's clock"
                );
                checked += 1;
            }
        }
    }
    assert!(checked > 0, "at least one window was actually checked");
}

/// Several Prometheus endpoints in one run: each is its own recording, and
/// each keeps its own metrics.
///
/// Every recording's table is keyed `prometheus/scrape` — the SAME key in all
/// of them — so this is where a per-recording namespace either holds or does
/// not. It also exercises the id space: each endpoint's converter counts from
/// 0, so both recordings' first metric is column `"0"` while meaning entirely
/// different things.
#[test]
fn several_prometheus_endpoints_each_become_their_own_recording() {
    let a = spawn_fake_exporter_named("alpha_total");
    let b = spawn_fake_exporter_named("beta_total");
    let dir = tempfile::tempdir().expect("failed to create a temp dir");
    let output = dir.path().join("fleet.dendro");

    let out = Command::new(env!("CARGO_BIN_EXE_rezolus"))
        .arg("record")
        .arg("--endpoint")
        .arg(format!("http://127.0.0.1:{a}/metrics,source=alpha"))
        .arg("--endpoint")
        .arg(format!("http://127.0.0.1:{b}/metrics,source=beta"))
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
        "two prometheus endpoints must record (status {:?})\nstderr:\n{stderr}",
        out.status.code()
    );

    let described = Command::new(env!("CARGO_BIN_EXE_rezolus"))
        .arg("mcp")
        .arg("describe-recording")
        .arg(&output)
        .output()
        .expect("failed to run rezolus mcp describe-recording");
    let listing = String::from_utf8_lossy(&described.stdout);
    assert!(
        listing.contains("source=alpha") && listing.contains("source=beta"),
        "both targets must be recordings in the archive: {listing}"
    );

    // Each recording holds ITS OWN metric, not the other's. Both are column
    // "0" in their own table, so a namespace collision would show up here as
    // the wrong name coming back.
    for (source, mine, theirs) in [
        ("alpha", "alpha_total", "beta_total"),
        ("beta", "beta_total", "alpha_total"),
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_rezolus"))
            .arg("mcp")
            .arg("describe-metrics")
            .arg(&output)
            .arg("--recording")
            .arg(format!("source={source}"))
            .output()
            .expect("failed to run rezolus mcp describe-metrics");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains(mine),
            "the {source} recording must hold {mine}: {stdout}"
        );
        assert!(
            !stdout.contains(theirs),
            "the {source} recording must NOT hold {theirs}: {stdout}"
        );
    }
}

/// A `.dendro` streams a Rezolus agent and scrapes a Prometheus exporter in
/// the same run, into one archive with a recording each.
///
/// The two reach the writer on different paths — the agent's rows off its
/// stream pump, the exporter's from the tick's scrape — and are committed
/// together each tick, so this is where a run that fed only one of them, or
/// opened a second archive handle, would show.
#[test]
fn a_dendro_streams_an_agent_and_scrapes_a_prometheus_exporter_into_one_archive() {
    use metriken_storage::Catalog;

    let agent = spawn_fake_agent();
    let exporter = spawn_fake_exporter_named("http_requests_total");
    let dir = tempfile::tempdir().expect("failed to create a temp dir");
    let output = dir.path().join("mixed.dendro");

    let out = Command::new(env!("CARGO_BIN_EXE_rezolus"))
        .arg("record")
        .arg("--endpoint")
        .arg(format!("http://127.0.0.1:{agent},source=agent"))
        .arg("--endpoint")
        .arg(format!("http://127.0.0.1:{exporter}/metrics,source=svc"))
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
        "a mixed .dendro run must record (status {:?})\nstderr:\n{stderr}",
        out.status.code()
    );
    assert!(
        stderr.contains("subscribed to its replication stream"),
        "the agent is streamed, not scraped:\n{stderr}"
    );

    let catalog = metriken_storage::DendroCatalog::open(&output).expect("the archive opens");
    let sources = catalog.sources().expect("the catalog reads");
    assert_eq!(sources.len(), 2, "one recording per endpoint: {sources:?}");
    let rows = |id: i64, table: &str| {
        let sealed = catalog.segment_span(id, table).unwrap().1.rows;
        let live = catalog.live_wal_span(id, table).unwrap().rows;
        sealed + live
    };
    for (source, table) in [("agent", "fake/ops"), ("svc", "prometheus/scrape")] {
        let recording = sources
            .iter()
            .find(|s| s.labels.get("source").map(String::as_str) == Some(source))
            .unwrap_or_else(|| panic!("a recording for {source}: {sources:?}"));
        assert!(recording.complete, "{source} is finalized");
        let tables = catalog.tables(recording.id).unwrap();
        assert!(
            tables.iter().any(|t| t == table),
            "{source} holds {table}: {tables:?}"
        );
        let n = rows(recording.id, table);
        assert!(n > 0, "{source}'s {table} holds rows");
    }
}

/// One `.dendro` run records an agent serving the legacy stream names
/// (`application/vnd.rezolus.replication.v1+dendro`, as 5.21.0 through
/// 6.0.0-alpha.44 agents do) and one serving metriken's names, a recording each.
/// GO check 5 of metriken's `docs/journal/2026-10-11-stream-route.md`.
#[test]
fn a_dendro_records_agents_serving_legacy_and_metriken_stream_names() {
    use metriken_storage::Catalog;

    let legacy = spawn_agent_with_names(Streams::Always, None, Names::Legacy);
    let current = spawn_agent_with_names(Streams::Always, None, Names::Metriken);
    let dir = tempfile::tempdir().expect("failed to create a temp dir");
    let output = dir.path().join("names.dendro");

    let out = Command::new(env!("CARGO_BIN_EXE_rezolus"))
        .arg("record")
        .arg("--endpoint")
        .arg(format!("http://127.0.0.1:{legacy},source=legacy"))
        .arg("--endpoint")
        .arg(format!("http://127.0.0.1:{current},source=current"))
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
        "both agents must record (status {:?})\nstderr:\n{stderr}",
        out.status.code()
    );
    assert_eq!(
        stderr
            .matches("subscribed to its replication stream")
            .count(),
        2,
        "both agents are streamed:\n{stderr}"
    );

    let catalog = metriken_storage::DendroCatalog::open(&output).expect("the archive opens");
    let sources = catalog.sources().expect("the catalog reads");
    assert_eq!(sources.len(), 2, "one recording per endpoint: {sources:?}");
    for source in ["legacy", "current"] {
        let recording = sources
            .iter()
            .find(|s| s.labels.get("source").map(String::as_str) == Some(source))
            .unwrap_or_else(|| panic!("a recording for {source}: {sources:?}"));
        assert!(recording.complete, "{source} is finalized");
        let sealed = catalog
            .segment_span(recording.id, "fake/ops")
            .unwrap()
            .1
            .rows;
        let live = catalog
            .live_wal_span(recording.id, "fake/ops")
            .unwrap()
            .rows;
        assert!(sealed + live > 0, "{source}'s fake/ops holds rows");
    }
}

/// An agent that restarts mid-recording is recorded in the archive's
/// metadata: `producer_epochs` gains the new process from its first row, and
/// `producer_epoch` and `version` name the new process, which reports a new
/// version.
#[test]
fn an_agent_restart_is_written_into_the_recording_metadata() {
    use metriken_storage::Catalog;

    let agent = spawn_agent(Streams::Restarts(5), Some(BEFORE_RESTART.1));
    let dir = tempfile::tempdir().expect("failed to create a temp dir");
    let output = dir.path().join("restart.dendro");

    let out = Command::new(env!("CARGO_BIN_EXE_rezolus"))
        .arg("record")
        .arg("--url")
        .arg(format!("http://127.0.0.1:{agent}"))
        .arg("-o")
        .arg(&output)
        .arg("--interval")
        .arg("100ms")
        .arg("--duration")
        .arg("3s")
        .output()
        .expect("failed to run rezolus record");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "the run must record through the restart (status {:?})\nstderr:\n{stderr}",
        out.status.code()
    );
    assert!(
        stderr.contains("now records the agent restart"),
        "the restart is written into the metadata:\n{stderr}"
    );

    let catalog = metriken_storage::DendroCatalog::open(&output).expect("the archive opens");
    let sources = catalog.sources().expect("the catalog reads");
    assert_eq!(sources.len(), 1, "{sources:?}");
    let metadata = &sources[0].metadata;
    assert_eq!(
        metadata.get("producer_epoch").map(String::as_str),
        Some(AFTER_RESTART.0),
        "{metadata:?}"
    );
    assert_eq!(
        metadata.get("version").map(String::as_str),
        Some(AFTER_RESTART.1),
        "{metadata:?}"
    );
    let epochs: serde_json::Value = serde_json::from_str(
        metadata
            .get("producer_epochs")
            .unwrap_or_else(|| panic!("no producer_epochs: {metadata:?}")),
    )
    .unwrap();
    let epochs = epochs.as_array().unwrap();
    assert_eq!(epochs.len(), 2, "{epochs:?}");
    assert_eq!(epochs[0]["epoch"], BEFORE_RESTART.0);
    assert_eq!(epochs[0]["version"], BEFORE_RESTART.1);
    assert_eq!(epochs[1]["epoch"], AFTER_RESTART.0);
    assert_eq!(epochs[1]["version"], AFTER_RESTART.1);
    assert!(
        epochs[1]["from_ts"].as_u64() > epochs[0]["from_ts"].as_u64(),
        "the new epoch starts after the first: {epochs:?}"
    );
}

/// An agent that cannot serve `/metrics/stream` (older than 5.21.0) is
/// refused by name and version before the archive is created, so nothing is
/// written, and the refusal points at a 5.x recorder, which scrapes it. It is
/// never scraped instead. The default output (no `-o`) is refused the same
/// way.
#[test]
fn an_agent_without_a_stream_is_refused_at_startup_pointing_at_a_5x_recorder() {
    let agent = spawn_fake_agent_without_stream("5.20.0");
    let dir = tempfile::tempdir().expect("failed to create a temp dir");
    let output = dir.path().join("old.dendro");

    for with_output in [true, false] {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_rezolus"));
        cmd.current_dir(dir.path())
            .arg("record")
            .arg("--url")
            .arg(format!("http://127.0.0.1:{agent}"))
            .arg("--interval")
            .arg("100ms")
            .arg("--duration")
            .arg("1s");
        if with_output {
            cmd.arg("-o").arg(&output);
        }
        let out = cmd.output().expect("failed to run rezolus record");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(1), "refused:\n{stderr}");
        for needle in [
            "Rezolus 5.20.0",
            "predates",
            "/metrics/stream",
            "5.21.0",
            "Nothing was recorded",
            "5.x recorder",
            "5.27.0 or later",
            "recording upgrade --to dendro",
            &format!("127.0.0.1:{agent}"),
        ] {
            assert!(stderr.contains(needle), "{needle:?} in:\n{stderr}");
        }
        for gone in ["-o out.rez", "-o out.parquet"] {
            assert!(!stderr.contains(gone), "{gone:?} in:\n{stderr}");
        }
        let left: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert!(left.is_empty(), "nothing is written: {left:?}");
    }
}

/// An output in a format 6.x no longer writes is refused before anything is
/// contacted or written: `.rez`, `.parquet` and `.raw` by name, pointing at
/// `.dendro` and a 5.x recorder, as are `--format` with one of those values
/// and `--separate`. Usage errors, so exit 2.
#[test]
fn an_output_in_a_removed_format_is_refused() {
    let dir = tempfile::tempdir().expect("failed to create a temp dir");
    let mut runs: Vec<Vec<String>> = ["rez", "parquet", "raw"]
        .iter()
        .map(|ext| vec!["-o".to_string(), format!("out.{ext}")])
        .collect();
    runs.push(vec!["--format".to_string(), "parquet".to_string()]);
    runs.push(vec!["--separate".to_string()]);
    for args in runs {
        let out = Command::new(env!("CARGO_BIN_EXE_rezolus"))
            .current_dir(dir.path())
            .arg("record")
            // Nothing listens here: the refusal comes before any connection.
            .arg("--url")
            .arg("http://127.0.0.1:1")
            .args(&args)
            .output()
            .expect("failed to run rezolus record");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{args:?}:\n{stderr}");
        for needle in [args[0].as_str(), "5.x recorder", "5.27.0 or later"] {
            assert!(
                stderr.contains(needle),
                "{args:?}: {needle:?} in:\n{stderr}"
            );
        }
        if args[0] != "--separate" {
            assert!(stderr.contains("-o out.dendro"), "{args:?}:\n{stderr}");
        }
        let left: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert!(left.is_empty(), "{args:?}: nothing is written: {left:?}");
    }
}

/// A recorder that cannot create its output must exit non-zero.
///
/// Regression this guards: a failure on the output path printed an error and
/// returned without setting `recording_failed`, so `run()` fell through to
/// exit 0 — and `rezolus record -o out.dendro && analyze out.dendro` then
/// succeeded on whatever was already at that path.
///
/// The recorder claims the output path with `O_EXCL` before the first tick,
/// so an unusable path fails at startup rather than at finalize — and it must
/// fail *loudly*. A supervisor or a `record && analyze` pipeline can only see
/// the exit code.
#[test]
fn a_recorder_that_cannot_create_its_output_exits_nonzero() {
    let port = spawn_fake_agent();
    let dir = tempfile::tempdir().expect("failed to create a temp dir");

    // An existing directory: `create_new` on it cannot succeed.
    let output = dir.path().join("out.dendro");
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
        stderr.contains("failed to start the recording"),
        "expected a startup failure, got stderr:\n{stderr}"
    );
    assert!(
        !out.status.success(),
        "an archive that could not be created must not exit 0 (status {:?})\nstderr:\n{stderr}",
        out.status.code()
    );
}

/// A bare `rezolus record` writes `rezolus.dendro`, and `recording metadata`
/// describes it as a finalized dendro archive.
///
/// The output is decided inside `run()`, which owns `std::process::exit`, so
/// the only place the default is observable is the file the real binary
/// leaves behind.
#[test]
fn a_default_recording_is_a_dendro_and_describes_itself() {
    let port = spawn_fake_agent();
    let dir = tempfile::tempdir().expect("failed to create a temp dir");

    let out = Command::new(env!("CARGO_BIN_EXE_rezolus"))
        .current_dir(dir.path())
        .arg("record")
        .arg("--url")
        .arg(format!("http://127.0.0.1:{port}"))
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

    let output = dir.path().join("rezolus.dendro");
    assert_eq!(
        metriken_storage::DendroCatalog::is_archive(&output),
        Ok(true),
        "the default output is a dendro archive at rezolus.dendro"
    );

    let described = Command::new(env!("CARGO_BIN_EXE_rezolus"))
        .arg("recording")
        .arg("metadata")
        .arg("-i")
        .arg(&output)
        .output()
        .expect("failed to run rezolus recording metadata");
    let stdout = String::from_utf8_lossy(&described.stdout);
    assert!(
        described.status.success(),
        "metadata must describe the archive\nstdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&described.stderr)
    );
    assert!(stdout.contains("dendro archive"), "{stdout}");
    assert!(
        stdout.contains("fake/ops"),
        "the recorded group must be listed: {stdout}"
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
/// noticed, dropping every unsealed segment.
#[test]
fn sigterm_exits_promptly_at_a_long_interval() {
    let port = spawn_fake_agent();
    let dir = tempfile::tempdir().expect("failed to create a temp dir");

    let mut child = Command::new(env!("CARGO_BIN_EXE_rezolus"))
        .arg("record")
        .arg("--url")
        .arg(format!("http://127.0.0.1:{port}"))
        .arg("-o")
        .arg(dir.path().join("out.dendro"))
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
    let output = dir.path().join("out.dendro");

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
    assert!(output.exists(), "the archive should have been written");
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
    let output = dir.path().join("fleet.dendro");

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

/// Run `rezolus record` wrapping `sh -c 'sleep 0.5'` against a fake agent at
/// a 100ms interval, writing to `output`. Returns the process output.
fn record_wrapped(output: &std::path::Path, extra: &[&str]) -> std::process::Output {
    let port = spawn_fake_agent();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rezolus"));
    cmd.arg("record")
        .arg("--url")
        .arg(format!("http://127.0.0.1:{port}"))
        .arg("--interval")
        .arg("100ms")
        .arg("-o")
        .arg(output);
    for arg in extra {
        cmd.arg(arg);
    }
    cmd.arg("--").arg("sh").arg("-c").arg("sleep 0.5");
    cmd.output().expect("failed to run rezolus record")
}

/// The `events` payload of the one recording in a `.dendro`, parsed.
fn run_events(output: &std::path::Path) -> Vec<dashboard::Event> {
    use metriken_storage::Catalog;
    let catalog = metriken_storage::DendroCatalog::open(output).expect("the archive opens");
    let sources = catalog.sources().expect("the catalog reads");
    assert_eq!(sources.len(), 1, "one endpoint, one source");
    let raw = sources[0]
        .metadata
        .get("events")
        .expect("a wrapped run writes the events key");
    let payload: dashboard::Events = serde_json::from_str(raw).expect("the payload parses");
    payload.events
}

/// Check the two run events a wrapped run writes: kinds, a shared uuid in
/// the ids, and a span that matches the command's own runtime.
fn assert_run_events(events: &[dashboard::Event]) -> (dashboard::Event, dashboard::Event) {
    assert_eq!(events.len(), 2, "run_start and run_end: {events:?}");
    let start = events[0].clone();
    let end = events[1].clone();
    assert_eq!(start.kind.as_deref(), Some("run_start"));
    assert_eq!(end.kind.as_deref(), Some("run_end"));
    assert_eq!(start.description, "sh");
    assert_eq!(end.description, "sh exited 0");

    let start_id = start.id.as_deref().expect("run_start has an id");
    let end_id = end.id.as_deref().expect("run_end has an id");
    let uuid = start_id
        .strip_prefix("run:")
        .and_then(|s| s.strip_suffix(":start"))
        .expect("id is run:<uuid>:start");
    assert_eq!(end_id, format!("run:{uuid}:end"));
    assert_eq!(uuid.len(), 36, "a canonical v4 uuid: {uuid}");

    // The child slept 500ms. Neither bound is exact. The start stamp is
    // taken when `spawn()` returns to the recorder, and the kernel may run
    // the child first: the shell can already be inside its `sleep` by then,
    // so the measured span can fall a few milliseconds short of the sleep
    // (CI measured 499.2 ms). The upper bound is loose on purpose: a loaded
    // host can delay the shell's start and the exit's delivery, and a tight
    // window here was a flake waiting to happen.
    let span = Duration::from_nanos(end.timestamp - start.timestamp);
    assert!(
        span >= Duration::from_millis(450) && span <= Duration::from_millis(2500),
        "run_end - run_start must be the command's runtime, got {span:?}"
    );
    (start, end)
}

/// A wrapped run writes the two events into the source's metadata through
/// the dendro writer (`SourceRecorder::update_metadata`): `run_start` at
/// spawn and `run_end` at exit, paired by one uuid, with the program name and
/// no argument list.
#[test]
fn a_wrapped_dendro_run_writes_run_start_and_run_end_events() {
    let dir = tempfile::tempdir().expect("failed to create a temp dir");
    let output = dir.path().join("out.dendro");
    let out = record_wrapped(&output, &[]);
    assert!(
        out.status.success(),
        "the wrapped run exits with the command's status (0)\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let (start, end) = assert_run_events(&run_events(&output));
    assert!(
        start.details.is_none(),
        "the argument list is not recorded by default: {:?}",
        start.details
    );
    assert!(end.details.is_some(), "run_end says how the command ended");
}

/// `--record-command-line` puts the full argument list, space-joined, in the
/// `run_start` event's details.
#[test]
fn record_command_line_stores_the_argument_list_in_run_start() {
    let dir = tempfile::tempdir().expect("failed to create a temp dir");
    let output = dir.path().join("out.dendro");
    let out = record_wrapped(&output, &["--record-command-line"]);
    assert!(
        out.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let (start, _end) = assert_run_events(&run_events(&output));
    assert_eq!(start.details.as_deref(), Some("sh -c sleep 0.5"));
}

/// Run `rezolus record --url url` wrapping `sh -c <script>` at `interval`,
/// writing to `output`.
fn record_wrapped_script(
    output: &std::path::Path,
    url: &str,
    interval: &str,
    script: &str,
) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_rezolus"))
        .arg("record")
        .arg("--url")
        .arg(url)
        .arg("--interval")
        .arg(interval)
        .arg("-o")
        .arg(output)
        .arg("--")
        .arg("sh")
        .arg("-c")
        .arg(script)
        .output()
        .expect("failed to run rezolus record")
}

/// A scraped endpoint whose wrapped command exits at once is still recorded:
/// the exit is followed by one scrape, so the archive has a row and both
/// events, and the run exits 0.
///
/// Regression: the exit arm went straight back to the loop top, which broke
/// out before the scrape, so `-- true` produced "command exited before any
/// metrics were recorded", no file, and exit 1.
#[test]
fn a_command_that_exits_at_once_still_gets_one_scrape() {
    let exporter = spawn_fake_exporter();
    let dir = tempfile::tempdir().expect("failed to create a temp dir");
    let output = dir.path().join("out.dendro");
    let url = format!("http://127.0.0.1:{exporter}/metrics");
    let out = record_wrapped_script(&output, &url, "200ms", "exit 0");
    assert!(
        out.status.success(),
        "an instant exit is a recorded run, exit 0\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let (rows, _, kinds) = dendro_rows(&output, "prometheus/scrape");
    assert!(rows >= 1, "the final scrape wrote a row, got {rows}");
    assert_eq!(kinds, vec!["run_start", "run_end"]);
}

/// The interval a scraped endpoint's command exited in is sampled: the last
/// row is no older than one interval before `run_end`.
///
/// Regression: the exit wake ended the recording without the scrape that
/// follows the tick, losing the tail of the run (`-- sleep 2.4` at 1s wrote
/// 2 rows where 3 were due).
#[test]
fn the_interval_the_command_exited_in_is_sampled() {
    let exporter = spawn_fake_exporter();
    let dir = tempfile::tempdir().expect("failed to create a temp dir");
    let output = dir.path().join("out.dendro");
    let url = format!("http://127.0.0.1:{exporter}/metrics");
    let out = record_wrapped_script(&output, &url, "200ms", "sleep 0.5");
    assert!(
        out.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let (_, last_ts, _) = dendro_rows(&output, "prometheus/scrape");
    let last_ts = last_ts.expect("a last row");
    let run_end = run_events(&output)
        .into_iter()
        .find(|e| e.kind.as_deref() == Some("run_end"))
        .expect("run_end");
    let interval_ns = Duration::from_millis(200).as_nanos() as u64;
    assert!(
        last_ts + interval_ns >= run_end.timestamp,
        "the last row ({last_ts}) must be within one interval of run_end ({})",
        run_end.timestamp
    );
}

/// Rows in `table` of the one source in a `.dendro`, and the newest row's
/// timestamp, sealed and live together.
fn dendro_rows(output: &std::path::Path, table: &str) -> (u64, Option<u64>, Vec<String>) {
    use metriken_storage::Catalog;
    let catalog = metriken_storage::DendroCatalog::open(output).expect("the archive opens");
    let sources = catalog.sources().expect("the catalog reads");
    assert_eq!(sources.len(), 1, "one endpoint, one source");
    let id = sources[0].id;
    let sealed = catalog.segment_span(id, table).unwrap().1;
    let live = catalog.live_wal_span(id, table).unwrap();
    let kinds = sources[0]
        .metadata
        .get("events")
        .map(|raw| {
            let payload: dashboard::Events = serde_json::from_str(raw).expect("the payload parses");
            payload.events.into_iter().filter_map(|e| e.kind).collect()
        })
        .unwrap_or_default();
    (
        sealed.rows + live.rows,
        sealed.last_ts.max(live.last_ts),
        kinds,
    )
}

/// Streamed form of [`a_command_that_exits_at_once_still_gets_one_scrape`]:
/// a command that exits before the agent's first frame is still recorded,
/// because the recorder waits for that frame rather than for a fixed grace.
///
/// Regression: the grace was one interval capped at two seconds, and stream
/// frames are paced by the agent, so `--interval 3s -- true` finished before
/// the first frame: "command exited before any metrics were recorded", no
/// file, exit 1.
#[test]
fn a_dendro_command_that_exits_at_once_still_gets_one_frame() {
    let dir = tempfile::tempdir().expect("failed to create a temp dir");
    let output = dir.path().join("out.dendro");
    let agent = format!("http://127.0.0.1:{}", spawn_fake_agent());
    let out = record_wrapped_script(&output, &agent, "3s", "exit 0");
    assert!(
        out.status.success(),
        "an instant exit is a recorded run, exit 0\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let (rows, _, kinds) = dendro_rows(&output, "fake/ops");
    assert!(rows >= 1, "the first frame was recorded, got {rows} rows");
    assert_eq!(kinds, vec!["run_start", "run_end"]);
}

/// Streamed form of [`the_interval_the_command_exited_in_is_sampled`]: the
/// frame covering the moment the command exited is recorded, so the last row
/// is no older than one interval before `run_end`.
///
/// At a 3s interval and a 3.5s command, the frame that covers the exit comes
/// 2.5s after it, past the old two-second grace.
#[test]
fn the_interval_the_dendro_command_exited_in_is_recorded() {
    let dir = tempfile::tempdir().expect("failed to create a temp dir");
    let output = dir.path().join("out.dendro");
    let agent = format!("http://127.0.0.1:{}", spawn_fake_agent());
    let out = record_wrapped_script(&output, &agent, "3s", "sleep 3.5");
    assert!(
        out.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let (rows, last_ts, _) = dendro_rows(&output, "fake/ops");
    assert!(rows >= 2, "{rows} rows");
    let last_ts = last_ts.expect("a last row");

    use metriken_storage::Catalog;
    let catalog = metriken_storage::DendroCatalog::open(&output).unwrap();
    let raw = catalog.sources().unwrap()[0]
        .metadata
        .get("events")
        .cloned()
        .expect("events");
    let payload: dashboard::Events = serde_json::from_str(&raw).expect("the payload parses");
    let run_end = payload
        .events
        .iter()
        .find(|e| e.kind.as_deref() == Some("run_end"))
        .expect("run_end");
    let interval_ns = Duration::from_secs(3).as_nanos() as u64;
    assert!(
        last_ts + interval_ns >= run_end.timestamp,
        "the last row ({last_ts}) must be within one interval of run_end ({})",
        run_end.timestamp
    );
    assert!(
        last_ts >= run_end.timestamp,
        "the frame after the exit was waited for: last row {last_ts}, run_end {}",
        run_end.timestamp
    );
}

/// An agent that comes up after the recording started, and cannot serve its
/// stream, is refused on its own: the other endpoints keep recording and the
/// archive is finalized, and the run exits 1 so a pipeline notices.
///
/// Regression: the refusal ended the whole run and left the archive
/// unfinalized.
#[test]
fn a_late_agent_without_a_stream_is_left_out_and_the_rest_finalize() {
    use metriken_storage::Catalog;

    let (exporter, hits) = spawn_fake_exporter_counted("http_requests_total");
    // Up once the exporter has been scraped twice: the startup probe, then
    // the first tick. By then the archive is open.
    let agent = spawn_fake_agent_without_stream_when("5.20.0", move || {
        hits.load(std::sync::atomic::Ordering::SeqCst) >= 2
    });
    let dir = tempfile::tempdir().expect("failed to create a temp dir");
    let output = dir.path().join("late.dendro");

    let out = Command::new(env!("CARGO_BIN_EXE_rezolus"))
        .arg("record")
        .arg("--endpoint")
        .arg(format!("http://127.0.0.1:{exporter}/metrics,source=svc"))
        .arg("--endpoint")
        .arg(format!("http://127.0.0.1:{agent},source=old"))
        .arg("-o")
        .arg(&output)
        .arg("--interval")
        .arg("200ms")
        .arg("--duration")
        .arg("2s")
        .output()
        .expect("failed to run rezolus record");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "exits 1:\n{stderr}");
    for needle in [
        "Rezolus 5.20.0",
        "predates",
        "excluded from this recording",
        "5.x recorder",
        &format!("127.0.0.1:{agent}"),
        &format!(
            "error: http://127.0.0.1:{agent}/ (old) was refused mid-run and has no rows in {}\n",
            output.display()
        ),
        "1 of 2 endpoints refused; the archive holds the rest and is finalized",
    ] {
        assert!(stderr.contains(needle), "{needle:?} in:\n{stderr}");
    }
    assert!(
        !stderr.contains("-o out.rez"),
        "no format switch advised:\n{stderr}"
    );

    let catalog = metriken_storage::DendroCatalog::open(&output).expect("the archive opens");
    let sources = catalog.sources().expect("the catalog reads");
    assert_eq!(
        sources.len(),
        1,
        "only the exporter is recorded: {sources:?}"
    );
    assert!(sources[0].complete, "the recording is finalized");
    let id = sources[0].id;
    let rows = catalog
        .segment_span(id, "prometheus/scrape")
        .unwrap()
        .1
        .rows
        + catalog.live_wal_span(id, "prometheus/scrape").unwrap().rows;
    assert!(rows > 0, "the exporter kept recording");
    drop(catalog);

    let described = Command::new(env!("CARGO_BIN_EXE_rezolus"))
        .arg("recording")
        .arg("metadata")
        .arg("-i")
        .arg(&output)
        .output()
        .expect("failed to run rezolus recording metadata");
    let stdout = String::from_utf8_lossy(&described.stdout);
    assert!(described.status.success(), "{stdout}");
    assert!(
        !stdout.contains("not cleanly finalized"),
        "a clean finalize: {stdout}"
    );
}

/// An agent that streamed, then answers its reconnect with a 404, is refused
/// on its own as a late agent is: its recording stops with the rows it had
/// and is finalized, the other endpoint keeps recording, and the run exits 1.
///
/// Regression: a refusal on a reconnect ended the whole run.
#[test]
fn an_agent_refused_on_reconnect_keeps_its_rows_and_the_rest_finalize() {
    use metriken_storage::Catalog;

    let exporter = spawn_fake_exporter();
    let agent = spawn_fake_agent_that_stops_streaming(3);
    let dir = tempfile::tempdir().expect("failed to create a temp dir");
    let output = dir.path().join("reconnect.dendro");

    let out = Command::new(env!("CARGO_BIN_EXE_rezolus"))
        .arg("record")
        .arg("--endpoint")
        .arg(format!("http://127.0.0.1:{exporter}/metrics,source=svc"))
        .arg("--endpoint")
        .arg(format!("http://127.0.0.1:{agent},source=agent"))
        .arg("-o")
        .arg(&output)
        .arg("--interval")
        .arg("200ms")
        .arg("--duration")
        .arg("3s")
        .output()
        .expect("failed to run rezolus record");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "exits 1:\n{stderr}");
    let dropped = format!(
        "error: http://127.0.0.1:{agent}/ (agent) was refused mid-run and has no rows in {} after ",
        output.display()
    );
    for needle in [
        "Rezolus 6.0.0",
        "/metrics/stream answered HTTP 404",
        "recording stops here",
        dropped.as_str(),
        "1 of 2 endpoints refused; the archive holds the rest and is finalized",
    ] {
        assert!(stderr.contains(needle), "{needle:?} in:\n{stderr}");
    }
    assert!(!stderr.contains("predates"), "a 6.0 agent:\n{stderr}");

    let catalog = metriken_storage::DendroCatalog::open(&output).expect("the archive opens");
    let sources = catalog.sources().expect("the catalog reads");
    assert_eq!(sources.len(), 2, "{sources:?}");
    let rows = |id: i64, table: &str| {
        catalog.segment_span(id, table).unwrap().1.rows
            + catalog.live_wal_span(id, table).unwrap().rows
    };
    let by_source = |name: &str| {
        sources
            .iter()
            .find(|s| s.labels.get("source").map(String::as_str) == Some(name))
            .unwrap_or_else(|| panic!("a recording for {name}: {sources:?}"))
    };
    let agent_src = by_source("agent");
    assert!(
        agent_src.complete,
        "the dropped agent's recording is finalized"
    );
    let agent_rows = rows(agent_src.id, "fake/ops");
    assert!(
        (1..=3).contains(&agent_rows),
        "the agent keeps the rows it streamed, and no more: {agent_rows}"
    );
    let svc = by_source("svc");
    assert!(svc.complete, "the exporter's recording is finalized");
    let svc_rows = rows(svc.id, "prometheus/scrape");
    // Scrapes after the refusal: more than the agent's three frames' worth.
    assert!(
        svc_rows > agent_rows + 3,
        "the exporter kept recording: {svc_rows} rows"
    );
}

/// A run whose every endpoint has been refused mid-run ends on its own: with
/// no `--duration` it would otherwise record nothing until ctrl-c. The
/// archive is finalized with the rows the agent streamed before its
/// reconnect was refused, and the run exits 1.
#[test]
fn a_run_whose_every_endpoint_is_refused_ends_on_its_own() {
    use metriken_storage::Catalog;

    let agent = spawn_fake_agent_that_stops_streaming(3);
    let dir = tempfile::tempdir().expect("failed to create a temp dir");
    let output = dir.path().join("alone.dendro");

    let mut child = Command::new(env!("CARGO_BIN_EXE_rezolus"))
        .arg("record")
        .arg("--url")
        .arg(format!("http://127.0.0.1:{agent}"))
        .arg("-o")
        .arg(&output)
        .arg("--interval")
        .arg("200ms")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to run rezolus record");
    // Three frames, a one-second reconnect delay and the refusal take about
    // two seconds; a run that does not end on its own is killed here and
    // fails the test rather than hanging it.
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the run did not end after every endpoint was refused");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert_eq!(status.code(), Some(1), "exits 1:\n{stderr}");
    assert!(
        stderr.contains("1 of 1 endpoints refused; the archive holds the rest and is finalized"),
        "{stderr}"
    );

    let catalog = metriken_storage::DendroCatalog::open(&output).expect("the archive opens");
    let sources = catalog.sources().expect("the catalog reads");
    assert_eq!(sources.len(), 1, "{sources:?}");
    assert!(sources[0].complete, "the archive is finalized");
    let id = sources[0].id;
    let rows = catalog.segment_span(id, "fake/ops").unwrap().1.rows
        + catalog.live_wal_span(id, "fake/ops").unwrap().rows;
    assert!((1..=3).contains(&rows), "the rows it streamed: {rows}");
}

/// A Prometheus exporter that serves `/metrics` and answers 404 to every
/// other path, `/` included, as most exporters do.
fn spawn_strict_exporter() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("failed to bind the fake exporter");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let mut tick = 0u64;
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = [0u8; 8192];
            let Ok(n) = stream.read(&mut buf) else {
                continue;
            };
            let req = String::from_utf8_lossy(&buf[..n]);
            let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
            if path != "/metrics" {
                let _ = stream.write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
                continue;
            }
            tick += 1;
            let body = format!("# TYPE served_total counter\nserved_total {tick}\n");
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain; version=0.0.4\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(body.as_bytes());
        }
    });
    port
}

/// Label values as an exporter writes them in exposition text, each with the
/// value the format says it stands for. `\\`, `\"` and `\n` are the format's
/// escapes.
const LABEL_VALUE_CASES: &[(&str, &str, &str)] = &[
    ("plain", "abc", "abc"),
    ("eq", "a=b", "a=b"),
    ("eq_end", "a=", "a="),
    ("pairs", "x=1,y=2", "x=1,y=2"),
    ("comma", "a,b", "a,b"),
    ("lbrace", "a{b", "a{b"),
    ("rbrace", "a}b", "a}b"),
    ("quote", r#"a\"b"#, "a\"b"),
    ("backslash", r"a\\b", "a\\b"),
    ("newline", r"a\nb", "a\nb"),
    ("lt_gt", "<b>", "<b>"),
    ("space", "a b", "a b"),
    ("amp", "a&b", "a&b"),
    ("apos", "a'b", "a'b"),
    ("unicode", "ü€", "ü€"),
    ("empty", "", ""),
];

/// A fake exporter serving one `xss_total` series per `LABEL_VALUE_CASES`
/// row, told apart by its `case` label.
fn spawn_label_value_exporter() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("failed to bind the fake exporter");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let mut tick = 0u64;
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = [0u8; 8192];
            if !matches!(stream.read(&mut buf), Ok(n) if n > 0) {
                continue;
            }
            tick += 1;
            let mut body = String::from("# TYPE xss_total counter\n");
            for (case, text, _) in LABEL_VALUE_CASES {
                body.push_str(&format!(
                    "xss_total{{case=\"{case}\",op=\"{text}\"}} {tick}\n"
                ));
            }
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain; version=0.0.4\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(body.as_bytes());
            let _ = stream.flush();
        }
    });
    port
}

/// A Prometheus label value is stored as the exposition text spells it,
/// read back through the reader `view` and `mcp` use. Through
/// metriken-recorder 0.3.0, prometheus-parse split the label block on every
/// `,` and `=`: `op="a=b"` was stored with no `op` label, `op="a,b"` as
/// `op="a"`, `op="x=1,y=2"` as `y="2"`, the `a}b` series not at all, and
/// escapes were stored as written.
#[test]
#[ignore = "needs metriken-recorder > 0.3.0 (exposition parser that reads label values per the format)"]
fn a_prometheus_label_value_is_stored_as_the_exposition_spells_it() {
    use metriken_query::MetricsSource;

    let port = spawn_label_value_exporter();
    let dir = tempfile::tempdir().expect("failed to create a temp dir");
    let output = dir.path().join("labels.dendro");
    let out = Command::new(env!("CARGO_BIN_EXE_rezolus"))
        .arg("record")
        .arg("--endpoint")
        .arg(format!("http://127.0.0.1:{port}/metrics,source=svc"))
        .arg("-o")
        .arg(&output)
        .arg("--interval")
        .arg("100ms")
        .arg("--duration")
        .arg("500ms")
        .output()
        .expect("failed to run rezolus record");
    assert!(
        out.status.success(),
        "recording exits 0:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let recordings = rez::reader::RezReader::open_recordings(
        &output,
        metriken_storage::BufferPool::new(8 * 1024 * 1024),
    )
    .expect("the archive opens");
    assert_eq!(recordings.len(), 1);
    let series = recordings[0].1.counter_labels("xss_total");
    let got: std::collections::BTreeMap<String, Option<String>> = series
        .iter()
        .filter_map(|l| Some((l.get("case")?.clone(), l.get("op").cloned())))
        .collect();
    let want: std::collections::BTreeMap<String, Option<String>> = LABEL_VALUE_CASES
        .iter()
        .map(|(case, _, value)| (case.to_string(), Some(value.to_string())))
        .collect();
    assert_eq!(got, want, "every series, with its op value: {series:?}");
    assert!(
        series.iter().all(|l| !l.contains_key("y")),
        "no label is made out of a value: {series:?}"
    );
}

/// The labels, the metadata, and the row count in `table` of the one
/// recording in a `.dendro`.
type Recording = (
    std::collections::BTreeMap<String, String>,
    std::collections::BTreeMap<String, String>,
    u64,
);

fn the_recording(output: &std::path::Path, table: &str) -> Recording {
    use metriken_storage::Catalog;
    let catalog = metriken_storage::DendroCatalog::open(output).expect("the archive opens");
    let sources = catalog.sources().expect("the catalog reads");
    assert_eq!(sources.len(), 1, "one endpoint, one source: {sources:?}");
    let id = sources[0].id;
    let rows = catalog.segment_span(id, table).unwrap().1.rows
        + catalog.live_wal_span(id, table).unwrap().rows;
    (
        sources[0].labels.clone().into_iter().collect(),
        sources[0].metadata.clone().into_iter().collect(),
        rows,
    )
}

/// A Prometheus endpoint given by its root URL, with no source, is detected
/// by stream: its stream route 404s, `/status` and `/` answer without naming
/// an agent, and it is scraped at `/metrics` under a source inferred from the
/// URL. GO check 3 of metriken's recorder entry.
#[test]
fn a_prometheus_endpoint_at_the_root_is_scraped_at_metrics() {
    let port = spawn_strict_exporter();
    let dir = tempfile::tempdir().expect("failed to create a temp dir");
    let output = dir.path().join("prom.dendro");

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
    assert!(out.status.success(), "exits 0:\n{stderr}");
    assert!(
        stderr.contains(&format!(
            "scraped as Prometheus at http://127.0.0.1:{port}/metrics"
        )),
        "{stderr}"
    );
    let (labels, _, rows) = the_recording(&output, "prometheus/scrape");
    assert_eq!(
        labels.get("source").map(String::as_str),
        Some(format!("127.0.0.1-{port}").as_str()),
        "{labels:?}"
    );
    assert!(rows > 0, "the scrapes were recorded");
}

/// An endpoint that answers 503 to everything at startup is pending, not
/// refused and not scraped; once it serves its stream it is streamed and
/// recorded beside the endpoint that was up from the start. GO check 3 of
/// metriken's recorder entry.
#[test]
fn an_endpoint_answering_5xx_is_pending_then_streamed() {
    use metriken_storage::Catalog;

    let (exporter, hits) = spawn_fake_exporter_counted("http_requests_total");
    // Up once the exporter has been asked three times: after startup's
    // detection (one request) and two ticks' scrapes.
    let agent = spawn_fixture(
        Streams::Always,
        None,
        Names::Metriken,
        Fixture {
            up: Some(std::sync::Arc::new(move || {
                hits.load(std::sync::atomic::Ordering::SeqCst) >= 3
            })),
            ..Fixture::default()
        },
    );
    let dir = tempfile::tempdir().expect("failed to create a temp dir");
    let output = dir.path().join("late.dendro");

    let out = Command::new(env!("CARGO_BIN_EXE_rezolus"))
        .arg("record")
        .arg("--endpoint")
        .arg(format!("http://127.0.0.1:{exporter}/metrics,source=svc"))
        .arg("--endpoint")
        .arg(format!("http://127.0.0.1:{agent},source=late"))
        .arg("-o")
        .arg(&output)
        .arg("--interval")
        .arg("200ms")
        .arg("--duration")
        .arg("3s")
        .output()
        .expect("failed to run rezolus record");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "exits 0:\n{stderr}");
    for needle in [
        "returned HTTP 503",
        "will retry each tick",
        "now reachable, subscribed to its replication stream",
    ] {
        assert!(stderr.contains(needle), "{needle:?} in:\n{stderr}");
    }
    assert!(!stderr.contains("refused"), "{stderr}");

    let catalog = metriken_storage::DendroCatalog::open(&output).expect("the archive opens");
    let sources = catalog.sources().expect("the catalog reads");
    assert_eq!(sources.len(), 2, "{sources:?}");
    let late = sources
        .iter()
        .find(|s| s.labels.get("source").map(String::as_str) == Some("late"))
        .unwrap_or_else(|| panic!("a recording for the late endpoint: {sources:?}"));
    assert!(late.complete, "finalized");
    let rows = catalog.segment_span(late.id, "fake/ops").unwrap().1.rows
        + catalog.live_wal_span(late.id, "fake/ops").unwrap().rows;
    assert!(rows > 0, "the late endpoint was streamed");
}

/// A producer that is not a Rezolus agent and names itself in its stream
/// handshake is recorded under that name and with its version, with no
/// `--endpoint url,source=`. GO check 4 of metriken's recorder entry.
#[test]
fn a_producers_handshake_names_its_recording() {
    let port = spawn_fixture(
        Streams::Always,
        None,
        Names::Metriken,
        Fixture {
            labels: &[("source", "cachecannon")],
            metadata: &[("version", "0.9.1"), ("workload", "kv")],
            ..Fixture::default()
        },
    );
    let dir = tempfile::tempdir().expect("failed to create a temp dir");
    let output = dir.path().join("producer.dendro");

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
    assert!(out.status.success(), "exits 0:\n{stderr}");
    assert!(!stderr.contains("no source name specified"), "{stderr}");

    let (labels, metadata, rows) = the_recording(&output, "fake/ops");
    assert_eq!(
        labels.get("source").map(String::as_str),
        Some("cachecannon"),
        "{labels:?}"
    );
    assert_eq!(
        metadata.get("source").map(String::as_str),
        Some("cachecannon"),
        "{metadata:?}"
    );
    assert_eq!(
        metadata.get("version").map(String::as_str),
        Some("0.9.1"),
        "{metadata:?}"
    );
    assert_eq!(
        metadata.get("workload").map(String::as_str),
        Some("kv"),
        "{metadata:?}"
    );
    assert!(rows > 0, "the stream was recorded");
}

/// A server that accepts every connection and never answers, as a process
/// that has bound its port but is stuck.
fn spawn_silent_server() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("failed to bind the silent server");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            std::thread::spawn(move || {
                let mut buf = [0u8; 8192];
                while matches!(stream.read(&mut buf), Ok(n) if n > 0) {}
            });
        }
    });
    port
}

/// The rows in `table` of the recording whose `source` label is `source`.
fn rows_of(output: &std::path::Path, source: &str, table: &str) -> u64 {
    use metriken_storage::Catalog;
    let catalog = metriken_storage::DendroCatalog::open(output).expect("the archive opens");
    let sources = catalog.sources().expect("the catalog reads");
    let found = sources
        .iter()
        .find(|s| s.labels.get("source").map(String::as_str) == Some(source))
        .unwrap_or_else(|| panic!("a recording for {source}: {sources:?}"));
    catalog.segment_span(found.id, table).unwrap().1.rows
        + catalog.live_wal_span(found.id, table).unwrap().rows
}

/// A pending endpoint that accepts connections and never answers does not
/// slow the ticks of the endpoint beside it: its detection runs off the
/// tick. At its end the run names it as having no recording.
///
/// Regression: each tick waited on the pending endpoint's detection, the
/// stream connect's timeout and then the probe hook's (4 s at a 1 s
/// interval), so the other endpoint was scraped once per detection.
#[test]
fn a_pending_endpoint_that_never_answers_does_not_stall_the_ticks() {
    let exporter = spawn_fake_exporter();
    let silent = spawn_silent_server();
    let dir = tempfile::tempdir().expect("failed to create a temp dir");
    let output = dir.path().join("stall.dendro");

    let out = Command::new(env!("CARGO_BIN_EXE_rezolus"))
        .arg("record")
        .arg("--endpoint")
        .arg(format!("http://127.0.0.1:{exporter}/metrics,source=svc"))
        .arg("--endpoint")
        .arg(format!("http://127.0.0.1:{silent},source=stuck"))
        .arg("-o")
        .arg(&output)
        .arg("--interval")
        .arg("200ms")
        .arg("--duration")
        .arg("3s")
        .output()
        .expect("failed to run rezolus record");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "exits 0:\n{stderr}");
    assert!(
        stderr.contains(&format!(
            "http://127.0.0.1:{silent}/ (stuck) did not become recordable during the run and \
             has no recording in {}",
            output.display()
        )),
        "{stderr}"
    );
    // 3 s at 200 ms is 15 ticks; a tick that waited on the detection got
    // one scrape per 4 s.
    let rows = rows_of(&output, "svc", "prometheus/scrape");
    assert!(
        rows >= 10,
        "the exporter is scraped every tick: {rows} rows"
    );
}

/// `--metadata systeminfo=` is kept through an agent restart, as
/// `--metadata version=` is: the restarted agent's own systeminfo does not
/// replace it.
#[test]
fn a_systeminfo_the_user_set_survives_an_agent_restart() {
    use metriken_storage::Catalog;

    let agent = spawn_fixture(
        Streams::Restarts(5),
        Some(BEFORE_RESTART.1),
        Names::Metriken,
        Fixture {
            systeminfo: Some(r#"{"hostname":"agent-host"}"#),
            ..Fixture::default()
        },
    );
    let dir = tempfile::tempdir().expect("failed to create a temp dir");
    let output = dir.path().join("pinned.dendro");

    let out = Command::new(env!("CARGO_BIN_EXE_rezolus"))
        .arg("record")
        .arg("--url")
        .arg(format!("http://127.0.0.1:{agent}"))
        .arg("--metadata")
        .arg("systeminfo=mine")
        .arg("-o")
        .arg(&output)
        .arg("--interval")
        .arg("100ms")
        .arg("--duration")
        .arg("3s")
        .output()
        .expect("failed to run rezolus record");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "exits 0:\n{stderr}");
    assert!(
        stderr.contains("now records the agent restart"),
        "the restart is written into the metadata:\n{stderr}"
    );

    let catalog = metriken_storage::DendroCatalog::open(&output).expect("the archive opens");
    let sources = catalog.sources().expect("the catalog reads");
    assert_eq!(sources.len(), 1, "{sources:?}");
    let metadata = &sources[0].metadata;
    assert_eq!(
        metadata.get("systeminfo").map(String::as_str),
        Some("mine"),
        "{metadata:?}"
    );
    assert_eq!(
        metadata.get("version").map(String::as_str),
        Some(AFTER_RESTART.1),
        "an unpinned key takes the new agent's: {metadata:?}"
    );
}

/// An endpoint that comes up mid-run and opens its stream, but whose
/// metadata read has not returned when the run ends, is named at the end as
/// having no recording rather than left out in silence.
#[test]
fn an_endpoint_still_reading_its_metadata_at_the_end_is_named() {
    use metriken_storage::Catalog;

    let (exporter, hits) = spawn_fake_exporter_counted("http_requests_total");
    let dir = tempfile::tempdir().expect("failed to create a temp dir");
    let flag = dir.path().join("metadata-requested");
    let agent = spawn_fixture(
        Streams::Always,
        None,
        Names::Metriken,
        Fixture {
            up: Some(std::sync::Arc::new(move || {
                hits.load(std::sync::atomic::Ordering::SeqCst) >= 3
            })),
            hang_metadata: true,
            on_hang: Some(flag.clone()),
            ..Fixture::default()
        },
    );
    let output = dir.path().join("activating.dendro");

    // The run is a command that exits once the agent has received the
    // metadata request that never returns. The read is bounded by the scrape
    // timeout (2 s at this interval) from that moment. The run's end waits
    // only for frames from recorded streams, and this endpoint is not
    // recorded yet, so the run ends first however long the agent took to come
    // up. `--duration` ends a run whose endpoint never comes up, which then
    // fails the exit-status assertion.
    let out = Command::new(env!("CARGO_BIN_EXE_rezolus"))
        .arg("record")
        .arg("--endpoint")
        .arg(format!("http://127.0.0.1:{exporter}/metrics,source=svc"))
        .arg("--endpoint")
        .arg(format!("http://127.0.0.1:{agent},source=late"))
        .arg("-o")
        .arg(&output)
        .arg("--interval")
        .arg("200ms")
        .arg("--duration")
        .arg("30s")
        .arg("--")
        .arg("sh")
        .arg("-c")
        .arg("until [ -e \"$1\" ]; do sleep 0.02; done")
        .arg("sh")
        .arg(&flag)
        .output()
        .expect("failed to run rezolus record");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "exits 0:\n{stderr}");
    assert!(
        stderr.contains("now reachable, subscribed to its replication stream"),
        "{stderr}"
    );
    assert!(
        stderr.contains(&format!(
            "http://127.0.0.1:{agent}/ (late) opened its stream, but the run ended before its \
             metadata arrived; it has no recording in {}",
            output.display()
        )),
        "{stderr}"
    );
    let catalog = metriken_storage::DendroCatalog::open(&output).expect("the archive opens");
    let sources = catalog.sources().expect("the catalog reads");
    assert_eq!(
        sources.len(),
        1,
        "only the exporter is recorded: {sources:?}"
    );
}

/// `--metadata descriptions=` is kept on a Prometheus endpoint: the scrape's
/// HELP text does not replace it.
#[test]
fn a_descriptions_the_user_set_survives_the_help_text() {
    let port = spawn_fake_exporter_named("widgets_total");
    let dir = tempfile::tempdir().expect("failed to create a temp dir");
    let output = dir.path().join("pinned-help.dendro");

    let out = Command::new(env!("CARGO_BIN_EXE_rezolus"))
        .arg("record")
        .arg("--endpoint")
        .arg(format!("http://127.0.0.1:{port}/metrics,source=svc"))
        .arg("--metadata")
        .arg("descriptions=mine")
        .arg("-o")
        .arg(&output)
        .arg("--interval")
        .arg("100ms")
        .arg("--duration")
        .arg("1s")
        .output()
        .expect("failed to run rezolus record");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    let (_, metadata, rows) = the_recording(&output, "prometheus/scrape");
    assert_eq!(
        metadata.get("descriptions").map(String::as_str),
        Some("mine"),
        "{metadata:?}"
    );
    assert!(rows > 0, "the scrapes were recorded");
}

/// A server that answers 503 to every request, counting the requests for
/// `/metrics/stream`.
fn spawn_unavailable_counted() -> (u16, std::sync::Arc<std::sync::atomic::AtomicU64>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("failed to bind the 503 server");
    let port = listener.local_addr().unwrap().port();
    let streams = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let counted = streams.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = [0u8; 8192];
            let Ok(n) = stream.read(&mut buf) else {
                continue;
            };
            let req = String::from_utf8_lossy(&buf[..n]);
            if req
                .split_whitespace()
                .nth(1)
                .is_some_and(|p| p.starts_with("/metrics/stream"))
            {
                counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            let _ = stream.write_all(
                b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\
                  Connection: close\r\n\r\n",
            );
        }
    });
    (port, streams)
}

/// A pending endpoint is detected again on every tick, not every other one:
/// the tick that takes a pending answer starts the next detection.
///
/// Regression: a tick took the answer and the next tick started the
/// detection, so a 503 endpoint was asked half as often (8 vs 14 stream
/// requests in 12 s at a 1 s interval).
#[test]
fn a_pending_endpoint_is_detected_every_tick() {
    let exporter = spawn_fake_exporter();
    let (pending, streams) = spawn_unavailable_counted();
    let dir = tempfile::tempdir().expect("failed to create a temp dir");
    let output = dir.path().join("every-tick.dendro");

    let out = Command::new(env!("CARGO_BIN_EXE_rezolus"))
        .arg("record")
        .arg("--endpoint")
        .arg(format!("http://127.0.0.1:{exporter}/metrics,source=svc"))
        .arg("--endpoint")
        .arg(format!("http://127.0.0.1:{pending},source=down"))
        .arg("-o")
        .arg(&output)
        .arg("--interval")
        .arg("200ms")
        .arg("--duration")
        .arg("3s")
        .output()
        .expect("failed to run rezolus record");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    // Startup's detection and one per tick: 3 s at 200 ms is 15 ticks.
    // Every other tick would be at most 9.
    let asked = streams.load(std::sync::atomic::Ordering::SeqCst);
    assert!(asked >= 12, "the stream route was asked {asked} times");
}
