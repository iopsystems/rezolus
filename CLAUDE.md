# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Rezolus is a high-resolution systems performance telemetry agent written in Rust that uses eBPF for low-overhead instrumentation on Linux. It collects detailed metrics across CPU, scheduler, block IO, network, system calls, and container-level performance.

## Build Commands

```bash
# Build (debug, default features = live-agent mode + MCP compiled in)
cargo build

# Build (release)
cargo build --release

# Build SQL-only — drops live-agent mode. MCP is included (post-May-2026
# migration onto DuckDbBackend). Legacy metriken-query (Tsdb + PromQL) is
# not in the dep tree.
# Verify with `cargo tree -p rezolus --no-default-features --features sql-only`.
cargo build --bin rezolus --no-default-features --features sql-only

# Run tests
cargo test

# Run specific test
cargo test test_name

# Run tests for a specific package
cargo test -p package_name

# Run pure-JS viewer tests (compare-math, selection-migration) — no
# bundler, no jsdom, just node's built-in test runner
node --test tests/*.mjs

# End-to-end viewer smoke (upload / file / A-B / proxy modes). Requires `jq`.
bash tests/viewer_smoke.sh

# Headless-Chromium per-section render check. Walks every section in
# /api/v1/sections, asserts each renders a chart, an `_unavailable`
# placeholder, or a `.section-notes` no-data callout. Catches the
# silent-render regression the API-only smoke can't see.
# Requires chromium, jq, python3, `pip install --user websockets`.
bash scripts/viewer_chromium_smoke.sh <parquet>
bash scripts/viewer_chromium_smoke.sh site/viewer/data/cachecannon.parquet

# Format code (runs rustfmt and clang-format on .c/.h files)
cargo xtask fmt

# Lint
cargo clippy

# Dump dashboard JSON for inspection/debugging
cargo run -p dashboard                  # print to stdout
cargo run -p dashboard -- output_dir/   # write files to directory

# Developer mode build (serves viewer assets from disk for hot reload)
cargo build --features developer-mode

# Build the WASM viewer for the static site (outputs to site/viewer-sql/pkg/).
# The static viewer at site/viewer/ imports it via a relative path.
./crates/viewer-sql/build.sh
```

## Running Modes

```bash
# Agent (default) - requires sudo for eBPF on Linux
sudo target/release/rezolus config/agent.toml

# Exporter - Prometheus-compatible metrics endpoint
sudo target/release/rezolus exporter config/exporter.toml

# Recorder - capture metrics to parquet
target/release/rezolus record http://localhost:4241 output.parquet
target/release/rezolus record --metadata source=llm-perf http://host:9090/metrics output.parquet
# Auto-detects Rezolus agent vs Prometheus endpoints. Supports --format {parquet|raw},
# --metadata key=value (repeatable), --interval, --duration.

# Viewer - web dashboard for parquet files, live agents, or upload mode.
# File / upload / A-B paths run SQL through metriken-query-sql::DuckDbBackend.
# Live-agent path runs PromQL through metriken-query::Tsdb (the `live-mode`
# carve-out, default-on); --no-default-features --features sql-only drops it.
target/release/rezolus view output.parquet [experiment.parquet] [--listen ADDR]
target/release/rezolus view http://localhost:4241 [--listen ADDR]   # live agent connection (live-mode feature)
target/release/rezolus view [--listen ADDR]                         # upload-only mode (no file)

# Hindsight - rolling ring buffer for incident analysis
target/release/rezolus hindsight config/hindsight.toml

# Parquet tools - file operations on parquet recordings
target/release/rezolus parquet metadata -i file.parquet             # show file/column metadata
target/release/rezolus parquet metadata -i file.parquet --json      # JSON output
target/release/rezolus parquet metadata -i file.parquet --field source
target/release/rezolus parquet annotate file.parquet                # add service extension KPIs
target/release/rezolus parquet annotate file.parquet --queries ext.json
target/release/rezolus parquet combine a.parquet b.parquet -o combined.parquet

# MCP - AI analysis server or CLI commands. Runs against parquet files via
# metriken_query_sql::DuckDbBackend (post-May-2026 migration). Built in
# both default and sql-only configurations. `query` takes DuckDB SQL;
# `detect-anomalies` and `analyze-correlation` accept either a bare metric
# name (auto-resolved to canonical rate/sum/quantile SQL) or full SQL.
target/release/rezolus mcp                                                    # stdio server
target/release/rezolus mcp describe-recording file.parquet                    # describe recording
target/release/rezolus mcp describe-metrics file.parquet                      # list all metrics
target/release/rezolus mcp detect-anomalies file.parquet                      # exhaustive anomaly detection
target/release/rezolus mcp detect-anomalies file.parquet cpu_usage            # targeted anomaly detection (bare metric name)
target/release/rezolus mcp query file.parquet "SELECT count(*) FROM _src"     # DuckDB SQL
target/release/rezolus mcp analyze-correlation file.parquet cpu_cycles cpu_instructions
```

