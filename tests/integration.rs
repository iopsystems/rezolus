//! Integration tests for the Rezolus agent.
//!
//! These tests start the agent binary, wait for it to begin serving metrics,
//! and verify that expected metrics are present. They require:
//!
//! - Linux (BPF samplers are Linux-only)
//! - Root privileges (BPF program loading requires CAP_BPF / root)
//! - The binary to be built before running tests
//!
//! Run with: `sudo cargo test --test integration -- --ignored`

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Represents a metric from the JSON snapshot endpoint.
#[derive(Debug, serde::Deserialize)]
struct Counter {
    #[allow(dead_code)]
    name: String,
    value: u64,
    metadata: HashMap<String, String>,
}

#[derive(Debug, serde::Deserialize)]
struct Gauge {
    #[allow(dead_code)]
    name: String,
    #[allow(dead_code)]
    value: i64,
    metadata: HashMap<String, String>,
}

#[derive(Debug, serde::Deserialize)]
struct Histogram {
    #[allow(dead_code)]
    name: String,
    metadata: HashMap<String, String>,
}

/// The JSON snapshot from /metrics/json (V2 format, serde untagged).
#[derive(Debug, serde::Deserialize)]
struct Snapshot {
    #[allow(dead_code)]
    metadata: HashMap<String, String>,
    counters: Vec<Counter>,
    gauges: Vec<Gauge>,
    histograms: Vec<Histogram>,
}

/// Find the rezolus binary, preferring release then debug.
fn find_binary() -> String {
    let candidates = [
        "target/release/rezolus",
        "target/debug/rezolus",
        // When run from workspace root
        "../target/release/rezolus",
        "../target/debug/rezolus",
    ];
    for path in &candidates {
        if std::path::Path::new(path).exists() {
            return path.to_string();
        }
    }
    panic!(
        "rezolus binary not found. Build it first with `cargo build` or `cargo build --release`"
    );
}

/// RAII guard that kills the agent process on drop.
struct AgentProcess {
    child: Child,
    port: u16,
}

impl AgentProcess {
    /// Start the agent on the given port and wait until it's ready.
    fn start(port: u16) -> Self {
        let binary = find_binary();

        // Write a minimal config to a temp file
        let config_content = format!(
            r#"
[general]
listen = "127.0.0.1:{port}"
ttl = "10ms"

[log]
level = "info"

[defaults]
enabled = true

# Disable samplers that may not be available in CI
[samplers.gpu_nvidia]
enabled = false

[samplers.gpu_apple]
enabled = false

# Disable perf counter samplers that may fail in VMs / containers
[samplers.cpu_frequency]
enabled = false

[samplers.cpu_branch]
enabled = false

[samplers.cpu_dtlb]
enabled = false

[samplers.cpu_l3]
enabled = false

[samplers.cpu_perf]
enabled = false

# Disable ethtool - requires specific NIC drivers
[samplers.network_ethtool]
enabled = false
"#
        );

        let config_path = format!("/tmp/rezolus-test-{port}.toml");
        std::fs::write(&config_path, config_content).expect("failed to write test config");

        let child = Command::new(&binary)
            .arg(&config_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| panic!("failed to start {binary}: {e}"));

        let mut agent = Self { child, port };
        agent.wait_ready();
        agent
    }

    /// Poll the HTTP endpoint until it responds or timeout.
    fn wait_ready(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(30);
        let addr = format!("127.0.0.1:{}", self.port);

        while Instant::now() < deadline {
            if let Ok(_stream) = TcpStream::connect_timeout(
                &addr.parse().unwrap(),
                Duration::from_millis(100),
            ) {
                // Give the agent a moment to finish initializing all samplers
                std::thread::sleep(Duration::from_secs(2));
                return;
            }
            std::thread::sleep(Duration::from_millis(200));
        }

        // On timeout, dump stderr for debugging
        self.dump_stderr();
        panic!("agent did not start within 30s on port {}", self.port);
    }

    /// Fetch the JSON snapshot from the agent.
    fn fetch_snapshot(&self) -> Snapshot {
        let url = format!("http://127.0.0.1:{}/metrics/json", self.port);
        let resp = reqwest::blocking::get(&url)
            .unwrap_or_else(|e| panic!("failed to fetch {url}: {e}"));
        assert!(resp.status().is_success(), "HTTP {}", resp.status());
        let body = resp.text().expect("failed to read response body");
        serde_json::from_str::<Snapshot>(&body).expect("failed to parse JSON snapshot")
    }

    /// Dump agent stderr for debugging test failures.
    fn dump_stderr(&mut self) {
        if let Some(stderr) = self.child.stderr.take() {
            let reader = BufReader::new(stderr);
            eprintln!("--- agent stderr ---");
            for line in reader.lines().take(50).flatten() {
                eprintln!("  {line}");
            }
            eprintln!("--- end stderr ---");
        }
    }
}

