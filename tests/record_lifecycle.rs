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

/// Minimal stand-in for a 6.0 agent: answers `/metrics/binary` with a
/// snapshot, serves a replication stream on `/metrics/stream` (see
/// [`serve_stream`]), and 404s the optional metadata routes (`/systeminfo`,
/// `/metrics/descriptions`, `/samplers`, `/status`), which the recorder treats
/// as absent. A `.rez`, parquet or raw run scrapes it; a `.dendro` run
/// streams it. Returns the bound port; the accept loop is detached and dies
/// with the test process.
fn spawn_fake_agent() -> u16 {
    spawn_agent(true, None)
}

/// A stand-in for an agent older than `/metrics/stream` (5.21.0): it scrapes
/// like any other, answers `/` with the `Rezolus <version> Agent` banner, and
/// 404s the stream route and `/status`.
fn spawn_fake_agent_without_stream(version: &'static str) -> u16 {
    spawn_agent(false, Some(version))
}

fn spawn_agent(streams: bool, banner: Option<&'static str>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("failed to bind the fake agent");
    let port = listener.local_addr().unwrap().port();
    let tick = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let tick = tick.clone();
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
                if path.starts_with("/metrics/binary") {
                    let tick = tick.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    let body = snapshot_bytes(tick);
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/msgpack\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(head.as_bytes());
                    let _ = stream.write_all(&body);
                } else if streams && path.starts_with("/metrics/stream") {
                    serve_stream(stream);
                    return;
                } else if let (Some(version), "/") = (banner, path.as_str()) {
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
/// preamble and a handshake, then one rows frame every 100ms carrying one
/// counter in the `fake/ops` acquisition group, until the recorder hangs up.
fn serve_stream(mut stream: std::net::TcpStream) {
    use dendro::replicate::{wire, Frame};
    use metriken_exposition::{GroupSchema, GroupSnapshot, MetricDesc};

    let head = "HTTP/1.1 200 OK\r\n\
                Content-Type: application/vnd.rezolus.replication.v1+dendro\r\n\
                Connection: close\r\n\r\n";
    let mut bytes = head.as_bytes().to_vec();
    wire::write_preamble(&mut bytes).unwrap();
    let anchor = wall_ns();
    wire::encode_frame(
        &Frame::Handshake {
            source: 0,
            uuid: Some("fake-epoch".to_string()),
            labels: Default::default(),
            metadata: Default::default(),
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
        std::thread::sleep(Duration::from_millis(100));
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
        }
    });
    port
}

/// A Prometheus endpoint records into a `.rez` — it used to demote the whole
/// run to parquet.
///
/// The refusal was a policy check, not a capability limit: a scrape is one
/// request and one response, which is exactly one acquisition group, and the
/// archive has always been able to hold those. This drives the real binary
/// end to end because the conversion, the archive write and the read back are
/// three separate layers and the interesting failures are between them.
#[test]
fn a_prometheus_endpoint_records_into_a_rez() {
    let port = spawn_fake_exporter();
    let dir = tempfile::tempdir().expect("failed to create a temp dir");
    let output = dir.path().join("prom.rez");

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
        "recording a prometheus endpoint to .rez must exit 0 (status {:?})\nstderr:\n{stderr}",
        out.status.code()
    );
    assert!(
        !stderr.contains("falling back") && !stderr.contains("requires a rezolus"),
        "the run must NOT demote to parquet:\n{stderr}"
    );

    // A v3 archive, not the parquet fallback.
    let head = std::fs::read(&output).expect("the recording should exist");
    assert!(
        head.starts_with(b"SQLite format 3\0"),
        "a prometheus recording must be a real .rez, not a renamed parquet"
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
        stdout.contains("source=svc") || stdout.contains("svc"),
        "the recording keeps its source label: {stdout}"
    );

    // And the metrics are queryable out of the archive by name.
    let described = Command::new(env!("CARGO_BIN_EXE_rezolus"))
        .arg("mcp")
        .arg("describe-metrics")
        .arg(&output)
        .output()
        .expect("failed to run rezolus mcp describe-metrics");
    let stdout = String::from_utf8_lossy(&described.stdout);
    assert!(
        stdout.contains("http_requests_total"),
        "the exporter's metrics must be readable back out: {stdout}"
    );
}

