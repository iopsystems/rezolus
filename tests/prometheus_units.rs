//! Prometheus series whose names carry a unit token, recorded by
//! `rezolus record` and read back through `mcp describe-metrics`,
//! `mcp query`, the MCP server's `query` tool, the server viewer's
//! `/api/v1/metrics` and `/api/v1/query_range`, and the WASM viewer's
//! `metrics` and `query_range` (built natively).
//!
//! metriken-recorder 0.12 stores such a series in nanoseconds or bytes
//! under its name with the token replaced (`queue_wait_ms` as
//! `queue_wait_nanoseconds`, ×1e6), with the exporter's name in the internal
//! label `__prometheus_name__`. metriken-query 0.48 reads a selector naming
//! the exporter's name, when no series is stored under it, from the series
//! renamed from it: in the stored unit, under the stored name. Each check
//! below reads a value by the stored name and by the exporter's name and
//! expects the same number in the stored unit.
//!
//! A second endpoint serves `jobs_total` with a different `# UNIT` line
//! (`seconds` on one, `kilobytes` on the other), so `jobs_total` is stored
//! as `jobs_nanoseconds_total` and as `jobs_bytes_total`; a query naming
//! `jobs_total` over both is an error, whose message must reach the user
//! as metriken-query wrote it.
//!
//! `__prometheus_name__` is an internal label: the metric catalogs' label
//! keys (the viewer's label pickers) and `describe-metrics` must not list
//! it.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// `queue_wait_ms`'s value on every scrape, in milliseconds.
const QUEUE_WAIT_MS: f64 = 2.5;

/// `queue_wait_ms` in the stored unit, nanoseconds.
const QUEUE_WAIT_NS: f64 = QUEUE_WAIT_MS * 1e6;

/// `cpu_busy_seconds_total`'s increase per scrape over `requests_total`'s
/// (0.5 s per request), in nanoseconds.
const BUSY_PER_REQUEST_NS: f64 = 0.5e9;

/// The mean of `rpc_latency_seconds` (its `_sum` over its `_count`), 4 ms
/// per observation, in nanoseconds.
const MEAN_LATENCY_NS: f64 = 4e6;

/// The bucket every `rpc_latency_seconds` observation falls in, 10 ms, in
/// nanoseconds: the median reads its end, within 1% at grouping power 7.
const LATENCY_BUCKET_NS: f64 = 1e7;

/// The service endpoint's body on the `tick`th request: a `_seconds`
/// histogram (10 observations of 4 ms per request), a `_ms` gauge, a
/// `_seconds_total` counter (0.5 s per request), an integer counter, and
/// `jobs_total` with `# UNIT jobs seconds`.
fn service_body(tick: u64) -> String {
    let n = 10 * tick;
    format!(
        "# HELP rpc_latency_seconds RPC latency.\n\
         # TYPE rpc_latency_seconds histogram\n\
         rpc_latency_seconds_bucket{{le=\"0.01\"}} {n}\n\
         rpc_latency_seconds_bucket{{le=\"0.1\"}} {n}\n\
         rpc_latency_seconds_bucket{{le=\"+Inf\"}} {n}\n\
         rpc_latency_seconds_sum {}\n\
         rpc_latency_seconds_count {n}\n\
         # HELP queue_wait_ms Time the newest request waited.\n\
         # TYPE queue_wait_ms gauge\n\
         queue_wait_ms {QUEUE_WAIT_MS}\n\
         # TYPE cpu_busy_seconds_total counter\n\
         cpu_busy_seconds_total {}\n\
         # TYPE requests_total counter\n\
         requests_total {tick}\n\
         # TYPE jobs counter\n\
         # UNIT jobs seconds\n\
         jobs_total {tick}\n",
        n as f64 * 0.004,
        tick as f64 * 0.5 + 0.25,
    )
}

/// The second endpoint's body: `jobs_total` with `# UNIT jobs kilobytes`.
fn other_body(tick: u64) -> String {
    format!("# TYPE jobs counter\n# UNIT jobs kilobytes\njobs_total {tick}\n")
}