impl Drop for AgentProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();

        // Clean up config file
        let config_path = format!("/tmp/rezolus-test-{}.toml", self.port);
        let _ = std::fs::remove_file(config_path);
    }
}

// Helper: collect all counter metric names from a snapshot
fn counter_metric_names(snapshot: &Snapshot) -> Vec<&str> {
    snapshot
        .counters
        .iter()
        .filter_map(|c| c.metadata.get("metric").map(|s| s.as_str()))
        .collect()
}

// Helper: collect all gauge metric names from a snapshot
fn gauge_metric_names(snapshot: &Snapshot) -> Vec<&str> {
    snapshot
        .gauges
        .iter()
        .filter_map(|g| g.metadata.get("metric").map(|s| s.as_str()))
        .collect()
}

// Helper: collect all histogram metric names from a snapshot
fn histogram_metric_names(snapshot: &Snapshot) -> Vec<&str> {
    snapshot
        .histograms
        .iter()
        .filter_map(|h| h.metadata.get("metric").map(|s| s.as_str()))
        .collect()
}

/// Verify the agent starts and serves a valid snapshot.
#[test]
#[ignore]
fn agent_serves_metrics() {
    let agent = AgentProcess::start(14241);
    let snapshot = agent.fetch_snapshot();

    // Should have some counters, gauges, or histograms
    let total = snapshot.counters.len() + snapshot.gauges.len() + snapshot.histograms.len();
    assert!(total > 0, "snapshot is empty — no metrics collected");
}

/// Verify CPU usage metrics are present (from cpu_usage BPF sampler).
#[test]
#[ignore]
fn cpu_usage_metrics() {
    let agent = AgentProcess::start(14242);
    let snapshot = agent.fetch_snapshot();
    let names = counter_metric_names(&snapshot);

    assert!(
        names.contains(&"cpu_usage"),
        "expected cpu_usage counter, got: {names:?}"
    );

    // Verify per-CPU breakdown exists with state metadata
    let cpu_usage_counters: Vec<_> = snapshot
        .counters
        .iter()
        .filter(|c| c.metadata.get("metric").map(|s| s.as_str()) == Some("cpu_usage"))
        .collect();

    assert!(
        cpu_usage_counters.len() >= 2,
        "expected at least 2 cpu_usage counters (user + system), got {}",
        cpu_usage_counters.len()
    );

    // Check that state metadata is present
    let states: Vec<&str> = cpu_usage_counters
        .iter()
        .filter_map(|c| c.metadata.get("state").map(|s| s.as_str()))
        .collect();
    assert!(states.contains(&"user"), "missing cpu_usage state=user");
    assert!(
        states.contains(&"system"),
        "missing cpu_usage state=system"
    );

    // At least some CPU should have non-zero usage
    let has_nonzero = cpu_usage_counters.iter().any(|c| c.value > 0);
    assert!(has_nonzero, "all cpu_usage counters are zero");
}

/// Verify scheduler runqueue histogram metrics are present.
#[test]
#[ignore]
fn scheduler_runqueue_metrics() {
    let agent = AgentProcess::start(14243);
    let snapshot = agent.fetch_snapshot();
    let histogram_names = histogram_metric_names(&snapshot);

    // The scheduler_runqueue sampler produces histograms for runqlat, running, offcpu
    assert!(
        histogram_names.contains(&"scheduler_runqueue_latency"),
        "expected scheduler_runqueue_latency histogram, got: {histogram_names:?}"
    );
}

/// Verify memory metrics are present (from /proc/meminfo and /proc/vmstat).
#[test]
#[ignore]
fn memory_metrics() {
    let agent = AgentProcess::start(14244);
    let snapshot = agent.fetch_snapshot();
    let gauge_names = gauge_metric_names(&snapshot);

    assert!(
        gauge_names.contains(&"memory_meminfo"),
        "expected memory_meminfo gauge, got: {gauge_names:?}"
    );

    // Memory total should be non-zero
    let meminfo_gauges: Vec<_> = snapshot
        .gauges
        .iter()
        .filter(|g| g.metadata.get("metric").map(|s| s.as_str()) == Some("memory_meminfo"))
        .collect();
    assert!(
        !meminfo_gauges.is_empty(),
        "no memory_meminfo gauges found"
    );
}

/// Verify the softirq metrics from the cpu_usage sampler.
#[test]
#[ignore]
fn softirq_metrics() {
    let agent = AgentProcess::start(14245);
    let snapshot = agent.fetch_snapshot();
    let names = counter_metric_names(&snapshot);

    assert!(
        names.contains(&"softirq"),
        "expected softirq counter, got: {names:?}"
    );

    // Check some expected softirq kinds
    let softirq_kinds: Vec<&str> = snapshot
        .counters
        .iter()
        .filter(|c| c.metadata.get("metric").map(|s| s.as_str()) == Some("softirq"))
        .filter_map(|c| c.metadata.get("kind").map(|s| s.as_str()))
        .collect();
    assert!(
        softirq_kinds.contains(&"timer"),
        "expected softirq kind=timer"
    );
    assert!(
        softirq_kinds.contains(&"sched"),
        "expected softirq kind=sched"
    );
}