## Architecture

### Operating Modes

The binary operates in seven modes via subcommands:

1. **Agent** (`src/agent/`) - Default. Collects system metrics via samplers.
2. **Exporter** (`src/exporter/`) - Pulls from agent's msgpack endpoint, exposes Prometheus metrics.
3. **Recorder** (`src/recorder/`) - Writes metrics to parquet files. Auto-detects Rezolus vs Prometheus sources. Supports `--metadata key=value` and `--format {parquet|raw}`.
4. **Hindsight** (`src/hindsight/`) - Maintains rolling ring buffer on disk for post-incident snapshots.
5. **Viewer** (`src/viewer/`) - Web dashboard.
   - **File / upload / A-B** paths run SQL through `metriken_query_sql::DuckDbBackend`
     via `src/viewer/sql_capture.rs::SqlCapture` (parquet path + cached `MetricCatalog`).
     `Arc<DuckDbBackend>` lives on `AppState`. The `/api/v1/query{,_range}` handlers
     accept raw SQL and project Arrow → Prometheus matrix JSON via the
     `prom-matrix` crate.
   - **Live agent** path ingests into `metriken-query::Tsdb` (the legacy
     in-memory store), but `/api/v1/query{,_range}` currently return
     `capture_not_found` for live mode — query path is a carve-out
     pending stages 3-9. Gated by the `live-mode` Cargo feature
     (default-on); `--no-default-features --features sql-only` drops
     it and removes `metriken-query` from the dep tree. The only
     PromQL execution still happening in live mode is
     `validate_service_extensions` (KPI availability check on load).
   - Service-extension KPI sections still ship PromQL-only templates pending
     transcription — see review/review.md for the deferred work list.
6. **MCP** (`src/mcp/`) - AI analysis tools (anomaly detection, correlation,
   SQL queries). Runs against parquet files via
   `metriken_query_sql::DuckDbBackend` after the May-2026 migration. The
   helper module `src/mcp/backend.rs` owns parquet opening
   (`open_capture`), Arrow → series projection (`batches_to_series`), and
   SQL builders for the three metric kinds (`counter_sum_rate_sql`,
   `gauge_sum_sql`, `histogram_quantile_sql`). Legacy Tsdb/PromQL entry
   points within the module are cfg-gated to `live-mode` until they're
   audited and deleted.
7. **Parquet** (`src/parquet_tools/`) - File operations: `metadata`
   (inspect), `annotate` (validates KPIs by running their SQL through
   `DuckDbBackend`; KPIs without `sql` are marked unavailable),
   `combine` (merge multi-source files), `events` (annotate one-off
   events), `filter` (column-trim by selection). `combine` is the only
   submodule unconditional; `annotate` and `filter` are gated to
   `live-mode` because `filter` consumes `annotate::extract_metric_selectors`.

### Sampler Architecture

Samplers live in `src/agent/samplers/{category}/`. Each sampler:
- Has platform-specific implementations (`linux/`, `macos/`)
- Registers via `linkme` distributed slice (`SAMPLERS` in `src/agent/samplers/mod.rs`)
- Implements the `Sampler` trait with `name()` and `refresh()` methods

Samplers with eBPF programs (Linux only) have a `mod.bpf.c` file alongside the Rust module. The BPF programs are compiled at build time by `build.rs`.

BPF-enabled samplers: `blockio/{latency,requests}`, `cpu/{bandwidth,migrations,perf,tlb_flush,usage}`, `network/{interfaces,traffic}`, `scheduler/runqueue`, `syscall/{counts,latency}`, `tcp/{connect_latency,packet_latency,receive,retransmit,traffic}`.

### eBPF Build System

`build.rs` compiles BPF programs using libbpf-cargo:
- Architecture-specific vmlinux.h headers in `src/agent/bpf/{x86_64,aarch64}/`
- Output skeletons go to `$OUT_DIR/{sampler}_{program}.bpf.rs`
- Requires clang for BPF compilation

### Parquet File Format

Parquet files produced by the recorder/hindsight use a columnar layout from `metriken-exposition`:
- **`timestamp`** (UInt64) - Nanoseconds since Unix epoch. Present in every file.
- **`duration`** (UInt64, nullable) - Snapshot collection duration in nanoseconds.
- **Metric columns** - One per metric: counters (UInt64), gauges (Int64), histograms (List&lt;UInt64&gt;).
- **Column metadata** - Each field carries `metric_type` ("counter"/"gauge"/"histogram"/"timestamp"/"duration") and metric labels.

