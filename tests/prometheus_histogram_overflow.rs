//! A Prometheus histogram whose `+Inf` bucket holds more than its highest
//! finite bucket, recorded by `rezolus record` and read back through
//! `mcp describe-metrics`, `mcp query`, the MCP server's `query` tool, the
//! server viewer's `/api/v1/query_range` and the WASM viewer's
//! `query_range` (built natively).
//!
//! metriken-recorder 0.11 records the observations above the highest finite
//! bucket as the float counter `<histogram>_overflow`, and metriken-query
//! 0.47 counts them in `histogram_quantile`: a quantile that falls among them
//! is `+Inf`, with the band `(highest finite bound, +Inf)`. The query JSON
//! writes it as the string "+Inf", where serde_json wrote `null`.
//!
//! Each scrape adds 9 observations at or below 0.1 s and 1 above 1 s, the
//! highest finite bucket. Over each row-to-row delta (F 9, O 1) the 0.99
//! quantile's rank is ceil(0.99 * 10) = 10 > 9, so it is +Inf, and the 0.5
//! quantile's rank 5 falls in the 0.1 s bucket.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const P99: &str = "histogram_quantile(0.99, request_duration_seconds)";
const P50: &str = "histogram_quantile(0.5, request_duration_seconds)";

/// The highest finite bucket, 1 s, in the stored unit (nanoseconds).
const TOP_BOUND: f64 = 1e9;

