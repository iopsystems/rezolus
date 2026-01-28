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

# Developer mode build (serves viewer assets from disk for hot reload)
cargo build --features developer-mode
```

## Running Modes

```bash
# Agent (default) - requires sudo for eBPF on Linux
sudo target/release/rezolus config/agent.toml

# Exporter - Prometheus-compatible metrics endpoint
sudo target/release/rezolus exporter config/exporter.toml

# Recorder - capture metrics to parquet
target/release/rezolus record http://localhost:4241 output.parquet

# Viewer - web dashboard for parquet files
target/release/rezolus view output.parquet [listen_address]

# Hindsight - rolling ring buffer for incident analysis
target/release/rezolus hindsight config/hindsight.toml

# MCP - AI analysis server or CLI commands
target/release/rezolus mcp                                    # stdio server
target/release/rezolus mcp describe-recording file.parquet    # describe recording
target/release/rezolus mcp detect-anomalies file.parquet      # exhaustive anomaly detection
target/release/rezolus mcp query file.parquet "sum(rate(cpu_cycles[1m]))"
```

## Architecture

### Operating Modes

The binary operates in six modes via subcommands:

1. **Agent** (`src/agent/`) - Default. Collects system metrics via samplers.
2. **Exporter** (`src/exporter/`) - Pulls from agent's msgpack endpoint, exposes Prometheus metrics.
3. **Recorder** (`src/recorder/`) - Writes metrics to parquet files.
4. **Hindsight** (`src/hindsight/`) - Maintains rolling ring buffer on disk for post-incident snapshots.
5. **Viewer** (`src/viewer/`) - Web dashboard with PromQL query engine (`promql/`) and TSDB (`tsdb/`).
6. **MCP** (`src/mcp/`) - AI analysis tools (anomaly detection, correlation, PromQL queries).

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

### Key Dependencies

- `metriken` - Metrics registration and exposition
- `libbpf-rs` / `libbpf-cargo` - eBPF program management (Linux)
- `axum` - HTTP server
- `tokio` - Async runtime
- `parquet` / `arrow` - Parquet file I/O
- `promql-parser` - PromQL parsing for viewer

### Configuration

TOML configs in `config/`:
- `agent.toml` - Sampler enable/disable, collection intervals
- `exporter.toml` - Scrape interval (must match Prometheus), percentile settings
- `hindsight.toml` - Buffer size, output path

## Platform Support

- **Linux**: Full support including eBPF (kernel 5.8+)
- **macOS**: Limited (CPU usage only, no eBPF)
- **Architectures**: x86_64 and ARM64
