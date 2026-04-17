# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Rezolus is a high-resolution systems performance telemetry agent written in Rust that uses eBPF for low-overhead instrumentation on Linux. It collects detailed metrics across CPU, scheduler, block IO, network, system calls, and container-level performance.

## Build Commands

```bash
# Build (debug)
cargo build

# Build (release)
cargo build --release

# Run tests
cargo test

# Run specific test
cargo test test_name

# Run tests for a specific package
cargo test -p package_name

# Format code (runs rustfmt and clang-format on .c/.h files)
cargo xtask fmt

# Lint
cargo clippy

# Generate dashboard JSON for site viewer (from Rust definitions)
cargo xtask generate-dashboards

# Check dashboards are up to date (used in CI)
cargo xtask generate-dashboards --check

# Developer mode build (serves viewer assets from disk for hot reload)
cargo build --features developer-mode

# Build the WASM viewer for the static site (outputs to site/viewer/pkg/)
./crates/rezolus-webview/build.sh
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

# Viewer - web dashboard for parquet files, live agents, or upload mode
target/release/rezolus view output.parquet [listen_address]
target/release/rezolus view http://localhost:4241 [listen_address]  # live agent connection
target/release/rezolus view [listen_address]                        # upload-only mode (no file)

# Hindsight - rolling ring buffer for incident analysis
target/release/rezolus hindsight config/hindsight.toml

# Parquet tools - file operations on parquet recordings
target/release/rezolus parquet metadata -i file.parquet             # show file/column metadata
target/release/rezolus parquet metadata -i file.parquet --json      # JSON output
target/release/rezolus parquet metadata -i file.parquet --field source
target/release/rezolus parquet annotate file.parquet                # add service extension KPIs
target/release/rezolus parquet annotate file.parquet --queries ext.json
target/release/rezolus parquet combine a.parquet b.parquet -o combined.parquet

# MCP - AI analysis server or CLI commands
target/release/rezolus mcp                                                    # stdio server
target/release/rezolus mcp describe-recording file.parquet                    # describe recording
target/release/rezolus mcp describe-metrics file.parquet                      # list all metrics
target/release/rezolus mcp detect-anomalies file.parquet                      # exhaustive anomaly detection
target/release/rezolus mcp detect-anomalies file.parquet "cpu_usage"          # targeted anomaly detection
target/release/rezolus mcp query file.parquet "sum(rate(cpu_cycles[1m]))"     # PromQL query
target/release/rezolus mcp analyze-correlation file.parquet "metric1" "metric2"
```

## Architecture

### Operating Modes

The binary operates in seven modes via subcommands:

1. **Agent** (`src/agent/`) - Default. Collects system metrics via samplers.
2. **Exporter** (`src/exporter/`) - Pulls from agent's msgpack endpoint, exposes Prometheus metrics.
3. **Recorder** (`src/recorder/`) - Writes metrics to parquet files. Auto-detects Rezolus vs Prometheus sources. Supports `--metadata key=value` and `--format {parquet|raw}`.
4. **Hindsight** (`src/hindsight/`) - Maintains rolling ring buffer on disk for post-incident snapshots.
5. **Viewer** (`src/viewer/`) - Web dashboard with PromQL query engine and TSDB (from `metriken-query` crate). Supports parquet files, live agent connections, and upload-only mode. Generates service KPI dashboards from `ServiceExtension` metadata.
6. **MCP** (`src/mcp/`) - AI analysis tools (anomaly detection, correlation, PromQL queries). Runs as stdio server or one-shot CLI commands.
7. **Parquet** (`src/parquet_tools/`) - File operations: `metadata` (inspect), `annotate` (add service extension KPIs), `combine` (merge multi-source files).

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

Service-level KPI dashboards are defined in `src/viewer/service_extension.rs` (`ServiceExtension`/`Kpi` structs). They allow the viewer to generate custom dashboard sections from PromQL queries embedded in parquet metadata. The `parquet annotate` command validates and embeds these. Templates live in `src/parquet_tools/templates/`.

### Static Site Viewer (WASM)

The `site/` directory hosts a browser-only viewer deployed to GitHub Pages. It shares the `src/viewer/assets/` frontend (via symlinks) with the server-backed viewer, but loads parquet files directly in the browser through a WASM module.

The WASM crate lives at `crates/rezolus-webview/`. It is its own Cargo workspace — it targets `wasm32-unknown-unknown` and has profile settings that differ from the main rezolus binary. Build with `./crates/rezolus-webview/build.sh`; output goes to `site/viewer/pkg/` where the frontend imports it as `../pkg/wasm_viewer.js`.

### Key Dependencies

- `metriken` - Metrics registration and exposition
- `metriken-exposition` - Snapshot serialization and msgpack-to-parquet conversion
- `metriken-query` - TSDB, PromQL query engine (re-exported in `src/viewer/mod.rs`)
- `libbpf-rs` / `libbpf-cargo` - eBPF program management (Linux)
- `axum` - HTTP server
- `tokio` - Async runtime
- `parquet` / `arrow` - Parquet file I/O

### Configuration

TOML configs in `config/`:
- `agent.toml` - Sampler enable/disable, collection intervals
- `exporter.toml` - Scrape interval (must match Prometheus), percentile settings
- `hindsight.toml` - Buffer size, output path

## Platform Support

- **Linux**: Full support including eBPF (kernel 5.8+)
- **macOS**: Limited (CPU usage only, no eBPF)
- **Architectures**: x86_64 and ARM64

## Git Conventions

Do not append claude.ai session links to commit messages.
