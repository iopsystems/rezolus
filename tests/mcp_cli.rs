//! End-to-end CLI tests for the MCP subcommands.
//!
//! These tests spawn the `rezolus` binary as a child process, pass
//! real CLI arguments (parquet path + query string), and inspect
//! stdout / exit code. They verify the full path:
//!
//!   clap argument parsing → `mcp::run(config)` →
//!   `run_<subcommand>` → `open_capture` →
//!   `DuckDbBackend::run_sql` → output formatting.
//!
//! If any of those layers regresses — including the thin shim that
//! the in-process unit tests don't touch — these tests fail.
//!
//! Skipped silently when the demo fixture is missing (CI environments
//! that don't ship `site/viewer/data/demo.parquet`).

use std::path::{Path, PathBuf};
use std::process::Command;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn find_binary() -> Option<PathBuf> {
    // Prefer the debug binary built by `cargo test`; release if present.
    let candidates = [
        workspace_root().join("target/debug/rezolus"),
        workspace_root().join("target/release/rezolus"),
    ];
    candidates.into_iter().find(|p| p.exists())
}

fn demo_parquet() -> PathBuf {
    workspace_root().join("site/viewer/data/demo.parquet")
}

/// Run `rezolus mcp <args...>` and return (stdout, stderr, exit_code).
fn run_mcp(args: &[&str]) -> (String, String, i32) {
    let binary = find_binary().expect(
        "rezolus binary not found — run `cargo build` before this test",
    );
    let output = Command::new(&binary)
        .arg("mcp")
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("failed to spawn {}: {e}", binary.display()));
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let code = output.status.code().unwrap_or(-1);
    (stdout, stderr, code)
}

fn skip_if_no_fixture() -> Option<()> {
    if !demo_parquet().exists() {
        eprintln!("skipping: fixture {} missing", demo_parquet().display());
        return None;
    }
    if find_binary().is_none() {
        eprintln!("skipping: rezolus binary missing (run `cargo build` first)");
        return None;
    }
    Some(())
}

fn demo_parquet_arg() -> String {
    demo_parquet().to_string_lossy().to_string()
}

// ─── describe-recording ──────────────────────────────────────────────

/// CLI dispatch for `mcp describe-recording <parquet>` works end-to-end:
/// the binary opens the parquet via SqlCapture, reads metadata, and
/// prints the recording-info header.
#[test]
fn cli_describe_recording_against_demo_parquet() {
    if skip_if_no_fixture().is_none() {
        return;
    }
    let (stdout, stderr, code) = run_mcp(&["describe-recording", &demo_parquet_arg()]);
    assert_eq!(code, 0, "exit code: stderr={stderr}");
    assert!(
        stdout.contains("Recording Information"),
        "missing header in stdout: {stdout}",
    );
    assert!(
        stdout.contains("Source: rezolus"),
        "missing 'Source: rezolus': {stdout}",
    );
}

// ─── describe-metrics ────────────────────────────────────────────────

/// CLI dispatch for `mcp describe-metrics <parquet>`: prints the
/// COUNTERS / GAUGES / HISTOGRAMS sections, lists `cpu_cycles` (a
/// known counter on demo.parquet), and reports the sampling interval.
#[test]
fn cli_describe_metrics_against_demo_parquet() {
    if skip_if_no_fixture().is_none() {
        return;
    }
    let (stdout, stderr, code) = run_mcp(&["describe-metrics", &demo_parquet_arg()]);
    assert_eq!(code, 0, "exit code: stderr={stderr}");
    assert!(stdout.contains("COUNTERS"), "missing COUNTERS section: {stdout}");
    assert!(stdout.contains("GAUGES"), "missing GAUGES section: {stdout}");
    assert!(stdout.contains("HISTOGRAMS"), "missing HISTOGRAMS section: {stdout}");
    assert!(stdout.contains("• cpu_cycles"), "missing cpu_cycles entry: {stdout}");
    assert!(stdout.contains("Sampling interval: 1000ms"), "missing interval line");
}

// ─── query (the headline DuckDB-via-MCP test) ────────────────────────

/// CLI dispatch for `mcp query <parquet> <SQL>`. This is the test the
/// "did we actually call DuckDB end-to-end through MCP?" question
/// turns on. We pass a real DuckDB SELECT, expect the binary to
/// open the parquet, run it through DuckDbBackend, and print a
/// pretty-formatted text table. `count(*)` against demo.parquet is
/// 302 (301-second 1Hz recording + final sample).
#[test]
fn cli_query_runs_duckdb_sql() {
    if skip_if_no_fixture().is_none() {
        return;
    }
    let (stdout, stderr, code) = run_mcp(&[
        "query",
        &demo_parquet_arg(),
        "SELECT count(*) AS n FROM _src",
    ]);
    assert_eq!(code, 0, "exit code: stderr={stderr}");
    // The pretty-formatted table includes `| n` as the column header
    // and `| 302` for the count row.
    assert!(stdout.contains("| n"), "missing column header: {stdout}");
    assert!(stdout.contains("302"), "missing count value: {stdout}");
}