/// A stand-in Prometheus exporter serving `body(tick)` on the `tick`th
/// request.
fn spawn_exporter(body: fn(u64) -> String) -> u16 {
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
            let body = body(tick);
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

/// The recording every test reads: both endpoints recorded for 3 seconds
/// at 200 ms into one archive, made once per test run under
/// `CARGO_TARGET_TMPDIR`. Its two recordings are read combined.
fn recording() -> &'static std::path::Path {
    static ARCHIVE: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    ARCHIVE.get_or_init(|| {
        let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("prometheus_units");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let output = dir.join("units.dendro");
        let service = spawn_exporter(service_body);
        let other = spawn_exporter(other_body);
        let out = Command::new(env!("CARGO_BIN_EXE_rezolus"))
            .arg("record")
            .arg("--endpoint")
            .arg(format!("http://127.0.0.1:{service}/metrics,alias=svc"))
            .arg("--endpoint")
            .arg(format!("http://127.0.0.1:{other}/metrics,alias=other"))
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

/// Each check: a query by the stored name, the same query by the
/// exporter's name, and what every value of both reads in the stored unit.
struct Check {
    stored: &'static str,
    exporter: &'static str,
    want: fn(f64) -> bool,
    what: &'static str,
}

const CHECKS: &[Check] = &[
    Check {
        stored: "queue_wait_nanoseconds",
        exporter: "queue_wait_ms",
        want: |v| v == QUEUE_WAIT_NS,
        what: "2.5 ms in nanoseconds",
    },
    Check {
        stored: "sum(irate(cpu_busy_nanoseconds_total[1s])) / sum(irate(requests_total[1s]))",
        exporter: "sum(irate(cpu_busy_seconds_total[1s])) / sum(irate(requests_total[1s]))",
        want: |v| (v - BUSY_PER_REQUEST_NS).abs() < 1e-3,
        what: "0.5 s per request in nanoseconds",
    },
    Check {
        stored: "sum(irate(rpc_latency_nanoseconds_sum[1s])) / \
                 sum(irate(rpc_latency_nanoseconds_count[1s]))",
        exporter: "sum(irate(rpc_latency_seconds_sum[1s])) / \
                   sum(irate(rpc_latency_seconds_count[1s]))",
        want: |v| (v - MEAN_LATENCY_NS).abs() < 1e-3,
        what: "a 4 ms mean in nanoseconds",
    },
    Check {
        stored: "histogram_quantile(0.5, rpc_latency_nanoseconds)",
        exporter: "histogram_quantile(0.5, rpc_latency_seconds)",
        want: |v| (LATENCY_BUCKET_NS..LATENCY_BUCKET_NS * 1.01).contains(&v),
        what: "the end of the 10 ms bucket in nanoseconds",
    },
    // The form a KPI percentile chart queries; the shipped KPI templates
    // name `_seconds` histograms by the exporter's name.
    Check {
        stored: "histogram_quantiles([0.5], rpc_latency_nanoseconds)",
        exporter: "histogram_quantiles([0.5], rpc_latency_seconds)",
        want: |v| (LATENCY_BUCKET_NS..LATENCY_BUCKET_NS * 1.01).contains(&v),
        what: "the end of the 10 ms bucket in nanoseconds",
    },
];

/// The query naming `jobs_total`, which both endpoints' series were renamed
/// from, under two names.
const AMBIGUOUS: &str = "sum(irate(jobs_total[1s]))";

/// What the ambiguity error must carry, as metriken-query writes it: both
/// stored names and the matcher that picks one.
fn check_ambiguity_message(message: &str, who: &str) {
    for part in [
        "jobs_nanoseconds_total",
        "jobs_bytes_total",
        "{__prometheus_name__=\"jobs_total\"}",
    ] {
        assert!(message.contains(part), "{who}: no {part} in {message:?}");
    }
    assert!(!message.contains("malformed"), "{who}: {message:?}");
}

/// The stored names, and none of the exporter names, are listed.
fn check_names(names: &[String], who: &str) {
    for name in [
        "queue_wait_nanoseconds",
        "cpu_busy_nanoseconds_total",
        "rpc_latency_nanoseconds",
        "rpc_latency_nanoseconds_sum",
        "rpc_latency_nanoseconds_count",
        "jobs_nanoseconds_total",
        "jobs_bytes_total",
    ] {
        assert!(
            names.iter().any(|n| n == name),
            "{who}: no {name}: {names:?}"
        );
    }
    for name in [
        "queue_wait_ms",
        "cpu_busy_seconds_total",
        "rpc_latency_seconds",
        "jobs_total",
    ] {
        assert!(!names.iter().any(|n| n == name), "{who}: {name}: {names:?}");
    }
}

/// `rezolus mcp <args>`'s exit status, stdout and stderr.
fn mcp_output(args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_rezolus"))
        .arg("mcp")
        .args(args)
        .output()
        .expect("run rezolus mcp");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// `rezolus mcp <args>`'s stdout, asserting it exits 0.
fn mcp(args: &[&str]) -> String {
    let (ok, stdout, stderr) = mcp_output(args);
    assert!(
        ok,
        "rezolus mcp {args:?} exits 0\nstdout:\n{stdout}\nstderr:\n{stderr}"
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

#[test]
fn renamed_series_read_back_through_mcp() {
    let output = recording();
    let file = output.to_str().unwrap();

    // Listed by the stored names; no `__` label among the listed labels.
    let described = mcp(&["describe-metrics", file]);
    let mut names = section(&described, "COUNTERS");
    names.extend(section(&described, "GAUGES"));
    names.extend(section(&described, "HISTOGRAMS"));
    check_names(&names, "describe-metrics");
    assert!(!described.contains("__"), "{described}");

    // Each value by the stored name and by the exporter's, in the stored
    // unit.
    for check in CHECKS {
        for query in [check.stored, check.exporter] {
            let stdout = mcp(&["query", file, query]);
            let values = printed_values(&stdout);
            assert_eq!(values.len(), 5, "{query}:\n{stdout}");
            for (key, v) in &values {
                assert!(
                    (check.want)(*v),
                    "{key} of {query} is {v}, not {}",
                    check.what
                );
            }
        }
    }

    // The ambiguity error, as metriken-query wrote it.
    let (ok, stdout, stderr) = mcp_output(&["query", file, AMBIGUOUS]);
    assert!(!ok, "{stdout}");
    check_ambiguity_message(&stderr, "mcp query");
    // The form the error names reads one of them: `jobs_total` from the
    // `# UNIT jobs seconds` endpoint, 5 per second, in nanoseconds per
    // second (the other reads 5000 bytes per second).
    let escape = "sum(irate(jobs_nanoseconds_total{__prometheus_name__=\"jobs_total\"}[1s]))";
    let stdout = mcp(&["query", file, escape]);
    let values = printed_values(&stdout);
    assert_eq!(values.len(), 5, "{stdout}");
    for (key, v) in &values {
        assert!(*v > 1e9, "{key} of {escape} is {v}:\n{stdout}");
    }

    // The MCP server's `query` tool: the fallback's result names the series
    // by the stored name, and the ambiguity error reaches the reply.
    let call = |id: u64, query: &str| {
        serde_json::json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {"name": "query", "arguments": {"parquet_file": file, "query": query}},
        })
    };
    let requests = [
        serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
        serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        call(2, "queue_wait_ms"),
        call(3, AMBIGUOUS),
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
    let response = |id: u64| -> serde_json::Value {
        stdout
            .lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
            .find(|r| r["id"] == id)
            .unwrap_or_else(|| panic!("no response {id}:\n{stdout}"))
    };
    let fallback = response(2);
    let text = fallback["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("{fallback}"));
    let body: serde_json::Value = serde_json::from_str(text).unwrap();
    check_fallback_series(&body["result"], "MCP query tool");
    let error = response(3);
    let message = error["error"]["message"]
        .as_str()
        .unwrap_or_else(|| panic!("{error}"));
    check_ambiguity_message(message, "MCP query tool");
}

/// A result of `queue_wait_ms` read through the fallback: one series, named
/// by the stored name, its exporter's name in `__prometheus_name__`, every
/// value in nanoseconds.
fn check_fallback_series(result: &serde_json::Value, who: &str) {
    let series = result
        .as_array()
        .unwrap_or_else(|| panic!("{who}: {result}"));
    assert_eq!(series.len(), 1, "{who}: {result}");
    let metric = &series[0]["metric"];
    assert_eq!(
        metric["__name__"], "queue_wait_nanoseconds",
        "{who}: {metric}"
    );
    assert_eq!(
        metric["__prometheus_name__"], "queue_wait_ms",
        "{who}: {metric}"
    );
    let values = range_values(result);
    assert!(
        values.iter().all(|v| *v == QUEUE_WAIT_NS),
        "{who}: {values:?}"
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

fn url_encode(q: &str) -> String {
    q.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-' | b'.' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// A `/api/v1/metrics` body's metric names, asserting that no metric's
/// label keys (what the viewer's label pickers offer) include a `__` label.
fn catalog_names(body: &serde_json::Value, who: &str) -> Vec<String> {
    let metrics = body["metrics"]
        .as_array()
        .unwrap_or_else(|| panic!("{who}: no metrics: {body}"));
    for m in metrics {
        let keys = m["label_keys"].as_array().unwrap();
        assert!(!keys.is_empty(), "{who}: {m}");
        assert!(
            keys.iter().all(|k| !k.as_str().unwrap().starts_with("__")),
            "{who}: {m}"
        );
    }
    metrics
        .iter()
        .map(|m| m["name"].as_str().unwrap().to_string())
        .collect()
}

/// Every value after the first point of every series of a matrix result
/// (`data.result` of a viewer's response), which a histogram function
/// leaves without a delta.
fn range_values(result: &serde_json::Value) -> Vec<f64> {
    let series = result.as_array().unwrap_or_else(|| panic!("{result}"));
    assert!(!series.is_empty(), "{result}");
    series
        .iter()
        .flat_map(|s| s["values"].as_array().unwrap().iter().skip(1))
        .map(|p| match &p[1] {
            serde_json::Value::String(s) => s.parse().unwrap(),
            v => v.as_f64().unwrap(),
        })
        .collect()
}

/// Every check, the fallback's series and the ambiguity error, through one
/// viewer's range query: `range(q)` is its response body.
fn check_viewer(range: impl Fn(&str) -> serde_json::Value, who: &str) {
    for check in CHECKS {
        for query in [check.stored, check.exporter] {
            let body = range(query);
            assert_eq!(body["status"], "success", "{who}: {query}: {body}");
            let values = range_values(&body["data"]["result"]);
            assert!(
                values.iter().all(|v| (check.want)(*v)),
                "{who}: {query} is {values:?}, not {}",
                check.what
            );
        }
    }
    check_fallback_series(&range("queue_wait_ms")["data"]["result"], who);
    let body = range(AMBIGUOUS);
    assert_eq!(body["status"], "error", "{who}: {body}");
    check_ambiguity_message(body["error"].as_str().unwrap(), who);
}

#[test]
fn renamed_series_read_back_through_both_viewers() {
    let output = recording();

    // The server viewer.
    let viewer = spawn_viewer(output);
    let names = catalog_names(&get(&viewer, "/api/v1/metrics"), "server viewer");
    check_names(&names, "server viewer");
    let sections = get(&viewer, "/api/v1/sections")["data"].clone();
    let start = sections["start_time"].as_u64().unwrap() as f64 / 1000.0;
    let end = sections["end_time"].as_u64().unwrap() as f64 / 1000.0;
    check_viewer(
        |q| {
            get(
                &viewer,
                &format!(
                    "/api/v1/query_range?query={}&start={start}&end={end}&step=0.2",
                    url_encode(q)
                ),
            )
        },
        "server viewer",
    );
    drop(viewer);

    // The WASM viewer, built natively.
    let bytes = std::fs::read(output).unwrap();
    let wasm = match viewer::Viewer::new(&bytes, "units.dendro") {
        Ok(v) => v,
        Err(_) => panic!("the WASM viewer opens the recording"),
    };
    let metrics: serde_json::Value = serde_json::from_str(&wasm.metrics(None)).unwrap();
    let names = catalog_names(&metrics, "WASM viewer");
    check_names(&names, "WASM viewer");
    let info: serde_json::Value = serde_json::from_str(&wasm.info()).unwrap();
    let (start, end) = (
        info["minTime"].as_f64().unwrap(),
        info["maxTime"].as_f64().unwrap(),
    );
    check_viewer(
        |q| serde_json::from_str(&wasm.query_range(q, start, end, 0.2, "grid", None)).unwrap(),
        "WASM viewer",
    );
}
