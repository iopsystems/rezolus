//! `rezolus view` on an archive that one `rezolus record` run wrote from a
//! Rezolus agent and a service: one combined view, the agent's built-in
//! sections next to the service's dashboard from the template in the
//! service's recording.
//!
//! Driven through the real binary: the recorder writes the archive from a
//! stand-in agent (a replication stream) and a stand-in Prometheus exporter,
//! and the viewer serves it over HTTP.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, SystemTime};

fn wall_ns() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
}

/// Answer one HTTP request on `stream` with `body`.
fn respond(stream: &mut TcpStream, content_type: &str, body: &str) {
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body.as_bytes());
    let _ = stream.flush();
}

/// A stand-in Rezolus agent on host `h1`: `/` names a 6.0.0 agent,
/// `/systeminfo` its hostname, and `/metrics/stream` serves one counter,
/// `cpu_usage{id="0",state="user"}`, rising 0.5 s per second.
fn spawn_agent() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            std::thread::spawn(move || {
                let mut buf = [0u8; 8192];
                let Ok(n) = stream.read(&mut buf) else { return };
                let req = String::from_utf8_lossy(&buf[..n]);
                let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
                match path.as_str() {
                    "/" => respond(&mut stream, "text/plain", "Rezolus 6.0.0 Agent\n"),
                    "/systeminfo" => {
                        respond(&mut stream, "application/json", r#"{"hostname":"h1"}"#)
                    }
                    p if p.starts_with("/metrics/stream") => serve_stream(stream, p),
                    _ => {
                        let _ = stream.write_all(
                            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\
                              Connection: close\r\n\r\n",
                        );
                    }
                }
            });
        }
    });
    port
}

/// The agent's replication stream: a handshake, then a rows frame per
/// requested interval until the recorder hangs up.
fn serve_stream(mut stream: TcpStream, path: &str) {
    use dendro::replicate::{wire, Frame};
    use metriken_exposition::{GroupSchema, GroupSnapshot, MetricDesc};

    let interval = path
        .split_once("interval=")
        .map(|(_, v)| v.split('&').next().unwrap_or(v).replace("%20", " "))
        .and_then(|v| humantime::parse_duration(&v).ok())
        .unwrap_or(Duration::from_millis(100));
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/vnd.metriken.replication.v1+dendro\r\n\
         x-metriken-update-floor: 100ms\r\nx-metriken-frame-interval: {}\r\n\
         x-metriken-layout: wide\r\nConnection: close\r\n\r\n",
        humantime::format_duration(interval)
    );
    let mut bytes = head.into_bytes();
    wire::write_preamble(&mut bytes).unwrap();
    wire::encode_frame(
        &Frame::Handshake {
            source: 0,
            uuid: Some("8c1f4f7e-0d0e-4b8a-9f43-1a2b3c4d5e6f".to_string()),
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
        counters: vec![MetricDesc {
            name: "0x0".to_string(),
            metadata: [("metric", "cpu_usage"), ("id", "0"), ("state", "user")]
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }],
        gauges: Vec::new(),
        histograms: Vec::new(),
    };
    let start = wall_ns();
    for seq in 0u64.. {
        std::thread::sleep(interval);
        let ts = wall_ns();
        let group = GroupSnapshot {
            name: "cpu_usage/cpu".to_string(),
            schema_hash: schema.hash(),
            schema: Some(std::sync::Arc::new(schema.clone())),
            window: Some(metriken::Window::new(ts - 1_000_000, ts)),
            counters: vec![Some((ts - start) / 2)],
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
                    stream: "cpu_usage/cpu".to_string(),
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

/// A stand-in Prometheus exporter whose `svc_requests_total` rises by 10 per
/// scrape.
fn spawn_exporter() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let mut tick = 0u64;
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = [0u8; 8192];
            if stream.read(&mut buf).unwrap_or(0) == 0 {
                continue;
            }
            tick += 1;
            let body = format!(
                "# TYPE svc_requests_total counter\nsvc_requests_total {}\n",
                tick * 10
            );
            respond(&mut stream, "text/plain; version=0.0.4", &body);
        }
    });
    port
}

/// A `rezolus view` process, killed when dropped.
struct Viewer {
    child: Child,
    addr: String,
}