/// Verify that BPF run time/count metrics are present (self-telemetry).
#[test]
#[ignore]
fn bpf_self_telemetry() {
    let agent = AgentProcess::start(14246);
    let snapshot = agent.fetch_snapshot();
    let names = counter_metric_names(&snapshot);

    assert!(
        names.contains(&"rezolus_bpf_run_count"),
        "expected rezolus_bpf_run_count counter, got: {names:?}"
    );
    assert!(
        names.contains(&"rezolus_bpf_run_time"),
        "expected rezolus_bpf_run_time counter, got: {names:?}"
    );

    // BPF programs should have run at least once
    let run_counts: Vec<_> = snapshot
        .counters
        .iter()
        .filter(|c| {
            c.metadata.get("metric").map(|s| s.as_str()) == Some("rezolus_bpf_run_count")
        })
        .collect();
    let has_nonzero = run_counts.iter().any(|c| c.value > 0);
    assert!(has_nonzero, "all rezolus_bpf_run_count are zero");
}

/// Verify network interface metrics are present.
#[test]
#[ignore]
fn network_interface_metrics() {
    let agent = AgentProcess::start(14247);
    let snapshot = agent.fetch_snapshot();
    let names = counter_metric_names(&snapshot);

    assert!(
        names.contains(&"network_interface"),
        "expected network_interface counter, got: {names:?}"
    );
}

/// Verify that taking two snapshots shows monotonically increasing counters.
#[test]
#[ignore]
fn counters_are_monotonic() {
    let agent = AgentProcess::start(14248);

    let snap1 = agent.fetch_snapshot();
    std::thread::sleep(Duration::from_secs(2));
    let snap2 = agent.fetch_snapshot();

    // Build a map of counter name -> max value for each snapshot
    let max_value = |snap: &Snapshot, metric: &str| -> u64 {
        snap.counters
            .iter()
            .filter(|c| c.metadata.get("metric").map(|s| s.as_str()) == Some(metric))
            .map(|c| c.value)
            .max()
            .unwrap_or(0)
    };

    // cpu_usage should be monotonically increasing (cumulative nanoseconds)
    let cpu1 = max_value(&snap1, "cpu_usage");
    let cpu2 = max_value(&snap2, "cpu_usage");
    assert!(
        cpu2 >= cpu1,
        "cpu_usage decreased: {cpu1} -> {cpu2}"
    );
    assert!(
        cpu2 > cpu1,
        "cpu_usage did not increase between snapshots: {cpu1} -> {cpu2}"
    );
}

/// Verify syscall count metrics are present.
#[test]
#[ignore]
fn syscall_count_metrics() {
    let agent = AgentProcess::start(14249);
    let snapshot = agent.fetch_snapshot();
    let names = counter_metric_names(&snapshot);

    assert!(
        names.contains(&"syscall"),
        "expected syscall counter, got: {names:?}"
    );

    // The agent itself generates syscalls, so there should be non-zero counts
    let syscall_counters: Vec<_> = snapshot
        .counters
        .iter()
        .filter(|c| c.metadata.get("metric").map(|s| s.as_str()) == Some("syscall"))
        .filter(|c| c.value > 0)
        .collect();
    assert!(
        !syscall_counters.is_empty(),
        "all syscall counters are zero"
    );
}

/// Verify CPU cores gauge is present and reports a reasonable value.
#[test]
#[ignore]
fn cpu_cores_metric() {
    let agent = AgentProcess::start(14250);
    let snapshot = agent.fetch_snapshot();
    let gauge_names = gauge_metric_names(&snapshot);

    assert!(
        gauge_names.contains(&"cpu_cores"),
        "expected cpu_cores gauge, got: {gauge_names:?}"
    );

    let cores_gauge = snapshot
        .gauges
        .iter()
        .find(|g| g.metadata.get("metric").map(|s| s.as_str()) == Some("cpu_cores"))
        .expect("cpu_cores gauge not found");
    assert!(
        cores_gauge.value > 0,
        "cpu_cores should be > 0, got {}",
        cores_gauge.value
    );
}

/// Verify rezolus self-monitoring (rusage) metrics.
#[test]
#[ignore]
fn rezolus_rusage_metrics() {
    let agent = AgentProcess::start(14251);
    let snapshot = agent.fetch_snapshot();
    let gauge_names = gauge_metric_names(&snapshot);

    assert!(
        gauge_names.contains(&"rezolus_rusage"),
        "expected rezolus_rusage gauge, got: {gauge_names:?}"
    );
}