/// `-o out.dendro` writes a dendro archive through metriken-archive's writer,
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
        metriken_archive::DendroCatalog::is_archive(&output),
        Ok(true),
        "a .dendro output is a dendro archive"
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
    let output = dir.path().join("window.rez");

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

    let db = rez::rez_sqlite::RezDb::open(&output).expect("the archive opens");
    let segments = db
        .read_segments(1, "prometheus/scrape")
        .expect("the scrape's segments read back");
    assert!(!segments.is_empty(), "the run sealed at least one segment");

    let mut checked = 0usize;
    for seg in &segments {
        let bytes = db
            .read_segment_bytes(1, "prometheus/scrape", seg.seq)
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
    let output = dir.path().join("fleet.rez");

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

/// A rezolus agent and a Prometheus exporter in the SAME archive — newly
/// possible, and the combination nothing else covers.
///
/// The two produce completely different table shapes (per-sampler V2 cells vs
/// a V3 acquisition group), so this is where a container that quietly assumed
/// one wire per archive would break.
#[test]
fn a_rezolus_agent_and_a_prometheus_exporter_share_one_archive() {
    let agent = spawn_fake_agent();
    let exporter = spawn_fake_exporter_named("http_requests_total");
    let dir = tempfile::tempdir().expect("failed to create a temp dir");
    let output = dir.path().join("mixed.rez");

    let out = Command::new(env!("CARGO_BIN_EXE_rezolus"))
        .arg("record")
        .arg("--endpoint")
        .arg(format!("http://127.0.0.1:{agent},source=rezolus"))
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
        "a mixed run must record (status {:?})\nstderr:\n{stderr}",
        out.status.code()
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
    assert!(
        stdout.contains("prometheus/scrape"),
        "the exporter's acquisition group must be there: {stdout}"
    );
    assert!(
        stdout.contains("fake"),
        "and the agent's own sampler table alongside it: {stdout}"
    );
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
    use metriken_archive::Catalog;

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

    let catalog = metriken_archive::DendroCatalog::open(&output).expect("the archive opens");
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

/// An agent that cannot serve `/metrics/stream` is refused for a `.dendro`,
/// by name and version, before the archive is created — so nothing is left
/// at the output path — and is never scraped instead.
#[test]
fn a_dendro_refuses_an_agent_without_a_stream_and_leaves_nothing_behind() {
    let agent = spawn_fake_agent_without_stream("5.20.0");
    let dir = tempfile::tempdir().expect("failed to create a temp dir");
    let output = dir.path().join("old.dendro");

    let out = Command::new(env!("CARGO_BIN_EXE_rezolus"))
        .arg("record")
        .arg("--url")
        .arg(format!("http://127.0.0.1:{agent}"))
        .arg("-o")
        .arg(&output)
        .arg("--interval")
        .arg("100ms")
        .arg("--duration")
        .arg("1s")
        .output()
        .expect("failed to run rezolus record");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "refused:\n{stderr}");
    for needle in [
        "Rezolus 5.20.0",
        "/metrics/stream",
        "5.21.0",
        "-o out.rez",
        &format!("127.0.0.1:{agent}"),
    ] {
        assert!(stderr.contains(needle), "{needle:?} in:\n{stderr}");
    }
    let left: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
    assert!(left.is_empty(), "nothing is written: {left:?}");

    // The same agent still records into a .rez, which scrapes it.
    let rez = dir.path().join("old.rez");
    let out = Command::new(env!("CARGO_BIN_EXE_rezolus"))
        .arg("record")
        .arg("--url")
        .arg(format!("http://127.0.0.1:{agent}"))
        .arg("-o")
        .arg(&rez)
        .arg("--interval")
        .arg("100ms")
        .arg("--duration")
        .arg("500ms")
        .output()
        .expect("failed to run rezolus record");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
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

/// The `events` payload of the one recording in a `.rez`, parsed.
fn rez_run_events(output: &std::path::Path) -> Vec<dashboard::Event> {
    let db = rez::rez_sqlite::RezDb::open(output).expect("the archive opens");
    let recordings = db.read_recordings().expect("the catalog reads");
    assert_eq!(recordings.len(), 1, "one endpoint, one recording");
    let raw = recordings[0]
        .meta
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

/// A wrapped `.rez` run marks the run: `run_start` at spawn and `run_end` at
/// exit, paired by one uuid, with the program name and no argument list.
#[test]
fn a_wrapped_rez_run_writes_run_start_and_run_end_events() {
    let dir = tempfile::tempdir().expect("failed to create a temp dir");
    let output = dir.path().join("out.rez");
    let out = record_wrapped(&output, &[]);
    assert!(
        out.status.success(),
        "the wrapped run exits with the command's status (0)\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let (start, end) = assert_run_events(&rez_run_events(&output));
    assert!(
        start.details.is_none(),
        "the argument list is not recorded by default: {:?}",
        start.details
    );
    assert!(end.details.is_some(), "run_end says how the command ended");
}

/// A wrapped `.dendro` run writes the same two events, into the source's
/// metadata through the dendro writer (`SourceRecorder::update_metadata`).
#[test]
fn a_wrapped_dendro_run_writes_run_start_and_run_end_events() {
    use metriken_archive::Catalog;

    let dir = tempfile::tempdir().expect("failed to create a temp dir");
    let output = dir.path().join("out.dendro");
    let out = record_wrapped(&output, &[]);
    assert!(
        out.status.success(),
        "the wrapped run exits with the command's status (0)\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let catalog = metriken_archive::DendroCatalog::open(&output).expect("the archive opens");
    let sources = catalog.sources().expect("the catalog reads");
    assert_eq!(sources.len(), 1, "one endpoint, one source");
    let raw = sources[0]
        .metadata
        .get("events")
        .expect("a wrapped run writes the events key");
    let payload: dashboard::Events = serde_json::from_str(raw).expect("the payload parses");
    let (start, end) = assert_run_events(&payload.events);
    assert!(start.details.is_none());
    assert!(end.details.is_some(), "run_end says how the command ended");
}

/// `--record-command-line` puts the full argument list, space-joined, in the
/// `run_start` event's details.
#[test]
fn record_command_line_stores_the_argument_list_in_run_start() {
    let dir = tempfile::tempdir().expect("failed to create a temp dir");
    let output = dir.path().join("out.rez");
    let out = record_wrapped(&output, &["--record-command-line"]);
    assert!(
        out.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let (start, _end) = assert_run_events(&rez_run_events(&output));
    assert_eq!(start.details.as_deref(), Some("sh -c sleep 0.5"));
}

/// Parquet output carries the same two events in its footer, under the key
/// `annotate` and `combine` use.
#[test]
fn a_wrapped_parquet_run_writes_the_events_to_the_footer() {
    let dir = tempfile::tempdir().expect("failed to create a temp dir");
    let output = dir.path().join("out.parquet");
    let out = record_wrapped(&output, &[]);
    assert!(
        out.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let described = Command::new(env!("CARGO_BIN_EXE_rezolus"))
        .arg("recording")
        .arg("metadata")
        .arg("-i")
        .arg(&output)
        .arg("--json")
        .output()
        .expect("failed to run rezolus recording metadata");
    assert!(
        described.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&described.stderr)
    );
    let json: serde_json::Value =
        serde_json::from_slice(&described.stdout).expect("--json output parses");
    let payload: dashboard::Events =
        serde_json::from_value(json["file_metadata"]["events"].clone())
            .expect("the footer carries an events payload");
    assert_run_events(&payload.events);
}

/// Raw output has no metadata channel, so a wrapped raw run writes the
/// snapshots and nothing else; it still succeeds.
#[test]
fn a_wrapped_raw_run_writes_the_file_and_exits_zero() {
    let dir = tempfile::tempdir().expect("failed to create a temp dir");
    let output = dir.path().join("out.raw");
    let out = record_wrapped(&output, &[]);
    assert!(
        out.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let size = std::fs::metadata(&output)
        .expect("the raw recording exists")
        .len();
    assert!(size > 0, "the raw recording holds the scraped snapshots");
}

/// Run `rezolus record` wrapping `sh -c <script>` against a fake agent at
/// `interval`, writing to `output`.
fn record_wrapped_script(
    output: &std::path::Path,
    interval: &str,
    script: &str,
) -> std::process::Output {
    let port = spawn_fake_agent();
    Command::new(env!("CARGO_BIN_EXE_rezolus"))
        .arg("record")
        .arg("--url")
        .arg(format!("http://127.0.0.1:{port}"))
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

/// A command that exits at once is still recorded: the exit is followed by
/// one scrape, so the archive has a row and both events, and the run exits 0.
///
/// Regression: the exit arm went straight back to the loop top, which broke
/// out before the scrape, so `-- true` produced "command exited before any
/// metrics were recorded", no file, and exit 1.
#[test]
fn a_command_that_exits_at_once_still_gets_one_scrape() {
    let dir = tempfile::tempdir().expect("failed to create a temp dir");
    let output = dir.path().join("out.rez");
    let out = record_wrapped_script(&output, "200ms", "exit 0");
    assert!(
        out.status.success(),
        "an instant exit is a recorded run, exit 0\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let db = rez::rez_sqlite::RezDb::open(&output).expect("the archive opens");
    let recordings = db.read_recordings().expect("the catalog reads");
    assert_eq!(recordings.len(), 1);
    let rows = db
        .total_rows(recordings[0].id, "fake")
        .expect("the sampler's rows count");
    assert!(rows >= 1, "the final scrape wrote a row, got {rows}");

    let raw = recordings[0]
        .meta
        .metadata
        .get("events")
        .expect("both run events are written");
    let payload: dashboard::Events = serde_json::from_str(raw).expect("the payload parses");
    let kinds: Vec<&str> = payload
        .events
        .iter()
        .filter_map(|e| e.kind.as_deref())
        .collect();
    assert_eq!(kinds, vec!["run_start", "run_end"]);
}

/// The interval the command exited in is sampled: the last row is no older
/// than one interval before `run_end`.
///
/// Regression: the exit wake ended the recording without the scrape that
/// follows the tick, losing the tail of the run (`-- sleep 2.4` at 1s wrote
/// 2 rows where 3 were due).
#[test]
fn the_interval_the_command_exited_in_is_sampled() {
    let dir = tempfile::tempdir().expect("failed to create a temp dir");
    let output = dir.path().join("out.rez");
    let out = record_wrapped_script(&output, "200ms", "sleep 0.5");
    assert!(
        out.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let db = rez::rez_sqlite::RezDb::open(&output).expect("the archive opens");
    let recordings = db.read_recordings().expect("the catalog reads");
    assert_eq!(recordings.len(), 1);
    let rid = recordings[0].id;
    // A finalized recording is segments only; the last segment's `last_ts`
    // is the last row.
    let segments = db.read_segment_meta(rid, "fake").expect("segment meta");
    let last_ts = segments
        .iter()
        .map(|(_, meta)| meta.last_ts)
        .max()
        .expect("at least one segment");

    let raw = recordings[0].meta.metadata.get("events").expect("events");
    let payload: dashboard::Events = serde_json::from_str(raw).expect("the payload parses");
    let run_end = payload
        .events
        .iter()
        .find(|e| e.kind.as_deref() == Some("run_end"))
        .expect("run_end");
    let interval_ns = Duration::from_millis(200).as_nanos() as u64;
    assert!(
        last_ts + interval_ns >= run_end.timestamp,
        "the last row ({last_ts}) must be within one interval of run_end ({})",
        run_end.timestamp
    );
}
