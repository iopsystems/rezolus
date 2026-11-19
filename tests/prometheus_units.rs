//! Prometheus series whose names carry a unit token, recorded by
//! `rezolus record` and read back through `mcp describe-metrics`,
//! `mcp query`, `mcp detect-anomalies`, the MCP server's `query` tool, the
//! server viewer's `/api/v1/metrics` and `/api/v1/query_range`, and the
//! WASM viewer's `metrics` and `query_range` (built natively).
//!
//! metriken-storage 0.13 stores each series as the exporter serves it:
//! under the exporter's name, in the exporter's unit, with the `unit` key
//! set from the name's unit token or the `# UNIT` line (`queue_wait_ms` is
//! `milliseconds`). Each check reads a value by the exporter's name and
//! expects the served value; each catalog lists the exporter's names with
//! their units.
//!
//! A second endpoint serves `jobs_total` with a different `# UNIT` line
//! (`seconds` on one, `kilobytes` on the other). Both are stored as
//! `jobs_total`, each in its own unit, and a selector reads both.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// `queue_wait_ms`'s value on every scrape, in milliseconds.
const QUEUE_WAIT_MS: f64 = 2.5;

/// `cpu_busy_seconds_total`'s increase per scrape over `requests_total`'s:
/// 0.5 s per request.
const BUSY_PER_REQUEST_S: f64 = 0.5;

/// The mean of `rpc_latency_seconds` (its `_sum` over its `_count`): 4 ms
/// per observation, in seconds.
const MEAN_LATENCY_S: f64 = 0.004;

/// The bucket every `rpc_latency_seconds` observation falls in, 10 ms, in
/// seconds: the median reads its end, within 1% at grouping power 7.
const LATENCY_BUCKET_S: f64 = 0.01;

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

