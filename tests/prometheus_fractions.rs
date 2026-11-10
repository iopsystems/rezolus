//! A Prometheus endpoint's fractional values, recorded by `rezolus record`
//! and read back through every reader a user has: `mcp describe-metrics`,
//! `mcp query`, `mcp detect-anomalies`, the server viewer's
//! `/api/v1/metrics` and `/api/v1/query_range`, and the WASM viewer's
//! `info`, `metrics` and `query_range` (built natively).
//!
//! metriken-recorder 0.9 stores a Prometheus scalar as an `f64` column
//! stamped `counter` or `gauge` (metriken-storage 0.8's float counters and
//! float gauges). Before it, a gauge of 0.25 was stored as 0 and a counter
//! moved in whole units. Each reader below must list the float columns
//! under their kind and return the fraction.
//!
//! A counter's rate depends on the scrape timing, so the fractional counter
//! is read as the ratio of its `irate()` to that of an integer counter of
//! the same scrape: both share each row's timestamp and window, so the ratio
//! is its increase per scrape over 1, 0.5 exactly. Stored as integers, its
//! values 0.75, 1.25, 1.75, ... would read 0, 1, 1, 2, ... and the ratio
//! would alternate between 0 and 1.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// The gauge's value on every scrape.
const GAUGE: f64 = 0.25;

/// The fractional counter's value on the `tick`th request: 0.5 per request
/// plus 0.25.
fn counter_at(tick: u64) -> f64 {
    tick as f64 * 0.5 + 0.25
}

/// `busy_seconds_total`'s increase per scrape over `requests_total`'s.
const RATIO: f64 = 0.5;

/// The query for [`RATIO`].
const RATIO_QUERY: &str = "sum(irate(busy_seconds_total[1s])) / sum(irate(requests_total[1s]))";

/// A stand-in Prometheus exporter serving the counter `busy_seconds_total`
/// ([`counter_at`]), the counter `requests_total` (the request count) and
/// the gauge `load_fraction` ([`GAUGE`]).
fn spawn_exporter() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind the fake exporter");
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
                "# HELP busy_seconds_total Seconds spent busy.\n\
                 # TYPE busy_seconds_total counter\n\
                 busy_seconds_total {}\n\
                 # TYPE requests_total counter\n\
                 requests_total {tick}\n\
                 # TYPE load_fraction gauge\n\
                 load_fraction {GAUGE}\n",
                counter_at(tick)
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

/// The recording both tests read: the exporter recorded for 11 seconds at
/// 200 ms (`mcp detect-anomalies` refuses less than 10 seconds of data),
/// made once per test run under `CARGO_TARGET_TMPDIR`.
fn recording() -> &'static std::path::Path {
    static ARCHIVE: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    ARCHIVE.get_or_init(|| {
        let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("prometheus_fractions");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let output = dir.join("fractions.dendro");
        let port = spawn_exporter();
        let out = Command::new(env!("CARGO_BIN_EXE_rezolus"))
            .arg("record")
            .arg("--endpoint")
            .arg(format!("http://127.0.0.1:{port}/metrics,alias=svc"))
            .arg("-o")
            .arg(&output)
            .arg("--interval")
            .arg("200ms")
            .arg("--duration")
            .arg("11s")
            .output()
            .expect("run rezolus record");
        assert!(
            out.status.success(),
            "rezolus record exits 0\nstderr:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        output
    })
}

/// `rezolus mcp <args>`'s stdout, asserting it exits 0.
fn mcp(args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_rezolus"))
        .arg("mcp")
        .args(args)
        .output()
        .expect("run rezolus mcp");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "rezolus mcp {args:?} exits 0\nstdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
}

/// The values `mcp query` prints for a range result's `First`, `Last`,
/// `Min`, `Max` and `Mean` lines.
fn printed_values(stdout: &str) -> Vec<(String, f64)> {
    let mut out = Vec::new();
    for line in stdout.lines() {
        let line = line.trim();
        for key in ["First:", "Last:", "Min:", "Max:", "Mean:"] {
            if let Some(rest) = line.strip_prefix(key) {
                // `First: <ts> = <value>[  [lo, hi]]` or `Min:   <value>`.
                let value = rest.rsplit_once('=').map_or(rest, |(_, v)| v);
                let value = value.split_whitespace().next().unwrap();
                out.push((key.to_string(), value.parse().unwrap()));
            }
        }
    }
    out
}

/// The metric names (`• name` lines) of `mcp describe-metrics` output under
/// the heading starting `heading`, up to the next heading (a line starting
/// with a capital letter).
fn section(stdout: &str, heading: &str) -> Vec<String> {
    let start = stdout
        .find(heading)
        .unwrap_or_else(|| panic!("no {heading} section:\n{stdout}"));
    stdout[start..]
        .lines()
        .skip(1)
        .take_while(|l| !l.starts_with(|c: char| c.is_ascii_uppercase()))
        .filter_map(|l| l.strip_prefix("• "))
        .map(str::to_string)
        .collect()
}

