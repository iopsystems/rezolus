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

# Run pure-JS viewer tests (compare-math, selection-migration) — no
# bundler, no jsdom, just node's built-in test runner
node --test tests/*.mjs

# Format code (runs rustfmt and clang-format on .c/.h files)
cargo xtask fmt

# Lint
cargo clippy

# Dump dashboard JSON for inspection/debugging
cargo run -p dashboard                  # print to stdout
cargo run -p dashboard -- output_dir/   # write files to directory

# Developer mode build (serves viewer assets from disk for hot reload)
cargo build --features developer-mode

# Build the WASM viewer for the static site (outputs to site/viewer/pkg/)
./crates/viewer/build.sh
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
target/release/rezolus record --url http://localhost:4241 -o out.rez --label arm=redis  # per-sampler .rez archive
# Auto-detects Rezolus agent vs Prometheus endpoints. Supports --format {parquet|raw|rez},
# --metadata key=value (repeatable), --label key=value (repeatable; tags a .rez recording,
# source/host auto-populated), --interval, --duration. .rez output requires a rezolus/msgpack endpoint.

# Viewer - web dashboard for parquet files, live agents, or upload mode
target/release/rezolus view output.parquet [experiment.parquet] [--listen ADDR]
target/release/rezolus view out.rez [--listen ADDR]                 # .rez archive (2-recording = A/B)
target/release/rezolus view http://localhost:4241 [--listen ADDR]   # live agent connection
target/release/rezolus view [--listen ADDR]                         # upload-only mode (no file)

# Hindsight - rolling ring buffer for incident analysis
target/release/rezolus hindsight config/hindsight.toml

# Parquet tools - file operations on parquet recordings
target/release/rezolus parquet metadata -i file.parquet             # show file/column metadata
target/release/rezolus parquet metadata -i file.parquet --json      # JSON output
target/release/rezolus parquet metadata -i file.parquet --field source
target/release/rezolus parquet annotate file.parquet                # add service extension KPIs
target/release/rezolus parquet annotate file.parquet --queries ext.json
target/release/rezolus parquet combine a.parquet b.parquet -o combined.parquet       # row-merge multi-source
target/release/rezolus parquet combine a.parquet b.parquet --ab baseline=redis experiment=valkey -o out.parquet.ab.tar  # A/B tarball (values are source names)
target/release/rezolus parquet filter file.parquet -o slim.parquet   # drop columns not needed by KPIs
# .rez archives: metadata describes the manifest (recordings, labels, per-sampler tables + cadence);
# combine a.rez b.rez -o out.rez assembles single-recording .rez into a multi-recording .rez (multi-host/A/B);
# filter file.rez --samplers cpu_usage,scheduler -o slim.rez drops whole per-sampler tables not listed;
# annotate file.rez --queries kpis.json embeds KPIs into each recording's manifest (--queries required for .rez).

# MCP - AI analysis server or CLI commands
target/release/rezolus mcp                                                    # stdio server
target/release/rezolus mcp describe-recording file.parquet                    # describe recording
target/release/rezolus mcp describe-metrics file.parquet                      # list all metrics
target/release/rezolus mcp detect-anomalies file.parquet                      # exhaustive anomaly detection
target/release/rezolus mcp detect-anomalies file.parquet "cpu_usage"          # targeted anomaly detection
target/release/rezolus mcp query file.parquet "sum(rate(cpu_cycles[1m]))"     # PromQL query
# query prints an acquisition-window uncertainty band [lo, hi] beside rate()/irate() values
# (scalar ops scale the band, e.g. rate(x)*k; series-op-series and non-rate queries show none)
target/release/rezolus mcp analyze-correlation file.parquet "metric1" "metric2"
```

## Architecture

### Operating Modes

The binary operates in seven modes via subcommands:

1. **Agent** (`src/agent/`) - Default. Collects system metrics via samplers.
2. **Exporter** (`src/exporter/`) - Pulls from agent's msgpack endpoint, exposes Prometheus metrics.
3. **Recorder** (`src/recorder/`) - Writes metrics to parquet files. Auto-detects Rezolus vs Prometheus sources. Supports `--metadata key=value` and `--format {parquet|raw|rez}`. The `.rez` format (`-o out.rez` or `--format rez`) writes a per-sampler archive (see "`.rez` archive format" below); `--label key=value` tags a `.rez` recording.
4. **Hindsight** (`src/hindsight/`) - Maintains rolling ring buffer on disk for post-incident snapshots.
5. **Viewer** (`src/viewer/`) - Web dashboard with PromQL query engine and TSDB (from `metriken-query` crate). Supports parquet files, `.rez` archives (a 2-recording `.rez` renders as an A/B baseline/experiment comparison, >2 shows the first two), live agent connections, and upload-only mode. Generates service KPI dashboards from `ServiceExtension` metadata.
6. **MCP** (`src/mcp/`) - AI analysis tools (anomaly detection, correlation, PromQL queries). Runs as stdio server or one-shot CLI commands. `query` prints acquisition-window uncertainty bands `[lo, hi]` beside `rate()`/`irate()` values (scalar ops scale the band; series-op-series and non-rate queries show none).
7. **Parquet** (`src/parquet_tools/`) - File operations: `metadata` (inspect; on a `.rez`, describes the manifest), `annotate` (add service extension KPIs; on a `.rez`, `--queries` embeds them into each recording's manifest), `combine` (merge multi-source files, build an A/B tarball, or assemble single-recording `.rez` into a multi-recording `.rez`), `filter` (drop columns not needed by KPIs; on a `.rez`, `--samplers` drops whole per-sampler tables). All four accept `.rez` inputs.

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
- `descriptions` - JSON map of metric name to help text. Present in single-source files; combined files nest this under `per_source_metadata.<source>.descriptions` instead.
- `per_source_metadata` - Per-source map with `version`, `role` ("service"/"loadgen"), `service_queries` (ServiceExtension KPI definitions), and `descriptions` (metric name → help text, combined files only).

### `.rez` Archive Format

The `.rez` format (`src/recorder/rez.rs`, `src/rez_reader.rs`) is a tar archive rather than a single parquet file. It holds a top-level `manifest.json` plus one parquet table per sampler under each recording's directory (`<dir>/<sampler>.parquet`). Each sampler records at its own cadence, and every metric carries per-observation acquisition-window columns (`<m>:window_begin`/`<m>:window_width`) that the query engine consumes to compute `rate()`/`irate()` uncertainty bounds. A `.rez` is always `source=rezolus` and requires a rezolus/msgpack endpoint to produce (not Prometheus). The manifest carries per-recording label sets (`source`/`host` auto-populated, plus any `record --label k=v`); a multi-recording `.rez` (built by `record` and `parquet combine`) drives the viewer's A/B comparison, aliasing baseline/experiment from each recording's `arm`/`host` labels.

### Service Extensions

Service-level KPI dashboards are defined in `src/viewer/service_extension.rs` (`ServiceExtension`/`Kpi` structs). They allow the viewer to generate custom dashboard sections from PromQL queries embedded in parquet metadata. The `parquet annotate` command validates and embeds these. Templates live in `src/parquet_tools/templates/`.

### Static Site Viewer (WASM)

The `site/` directory hosts a browser-only viewer deployed to GitHub Pages. It shares the `src/viewer/assets/` frontend (via symlinks) with the server-backed viewer, but loads parquet files directly in the browser through a WASM module.

The WASM crate lives at `crates/viewer/`. It is its own Cargo workspace — it targets `wasm32-unknown-unknown` and has profile settings that differ from the main rezolus binary. Build with `./crates/viewer/build.sh`; output goes to `site/viewer/pkg/` where the frontend imports it as `../pkg/wasm_viewer.js`.

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

## BPF Sampler Principles

When working on code under `src/agent/samplers/` or `src/agent/bpf/`, read `docs/principles.md` first. It captures the design rules Rezolus commits to for BPF samplers (always-on fleetwide production, in-kernel aggregation read via mmap, H2 histograms, etc.), the operational checklist for reviewing or writing a sampler, and the current improvement backlog. Any change to BPF code should be consistent with that document. If a change appears to conflict with a principle, raise it explicitly with reasoning rather than working around it.

## Platform Support

- **Linux**: Full support including eBPF (kernel 5.8+)
- **macOS**: Limited (CPU usage only, no eBPF)
- **Architectures**: x86_64 and ARM64

## Git Conventions

Do not append claude.ai session links to commit messages.