/// The recording every test reads: both endpoints recorded for 11 seconds
/// at 200 ms (`mcp detect-anomalies` refuses less than 10 seconds) into one
/// archive, made once per test process under `CARGO_TARGET_TMPDIR`, in a
/// directory of the process's own: nextest runs each test in a process. Its
/// two recordings are read combined.
fn recording() -> &'static std::path::Path {
    static ARCHIVE: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    ARCHIVE.get_or_init(|| {
        let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("prometheus_units-{}", std::process::id()));
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

/// Each check: a query by the exporter's name, and what every value of it
/// reads in the exporter's unit.
struct Check {
    query: &'static str,
    want: fn(f64) -> bool,
    what: &'static str,
}

const CHECKS: &[Check] = &[
    Check {
        query: "queue_wait_ms",
        want: |v| v == QUEUE_WAIT_MS,
        what: "2.5 ms",
    },
    Check {
        query: "sum(irate(cpu_busy_seconds_total[1s])) / sum(irate(requests_total[1s]))",
        want: |v| (v - BUSY_PER_REQUEST_S).abs() < 1e-9,
        what: "0.5 s per request",
    },
    Check {
        query: "sum(irate(rpc_latency_seconds_sum[1s])) / \
                sum(irate(rpc_latency_seconds_count[1s]))",
        want: |v| (v - MEAN_LATENCY_S).abs() < 1e-12,
        what: "a 4 ms mean, in seconds",
    },
    Check {
        query: "histogram_quantile(0.5, rpc_latency_seconds)",
        want: |v| (LATENCY_BUCKET_S..LATENCY_BUCKET_S * 1.01).contains(&v),
        what: "the end of the 10 ms bucket, in seconds",
    },
    // The form a KPI percentile chart queries.
    Check {
        query: "histogram_quantiles([0.5], rpc_latency_seconds)",
        want: |v| (LATENCY_BUCKET_S..LATENCY_BUCKET_S * 1.01).contains(&v),
        what: "the end of the 10 ms bucket, in seconds",
    },
    // Both endpoints' `jobs_total`, one per 200 ms scrape each, summed as
    // served. The rate is over the scrapes' real spacing, so it varies.
    Check {
        query: "sum(irate(jobs_total[1s]))",
        want: |v| (8.0..12.0).contains(&v),
        what: "about 10 per second",
    },
];

/// The exporter's names, and no name a unit was rewritten into.
fn check_names(names: &[String], who: &str) {
    for name in [
        "queue_wait_ms",
        "cpu_busy_seconds_total",
        "rpc_latency_seconds",
        "rpc_latency_seconds_sum",
        "rpc_latency_seconds_count",
        "jobs_total",
    ] {
        assert!(
            names.iter().any(|n| n == name),
            "{who}: no {name}: {names:?}"
        );
    }
    assert!(
        !names
            .iter()
            .any(|n| n.contains("nanoseconds") || n.contains("_bytes")),
        "{who}: {names:?}"
    );
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
fn served_series_read_back_through_mcp() {
    let output = recording();
    let file = output.to_str().unwrap();

    // Listed by the exporter's names; no `__` label among the listed labels.
    let described = mcp(&["describe-metrics", file]);
    let mut names = section(&described, "COUNTERS");
    names.extend(section(&described, "GAUGES"));
    names.extend(section(&described, "HISTOGRAMS"));
    check_names(&names, "describe-metrics");
    assert!(!described.contains("__"), "{described}");

    for check in CHECKS {
        let stdout = mcp(&["query", file, check.query]);
        let values = printed_values(&stdout);
        assert_eq!(values.len(), 5, "{}:\n{stdout}", check.query);
        for (key, v) in &values {
            assert!(
                (check.want)(*v),
                "{key} of {} is {v}, not {}",
                check.query,
                check.what
            );
        }
    }

    // One endpoint's `jobs_total`, picked by its recording label.
    let by_alias = mcp(&["query", file, "sum(irate(jobs_total{alias=\"other\"}[1s]))"]);
    let values = printed_values(&by_alias);
    assert_eq!(values.len(), 5, "{by_alias}");
    for (key, v) in &values {
        assert!((4.0..6.0).contains(v), "{key} by alias is {v}:\n{by_alias}");
    }

    // detect-anomalies wraps a bare exporter name by its kind.
    for (name, query) in [
        (
            "cpu_busy_seconds_total",
            "sum(irate(cpu_busy_seconds_total[1m]))",
        ),
        ("queue_wait_ms", "sum(queue_wait_ms)"),
        (
            "rpc_latency_seconds",
            "histogram_quantile(0.99, rpc_latency_seconds)",
        ),
    ] {
        let anomalies = mcp(&["detect-anomalies", file, name]);
        assert!(
            anomalies.contains(&format!("Query: {query}")),
            "{name}:\n{anomalies}"
        );
    }

    // The MCP server's `query` tool names the series by the exporter's name.
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
    let response = stdout
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .find(|r| r["id"] == 2)
        .unwrap_or_else(|| panic!("no response:\n{stdout}"));
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("{response}"));
    let body: serde_json::Value = serde_json::from_str(text).unwrap();
    check_served_series(&body["result"], "MCP query tool");
}

/// A result of `queue_wait_ms`: one series, named by the exporter's name,
/// with no internal label beside `__name__`, every value as served.
fn check_served_series(result: &serde_json::Value, who: &str) {
    let series = result
        .as_array()
        .unwrap_or_else(|| panic!("{who}: {result}"));
    assert_eq!(series.len(), 1, "{who}: {result}");
    let metric = series[0]["metric"]
        .as_object()
        .unwrap_or_else(|| panic!("{who}: {result}"));
    assert_eq!(metric["__name__"], "queue_wait_ms", "{who}: {metric:?}");
    assert!(
        metric
            .keys()
            .all(|k| k == "__name__" || !k.starts_with("__")),
        "{who}: {metric:?}"
    );
    let values = range_values(result);
    assert!(
        values.iter().all(|v| *v == QUEUE_WAIT_MS),
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
/// label keys (what the viewer's label pickers offer) include a `__` label
/// and that each served name carries the unit its token names.
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
    let unit = |name: &str| {
        metrics
            .iter()
            .find(|m| m["name"] == name)
            .and_then(|m| m["unit"].as_str())
    };
    for (name, want) in [
        ("queue_wait_ms", "milliseconds"),
        ("cpu_busy_seconds_total", "seconds"),
        ("rpc_latency_seconds", "seconds"),
        ("rpc_latency_seconds_sum", "seconds"),
    ] {
        assert_eq!(unit(name), Some(want), "{who}: {name}");
    }
    assert_eq!(unit("requests_total"), None, "{who}: no unit token");
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

/// Every check and the served series, through one viewer's range query:
/// `range(q)` is its response body.
fn check_viewer(range: impl Fn(&str) -> serde_json::Value, who: &str) {
    for check in CHECKS {
        let body = range(check.query);
        assert_eq!(body["status"], "success", "{who}: {}: {body}", check.query);
        let values = range_values(&body["data"]["result"]);
        assert!(
            values.iter().all(|v| (check.want)(*v)),
            "{who}: {} is {values:?}, not {}",
            check.query,
            check.what
        );
    }
    check_served_series(&range("queue_wait_ms")["data"]["result"], who);
}

#[test]
fn served_series_read_back_through_both_viewers() {
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