#[test]
fn a_prometheus_endpoints_fractions_read_back_through_mcp() {
    let output = recording();
    let file = output.to_str().unwrap();

    // The float columns are listed under their kind.
    let described = mcp(&["describe-metrics", file]);
    let counters = section(&described, "COUNTERS");
    let gauges = section(&described, "GAUGES");
    assert_eq!(
        counters,
        ["busy_seconds_total", "requests_total"],
        "{described}"
    );
    assert_eq!(gauges, ["load_fraction"], "{described}");

    // A bare gauge reads as stored.
    let gauge = mcp(&["query", file, "load_fraction"]);
    let values = printed_values(&gauge);
    assert_eq!(values.len(), 5, "{gauge}");
    for (key, v) in &values {
        assert_eq!(*v, GAUGE, "{key} of load_fraction:\n{gauge}");
    }

    // A counter is read through a rate; the fractional counter's rate is
    // half the integer counter's at every point.
    let ratio = mcp(&["query", file, RATIO_QUERY]);
    let values = printed_values(&ratio);
    assert_eq!(values.len(), 5, "{ratio}");
    for (key, v) in &values {
        assert!(
            (v - RATIO).abs() < 1e-9,
            "{key} of the ratio is {v}:\n{ratio}"
        );
    }

    // A bare metric name given to detect-anomalies is wrapped by its kind:
    // the float counter in irate(), the float gauge as is.
    let anomalies = mcp(&["detect-anomalies", file, "busy_seconds_total"]);
    assert!(
        anomalies.contains("Query: sum(irate(busy_seconds_total[1m]))"),
        "{anomalies}"
    );
    let anomalies = mcp(&["detect-anomalies", file, "load_fraction"]);
    assert!(
        anomalies.contains("Query: sum(load_fraction)"),
        "{anomalies}"
    );
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

/// The `metric_type` of each metric in a `/api/v1/metrics` body.
fn catalog_types(body: &serde_json::Value) -> std::collections::BTreeMap<String, String> {
    body["metrics"]
        .as_array()
        .unwrap_or_else(|| panic!("no metrics: {body}"))
        .iter()
        .map(|m| {
            (
                m["name"].as_str().unwrap().to_string(),
                m["metric_type"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

/// Every value of every series in a `/api/v1/query_range` body.
fn range_values(body: &serde_json::Value) -> Vec<f64> {
    assert_eq!(body["status"], "success", "{body}");
    let series = body["data"]["result"].as_array().unwrap();
    assert!(!series.is_empty(), "{body}");
    series
        .iter()
        .flat_map(|s| s["values"].as_array().unwrap().iter())
        .map(|p| match &p[1] {
            serde_json::Value::String(s) => s.parse().unwrap(),
            v => v.as_f64().unwrap(),
        })
        .collect()
}

#[test]
fn a_prometheus_endpoints_fractions_read_back_through_both_viewers() {
    let output = recording();

    let want_types = std::collections::BTreeMap::from([
        ("busy_seconds_total".to_string(), "counter".to_string()),
        ("requests_total".to_string(), "counter".to_string()),
        ("load_fraction".to_string(), "gauge".to_string()),
    ]);
    let check_gauge = |values: Vec<f64>, who: &str| {
        assert!(!values.is_empty(), "{who}");
        assert!(values.iter().all(|v| *v == GAUGE), "{who}: {values:?}");
    };
    let check_ratio = |values: Vec<f64>, who: &str| {
        assert!(!values.is_empty(), "{who}");
        assert!(
            values.iter().all(|v| (v - RATIO).abs() < 1e-9),
            "{who}: {values:?}"
        );
    };
    let encoded: String = RATIO_QUERY
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-' | b'.' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect();

    // The server viewer.
    let viewer = spawn_viewer(output);
    let metrics = get(&viewer, "/api/v1/metrics");
    let types = catalog_types(&metrics);
    for (name, kind) in &want_types {
        assert_eq!(types.get(name), Some(kind), "{name}: {metrics}");
    }
    let sections = get(&viewer, "/api/v1/sections")["data"].clone();
    let start = sections["start_time"].as_u64().unwrap() as f64 / 1000.0;
    let end = sections["end_time"].as_u64().unwrap() as f64 / 1000.0;
    let range = |q: &str| {
        get(
            &viewer,
            &format!("/api/v1/query_range?query={q}&start={start}&end={end}&step=0.1"),
        )
    };
    check_gauge(
        range_values(&range("load_fraction")),
        "server load_fraction",
    );
    check_ratio(range_values(&range(&encoded)), "server ratio");
    drop(viewer);

    // The WASM viewer, built natively.
    let bytes = std::fs::read(output).unwrap();
    let wasm = match viewer::Viewer::new(&bytes, "fractions.dendro") {
        Ok(v) => v,
        Err(_) => panic!("the WASM viewer opens the recording"),
    };
    let info: serde_json::Value = serde_json::from_str(&wasm.info()).unwrap();
    assert!(
        info["counter_names"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("busy_seconds_total")),
        "{info}"
    );
    assert!(
        info["gauge_names"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("load_fraction")),
        "{info}"
    );
    let metrics: serde_json::Value = serde_json::from_str(&wasm.metrics(None)).unwrap();
    let types = catalog_types(&metrics);
    for (name, kind) in &want_types {
        assert_eq!(types.get(name), Some(kind), "WASM {name}: {metrics}");
    }
    let (start, end) = (
        info["minTime"].as_f64().unwrap(),
        info["maxTime"].as_f64().unwrap(),
    );
    let range = |q: &str| -> serde_json::Value {
        serde_json::from_str(&wasm.query_range(q, start, end, 0.1, "grid", None)).unwrap()
    };
    check_gauge(range_values(&range("load_fraction")), "WASM load_fraction");
    check_ratio(range_values(&range(RATIO_QUERY)), "WASM ratio");
}