impl Drop for Viewer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_viewer(archive: &std::path::Path) -> Viewer {
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let addr = format!("127.0.0.1:{port}");
    let mut child = Command::new(env!("CARGO_BIN_EXE_rezolus"))
        .arg("view")
        .arg(archive)
        .arg("--listen")
        .arg(&addr)
        .env("REZOLUS_NO_OPEN", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Ready once it says where to browse.
    let stderr = child.stderr.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            let _ = tx.send(line);
        }
    });
    let viewer = Viewer { child, addr };
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        let line = rx
            .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
            .expect("the viewer starts");
        if line.contains("Use your browser to view") {
            return viewer;
        }
    }
}

/// `GET path` from the viewer, as JSON.
fn get(viewer: &Viewer, path: &str) -> serde_json::Value {
    let mut stream = TcpStream::connect(&viewer.addr).unwrap();
    write!(
        stream,
        "GET {path} HTTP/1.0\r\nHost: {}\r\n\r\n",
        viewer.addr
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let (head, body) = response.split_once("\r\n\r\n").unwrap();
    assert!(head.contains(" 200 "), "{path}: {head}");
    serde_json::from_str(body).unwrap_or_else(|e| panic!("{path}: {e}: {body}"))
}

/// The plot queries of a section, by plot id.
fn plot_queries(section: &serde_json::Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for group in section["groups"].as_array().unwrap() {
        for sg in group["subgroups"].as_array().unwrap() {
            for plot in sg["plots"].as_array().unwrap() {
                out.push((
                    plot["opts"]["id"].as_str().unwrap().to_string(),
                    plot["promql_query"].as_str().unwrap().to_string(),
                ));
            }
        }
    }
    out
}

#[test]
fn an_agent_and_a_service_of_one_run_open_as_one_view() {
    let agent = spawn_agent();
    let exporter = spawn_exporter();
    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("run.dendro");

    let out = Command::new(env!("CARGO_BIN_EXE_rezolus"))
        .arg("record")
        .arg("--endpoint")
        .arg(format!("http://127.0.0.1:{agent}"))
        .arg("--endpoint")
        .arg(format!("http://127.0.0.1:{exporter}/metrics,source=svc"))
        .arg("-o")
        .arg(&archive)
        .arg("--interval")
        .arg("100ms")
        .arg("--duration")
        .arg("2s")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "record: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The service's template in its recording's metadata, as a producer's
    // handshake (or `--endpoint ..,template=`) would put it there.
    let template = serde_json::json!({
        "service_name": "svc",
        "kpis": [{
            "role": "throughput",
            "title": "Requests",
            "query": "sum(irate(svc_requests_total{source=\"svc\"}[1s]))",
            "type": "delta_counter",
            "denominator": true,
        }],
    })
    .to_string();
    {
        let mut db = dendro::archive::ArchiveMut::open(&archive).unwrap();
        let sources = db.read_sources().unwrap();
        assert_eq!(sources.len(), 2, "{sources:?}");
        let svc = sources
            .iter()
            .find(|s| s.meta.labels.get("source").map(String::as_str) == Some("svc"))
            .unwrap();
        let agent = sources.iter().find(|s| s.id != svc.id).unwrap();
        assert_eq!(agent.meta.labels.get("source").unwrap(), "rezolus");
        assert_eq!(agent.meta.labels.get("host").unwrap(), "h1");
        let mut metadata = svc.meta.metadata.clone();
        metadata.insert("service_queries".to_string(), template);
        db.update_source_metadata(svc.id, &metadata).unwrap();
    }

    let viewer = spawn_viewer(&archive);
    let sections = get(&viewer, "/api/v1/sections")["data"].clone();
    let routes: Vec<&str> = sections["sections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["route"].as_str().unwrap())
        .collect();
    assert_eq!(routes[..2], ["/overview", "/service/svc"], "{routes:?}");
    assert!(routes.contains(&"/cpu"), "{routes:?}");
    assert!(!routes.iter().any(|r| r.contains('@')), "{routes:?}");

    // The WASM viewer, given the archive's bytes, serves the same
    // navigation and the same service section.
    let mut wasm = viewer::WasmCaptureRegistry::new();
    wasm.attach("baseline", &std::fs::read(&archive).unwrap(), "run.dendro")
        .unwrap();
    wasm.init_templates("baseline", "[]").unwrap();
    let wasm_sections: serde_json::Value =
        serde_json::from_str(&wasm.get_sections("baseline").unwrap()).unwrap();
    assert_eq!(wasm_sections, sections["sections"]);
    let mut wasm_svc: serde_json::Value =
        serde_json::from_str(&wasm.get_section("baseline", "service/svc").unwrap()).unwrap();
    let mut server_svc = get(&viewer, "/data/service/svc.json");
    for v in [&mut wasm_svc, &mut server_svc] {
        let o = v.as_object_mut().unwrap();
        o.remove("filename");
        o.remove("filesize");
    }
    assert_eq!(wasm_svc, server_svc);

    let mode = get(&viewer, "/api/v1/captures");
    assert_eq!(mode.to_string().matches("\"id\"").count(), 1, "{mode}");

    let svc = get(&viewer, "/data/service/svc.json");
    let queries = plot_queries(&svc);
    // The template's own `source="svc"`, and `host!="h1"`: the service's
    // recording has no `host`, the agent's has.
    assert_eq!(
        queries[0].1,
        "sum(irate(svc_requests_total{source=\"svc\",host!=\"h1\"}[1s]))"
    );
    let cpu = get(&viewer, "/data/cpu.json");
    let cpu_queries = plot_queries(&cpu);
    assert!(!cpu_queries.is_empty());
    for (id, q) in &cpu_queries {
        assert!(q.contains("source=\"rezolus\""), "{id}: {q}");
    }

    // The service's KPI reads the service's series through its recording's
    // `source` label.
    let range = |query: &str| {
        let start = sections["start_time"].as_u64().unwrap() as f64 / 1000.0;
        let end = sections["end_time"].as_u64().unwrap() as f64 / 1000.0;
        let q: String = query
            .bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-' | b'.' => {
                    (b as char).to_string()
                }
                _ => format!("%{b:02X}"),
            })
            .collect();
        get(
            &viewer,
            &format!("/api/v1/query_range?query={q}&start={start}&end={end}&step=0.1"),
        )
    };
    let svc_result = range(&queries[0].1);
    let series = svc_result["data"]["result"].as_array().unwrap();
    assert_eq!(series.len(), 1, "{svc_result}");
    // The agent's series under the matchers its sections carry, and none
    // under the service's.
    let busy = cpu_queries
        .iter()
        .find(|(id, _)| id == "busy-pct")
        .map(|(_, q)| q.clone())
        .unwrap_or_else(|| panic!("{cpu_queries:?}"));
    assert!(
        busy.contains(r#"cpu_usage{host="h1",source="rezolus"}"#),
        "{busy}"
    );
    for (query, n) in [
        (r#"irate(cpu_usage{host="h1",source="rezolus"}[1s])"#, 1),
        (r#"irate(cpu_usage{source="svc"}[1s])"#, 0),
    ] {
        let result = range(query);
        assert_eq!(
            result["data"]["result"].as_array().unwrap().len(),
            n,
            "{query}: {result}"
        );
    }
}

/// A single-source parquet is a recording of one member: its series carry
/// its `source`, so a KPI template carried in it whose queries select
/// `source=` (as an archive's do) renders, and `mcp query` with the matcher
/// reads the series.
#[test]
fn a_parquets_template_selecting_its_source_renders() {
    let dir = tempfile::tempdir().unwrap();
    let parquet = dir.path().join("run.parquet");
    std::fs::copy(
        concat!(env!("CARGO_MANIFEST_DIR"), "/site/viewer/data/demo.parquet"),
        &parquet,
    )
    .unwrap();
    let kpis = dir.path().join("kpis.json");
    std::fs::write(
        &kpis,
        serde_json::json!({
            "service_name": "rezolus",
            "kpis": [{
                "role": "throughput",
                "title": "Cores",
                "query": "sum(cpu_cores{source=\"rezolus\"})",
                "type": "gauge",
            }],
        })
        .to_string(),
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_rezolus"))
        .args(["recording", "annotate"])
        .arg(&parquet)
        .arg("--queries")
        .arg(&kpis)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let query = Command::new(env!("CARGO_BIN_EXE_rezolus"))
        .args(["mcp", "query"])
        .arg(&parquet)
        .arg("sum(cpu_cores{source=\"rezolus\"})")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&query.stdout);
    assert!(
        query.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&query.stderr)
    );
    assert!(!stdout.contains("no series"), "{stdout}");

    let viewer = spawn_viewer(&parquet);
    let svc = get(&viewer, "/data/service/rezolus.json");
    assert!(svc["metadata"].get("unavailable_kpis").is_none(), "{svc}");
    assert_eq!(
        plot_queries(&svc)[0].1,
        "sum(cpu_cores{source=\"rezolus\"})"
    );
}