File-level metadata keys are defined in `src/parquet_metadata.rs`:
- `source` - Recording source: `"rezolus"` (single) or `["rezolus","llm-perf"]` (combined).
- `sampling_interval_ms` - Collection interval in milliseconds.
- `systeminfo` - JSON hardware summary from agent.
- `descriptions` - JSON map of metric name to help text.
- `per_source_metadata` - Per-source map with `version`, `role` ("service"/"loadgen"), and `service_queries` (ServiceExtension KPI definitions).

### Service Extensions

Service-level KPI dashboards are defined in
`crates/dashboard/src/service_extension.rs` (`ServiceExtension`/`Kpi`
structs). Each `Kpi` carries both `query` (PromQL, legacy) and an
optional `sql` field (`fd285bb`). The dashboard's
`service.rs::generate` calls `plot_promql_with_sql{,_full}` when
`sql` is present and `plot_promql{,_full}` otherwise. Templates live
in `config/templates/{cachecannon,vllm,vllm-prefill,vllm-decode,sglang,sglang-decode,sglang-prefill,sglang-router,llm-perf,valkey,inference-library}.json`
and currently ship PromQL-only — transcription to SQL is the next
known piece of work (see review/review.md). `parquet annotate` validates
each KPI's SQL by running it through `DuckDbBackend`; KPIs without
SQL are marked `available: false` with a warn-level log.

### Static Site Viewer (WASM)

The `site/` directory hosts a browser-only viewer deployed to GitHub
Pages. It shares the `src/viewer/assets/` frontend (via symlinks)
with the server-backed viewer, but loads parquet files directly in
the browser through a WASM module.

The WASM crate lives at `crates/viewer-sql/`. It targets
`wasm32-unknown-unknown` and runs queries through duckdb-wasm against
the loaded parquet (no in-process PromQL engine). Build with
`./crates/viewer-sql/build.sh`; output goes to `site/viewer-sql/pkg/`
where the frontend imports it as
`../viewer-sql/pkg/wasm_viewer_sql.js`. The static viewer at
`site/viewer/` boots a `CaptureRegistry` from
`site/viewer-sql/lib/duckdb-registry.js` (a JS-side multi-worker pool
that mirrors the legacy `WasmCaptureRegistry` surface). The pre-2026
PromQL/metriken-query WASM crate at `crates/viewer/` was retired once
every dashboard plot emitted SQL — see
`git log --oneline -- crates/viewer` for migration history.

### Arrow → Prometheus matrix projection

`crates/prom-matrix/` owns the projection from Arrow `RecordBatch`es
(server-side, native) or a JS Arrow `Table` (WASM) to the Prometheus
matrix JSON shape the frontend renders. The two entry points
(`arrow_to_prom_matrix` and `js_arrow_to_prom_matrix`) share a
`pub(crate) emit_prom_matrix_json` envelope formatter so the JSON
shape can't drift between server and browser. Both consumers expect
SQL emitters to project `t` (DOUBLE seconds), `v` (numeric), and
zero or more label columns.

### Key Dependencies

- `metriken` - Metric registration core (unconditional)
- `metriken-exposition` - Snapshot serialization and msgpack-to-parquet conversion (unconditional)
- `metriken-query-sql` - DuckDB-backed SQL engine: `DuckDbBackend::run_sql`, `describe_parquet`, `MetricCatalog`. The query engine for the server viewer **and** the MCP CLI/server.
- `metriken-query` - Legacy in-memory `Tsdb` + PromQL engine. **Optional dep behind `live-mode` feature** — pulled in only when the live-agent ingest path is compiled in. MCP no longer requires it post-May-2026.
- `libbpf-rs` / `libbpf-cargo` - eBPF program management (Linux)
- `axum` - HTTP server
- `tokio` - Async runtime
- `parquet` / `arrow` - Parquet file I/O

### Configuration

TOML configs in `config/`:
- `agent.toml` - Sampler enable/disable, collection intervals
- `exporter.toml` - Scrape interval (must match Prometheus), percentile settings
- `hindsight.toml` - Buffer size, output path

## BPF Sampler Principles

When working on code under `src/agent/samplers/` or `src/agent/bpf/`, read `docs/principles.md` first. It captures the design rules Rezolus commits to for BPF samplers (always-on fleetwide production, in-kernel aggregation read via mmap, H2 histograms, etc.), the operational checklist for reviewing or writing a sampler, and the current improvement backlog. Any change to BPF code should be consistent with that document. If a change appears to conflict with a principle, raise it explicitly with reasoning rather than working around it.

## Platform Support

- **Linux**: Full support including eBPF (kernel 5.8+)
- **macOS**: Limited (CPU usage only, no eBPF)
- **Architectures**: x86_64 and ARM64

## Git Conventions

Do not append claude.ai session links to commit messages.