/// CLI dispatch for `mcp query` with the SHARED_MACROS layer. Confirms
/// the macros are registered on the connection before the SQL runs —
/// otherwise `irate_1s` would binder-error.
#[test]
fn cli_query_uses_shared_macros() {
    if skip_if_no_fixture().is_none() {
        return;
    }
    let (stdout, stderr, code) = run_mcp(&[
        "query",
        &demo_parquet_arg(),
        "SELECT irate_1s(\"cpu_cycles/0\", timestamp) FROM _src LIMIT 5",
    ]);
    assert_eq!(code, 0, "exit code: stderr={stderr}");
    // First row gets NULL from the LAG; remaining rows produce finite
    // f64 values. We only need to confirm the macro bound and the
    // statement returned rows.
    assert!(stdout.contains("|"), "no table output: {stdout}");
}

/// CLI dispatch for malformed SQL: process exits non-zero, error
/// surfaces on stderr. Pinned because `run_query` catches `Err` from
/// `execute_query` and calls `std::process::exit(1)` — a regression
/// that swallowed the failure or panicked instead of exiting cleanly
/// would surface here.
#[test]
fn cli_query_rejects_malformed_sql() {
    if skip_if_no_fixture().is_none() {
        return;
    }
    let (_stdout, stderr, code) = run_mcp(&[
        "query",
        &demo_parquet_arg(),
        "SELECT bogus_column FROM _src",
    ]);
    assert_ne!(code, 0, "malformed SQL should exit non-zero");
    assert!(
        stderr.to_lowercase().contains("query failed"),
        "expected 'Query failed' prefix on stderr, got: {stderr}",
    );
}

// ─── detect-anomalies ────────────────────────────────────────────────

/// CLI dispatch for `mcp detect-anomalies <parquet> <metric_name>`.
/// Auto-resolves the bare metric name to canonical SQL via
/// `resolve_query_to_sql`, runs through DuckDB, computes the
/// statistical analysis. Confirms the full chain works.
#[test]
fn cli_detect_anomalies_bare_metric_name() {
    if skip_if_no_fixture().is_none() {
        return;
    }
    let (stdout, stderr, code) = run_mcp(&[
        "detect-anomalies",
        &demo_parquet_arg(),
        "cpu_cores",
    ]);
    assert_eq!(code, 0, "exit code: stderr={stderr}");
    assert!(
        stdout.contains("Anomaly Detection Analysis"),
        "missing header: {stdout}",
    );
    // The Query line should echo the SQL we built (with the COALESCE
    // gauge-sum shape) rather than the bare metric name — verifies
    // the auto-resolution happened.
    assert!(
        stdout.contains("FROM _src"),
        "query line should record executed SQL: {stdout}",
    );
}

// ─── analyze-correlation ─────────────────────────────────────────────

/// CLI dispatch for `mcp analyze-correlation <parquet> <m1> <m2>`.
/// Two bare metric names, both auto-resolved, both run through
/// DuckDB, correlation computed across the resulting series. The
/// max correlation between cpu_cycles and cpu_instructions on
/// demo.parquet is ~0.97 (CPUs run instructions to generate
/// cycles); we use a loose threshold to survive minor numerical
/// drift across recordings.
#[test]
fn cli_analyze_correlation_two_metrics() {
    if skip_if_no_fixture().is_none() {
        return;
    }
    let (stdout, stderr, code) = run_mcp(&[
        "analyze-correlation",
        &demo_parquet_arg(),
        "cpu_cycles",
        "cpu_instructions",
    ]);
    assert_eq!(code, 0, "exit code: stderr={stderr}");
    assert!(
        stdout.contains("Cross-Correlation Analysis"),
        "missing header: {stdout}",
    );
    // The "Max correlation: 0.NNNN" line should report a strong
    // correlation (|r| > 0.5). Parsing the number out is brittle;
    // checking that the string is present + that a value > "0.5"
    // appears on the same line is the right specificity.
    let line = stdout
        .lines()
        .find(|l| l.contains("Max correlation:"))
        .expect("Max correlation line missing");
    let value: f64 = line
        .split_whitespace()
        .last()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.0);
    assert!(
        value.abs() > 0.5,
        "cpu_cycles ↔ cpu_instructions should be strongly correlated, got {value}",
    );
}

// ─── help text reflects SQL semantics ────────────────────────────────

/// `mcp query --help` describes SQL (not PromQL) — pins the CLI
/// contract documented in CHANGELOG. Catches an accidental revert of
/// the help-string update.
#[test]
fn cli_query_help_mentions_sql_not_promql() {
    if find_binary().is_none() {
        eprintln!("skipping: rezolus binary missing");
        return;
    }
    let binary = find_binary().unwrap();
    let output = Command::new(&binary)
        .args(["mcp", "query", "--help"])
        .output()
        .expect("spawn rezolus mcp query --help");
    let help = String::from_utf8_lossy(&output.stdout).to_string();
    assert!(help.contains("DuckDB"), "help should mention DuckDB: {help}");
    assert!(!help.contains("PromQL"), "help should not mention PromQL: {help}");
}

// Silence unused-import lints when the fixture is missing — `Path`
// only matters for the `demo_parquet().exists()` predicate inside
// `skip_if_no_fixture`.
#[allow(dead_code)]
fn _unused(_: &Path) {}