/// A stand-in Prometheus exporter serving the histogram
/// `request_duration_seconds`, 10 observations per request: 9 in the 0.1 s
/// bucket and 1 past the 1 s bucket.
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
            let finite = 9 * tick;
            let all = 10 * tick;
            let body = format!(
                "# HELP request_duration_seconds Request latency.\n\
                 # TYPE request_duration_seconds histogram\n\
                 request_duration_seconds_bucket{{le=\"0.1\"}} {finite}\n\
                 request_duration_seconds_bucket{{le=\"1\"}} {finite}\n\
                 request_duration_seconds_bucket{{le=\"+Inf\"}} {all}\n\
                 request_duration_seconds_sum {}\n\
                 request_duration_seconds_count {all}\n",
                tick as f64 * 2.0
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

/// The recording every test reads: the exporter recorded for 3 seconds at
/// 200 ms, made once per test run under `CARGO_TARGET_TMPDIR`.
fn recording() -> &'static std::path::Path {
    static ARCHIVE: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    ARCHIVE.get_or_init(|| {
        let dir =
            std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("prometheus_histogram_overflow");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let output = dir.join("overflow.dendro");
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
            .arg("3s")
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

/// The metric names (`• name` lines) of `mcp describe-metrics` output under
/// the heading starting `heading`, up to the next heading.
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

/// The values of a matrix result's JSON (`data.result` of the viewer's
/// response, or `result` of the MCP tool's), as written: every value after
/// the first point, since the first row has no delta.
fn written_values(result: &serde_json::Value) -> Vec<serde_json::Value> {
    let series = result.as_array().unwrap_or_else(|| panic!("{result}"));
    assert_eq!(series.len(), 1, "{result}");
    let values = series[0]["values"].as_array().unwrap();
    assert!(values.len() >= 3, "{result}");
    values.iter().map(|p| p[1].clone()).collect()
}

/// A value of the query JSON as a number: a number, or a string such as
/// "+Inf" or "0.1".
fn number(v: &serde_json::Value) -> f64 {
    match v {
        serde_json::Value::String(s) if s == "+Inf" => f64::INFINITY,
        serde_json::Value::String(s) => s.parse().unwrap_or_else(|e| panic!("{s}: {e}")),
        v => v.as_f64().unwrap_or_else(|| panic!("not a number: {v}")),
    }
}

/// The p99 result is "+Inf" at every point, with the band
/// `(TOP_BOUND, "+Inf")`, and the p50 result is finite: the end of the
/// stored bucket holding 0.1 s, within 1% of it at grouping power 7.
fn check_p99_and_p50(p99: &serde_json::Value, p50: &serde_json::Value, who: &str) {
    let values = written_values(p99);
    assert!(
        values.iter().all(|v| v == "+Inf"),
        "{who}: p99 is \"+Inf\" at every point: {values:?}"
    );
    let bands = p99[0]
        .get("bands")
        .or_else(|| p99[0].get("intervals"))
        .and_then(|b| b.as_array())
        .unwrap_or_else(|| panic!("{who}: no band: {p99}"));
    for band in bands {
        assert_eq!(number(&band[0]), TOP_BOUND, "{who}: {band}");
        assert_eq!(band[1], "+Inf", "{who}: {band}");
    }
    for v in written_values(p50) {
        let n = number(&v);
        assert!(
            n.is_finite() && (1e8..1.01e8).contains(&n),
            "{who}: p50 {v}"
        );
    }
}

#[test]
fn the_overflow_reads_back_through_mcp() {
    let output = recording();
    let file = output.to_str().unwrap();

    // The overflow counter is one more counter, beside the histogram's
    // `_sum` and `_count`.
    let described = mcp(&["describe-metrics", file]);
    let counters = section(&described, "COUNTERS");
    assert_eq!(
        counters,
        [
            "request_duration_seconds_count",
            "request_duration_seconds_overflow",
            "request_duration_seconds_sum",
        ],
        "{described}"
    );
    assert_eq!(
        section(&described, "HISTOGRAMS"),
        ["request_duration_seconds"],
        "{described}"
    );

    // The text output spells it as the JSON does.
    let text = mcp(&["query", file, P99]);
    assert!(
        text.contains("Last:  ") && text.contains(" = +Inf"),
        "{text}"
    );
    assert!(text.contains("Max:   +Inf"), "{text}");
    assert!(text.contains("+Inf:  "), "{text}");
    assert!(!text.contains(" inf"), "{text}");

    // The MCP server's `query` tool returns the result's JSON.
    let call = |id: u64, query: &str| {
        serde_json::json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {"name": "query", "arguments": {"parquet_file": file, "query": query}},
        })
    };
    let requests = [
        serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
        serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        call(2, P99),
        call(3, P50),
    ];
    let mut child = Command::new(env!("CARGO_BIN_EXE_rezolus"))
        .arg("mcp")
        .env_remove("RUST_LOG")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    {
        let mut stdin = child.stdin.take().unwrap();
        for r in &requests {
            writeln!(stdin, "{r}").unwrap();
        }
    }
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8(out.stdout).unwrap();
    let result_of = |id: u64| -> serde_json::Value {
        let response: serde_json::Value = stdout
            .lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
            .find(|r| r["id"] == id)
            .unwrap_or_else(|| panic!("no response {id}:\n{stdout}"));
        let text = response["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("{response}"));
        let body: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(body["resultType"], "matrix", "{body}");
        body["result"].clone()
    };
    check_p99_and_p50(&result_of(2), &result_of(3), "MCP query tool");
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

fn url_encode(q: &str) -> String {
    q.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-' | b'.' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// The metric names of a `/api/v1/metrics` body with their `metric_type`.
fn catalog(body: &serde_json::Value) -> std::collections::BTreeMap<String, String> {
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

#[test]
fn the_overflow_reads_back_through_both_viewers() {
    let output = recording();
    let overflow = (
        "request_duration_seconds_overflow".to_string(),
        "counter".to_string(),
    );

    // The server viewer.
    let viewer = spawn_viewer(output);
    let metrics = catalog(&get(&viewer, "/api/v1/metrics"));
    assert!(
        metrics.contains_key(&overflow.0) && metrics[&overflow.0] == overflow.1,
        "{metrics:?}"
    );
    let sections = get(&viewer, "/api/v1/sections")["data"].clone();
    let start = sections["start_time"].as_u64().unwrap() as f64 / 1000.0;
    let end = sections["end_time"].as_u64().unwrap() as f64 / 1000.0;
    let range = |q: &str| {
        let body = get(
            &viewer,
            &format!(
                "/api/v1/query_range?query={}&start={start}&end={end}&step=0.2",
                url_encode(q)
            ),
        );
        assert_eq!(body["status"], "success", "{body}");
        body["data"]["result"].clone()
    };
    check_p99_and_p50(&range(P99), &range(P50), "server viewer");
    drop(viewer);

    // The WASM viewer, built natively.
    let bytes = std::fs::read(output).unwrap();
    let wasm = match viewer::Viewer::new(&bytes, "overflow.dendro") {
        Ok(v) => v,
        Err(_) => panic!("the WASM viewer opens the recording"),
    };
    let metrics: serde_json::Value = serde_json::from_str(&wasm.metrics(None)).unwrap();
    let metrics = catalog(&metrics);
    assert!(
        metrics.contains_key(&overflow.0) && metrics[&overflow.0] == overflow.1,
        "WASM {metrics:?}"
    );
    let info: serde_json::Value = serde_json::from_str(&wasm.info()).unwrap();
    let (start, end) = (
        info["minTime"].as_f64().unwrap(),
        info["maxTime"].as_f64().unwrap(),
    );
    let range = |q: &str| -> serde_json::Value {
        let body: serde_json::Value =
            serde_json::from_str(&wasm.query_range(q, start, end, 0.2, "grid", None)).unwrap();
        assert_eq!(body["status"], "success", "{body}");
        body["data"]["result"].clone()
    };
    check_p99_and_p50(&range(P99), &range(P50), "WASM viewer");
}
