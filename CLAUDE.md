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

# Recorder - capture metrics to disk (.rez by default)
target/release/rezolus record                                                           # localhost:4241 -> rezolus.rez
target/release/rezolus record --url http://localhost:4241 -o out.rez --label arm=redis  # per-sampler .rez archive
target/release/rezolus record --endpoint http://web-01:4241 --endpoint http://web-02:4241 -o fleet.rez  # one recording per endpoint
target/release/rezolus record --url http://host:9090/metrics -o out.parquet --metadata source=llm-perf
# Auto-detects Rezolus agent vs Prometheus endpoints. The -o extension picks the format
# (.rez | .parquet | .raw); --format {rez|parquet|raw} is rarely needed and conflicting with
# the extension is an error. With no -o, the output is rezolus.<ext> for the format in play.
# .rez requires every endpoint to be rezolus/msgpack, but any number of them: several
# endpoints become one multi-recording archive. A run that chose no format at all falls
# back to rezolus.parquet (with a note) for a Prometheus source; endpoint count never
# triggers that fallback.
# Also: --metadata key=value (repeatable), --label key=value (repeatable; tags a .rez
# recording, source/host auto-populated), --interval, --duration.

# Viewer - web dashboard for parquet files, live agents, or upload mode
target/release/rezolus view output.parquet [experiment.parquet] [--listen ADDR]
target/release/rezolus view out.rez [--listen ADDR]                 # .rez archive (2-recording = A/B)
target/release/rezolus view fleet.rez --baseline host=web-01 --experiment host=web-02  # pick two arms
target/release/rezolus view http://localhost:4241 [--listen ADDR]   # live agent connection
target/release/rezolus view [--listen ADDR]                         # upload-only mode (no file)
target/release/rezolus view --tui output.parquet                    # terminal UI (parquet or live agent; not upload-only)
target/release/rezolus view --tui http://localhost:4241             # terminal UI, live agent

# Hindsight - rolling .rez buffer for incident analysis
target/release/rezolus hindsight config/hindsight.toml

# Parquet tools - file operations on parquet recordings
target/release/rezolus recording metadata -i file.parquet             # show file/column metadata
target/release/rezolus recording metadata -i file.parquet --json      # JSON output
target/release/rezolus recording metadata -i file.parquet --field source
target/release/rezolus recording annotate file.parquet                # add service extension KPIs
target/release/rezolus recording annotate file.parquet --queries ext.json
target/release/rezolus recording convert rezolus.raw.zst               # raw msgpack -> parquet (writes rezolus.parquet)
target/release/rezolus recording convert rezolus.raw -o out.parquet --interval 250ms
target/release/rezolus recording convert rezolus.raw --systeminfo sysinfo.json --descriptions help.json
# convert auto-detects zstd by magic bytes; interval is inferred from snapshot
# timestamps unless --interval is given; -m/--metadata key=value tags the file;
# --force overwrites an existing output. Raw input only (not .rez, not parquet).
target/release/rezolus recording combine a.parquet b.parquet -o combined.parquet       # row-merge multi-source
target/release/rezolus recording combine a.parquet b.parquet --ab baseline=redis experiment=valkey -o out.parquet.ab.tar  # A/B tarball (values are source names)
target/release/rezolus recording filter file.parquet -o slim.parquet   # drop columns not needed by KPIs
target/release/rezolus recording upgrade old.rez                       # v1/v2 tar .rez -> v3 SQLite (in place)
target/release/rezolus recording upgrade old.rez -o new.rez            # ...or to a new file
target/release/rezolus recording snapshot live.rez -o incident.rez     # complete copy of an archive still being written
# .rez archives: metadata describes the manifest (recordings, labels, tables + cadence; V3 group
#   tables appear as <sampler>/<group>);
# combine a.rez b.rez -o out.rez assembles single-recording .rez into a multi-recording .rez (multi-host/A/B);
#   v1/v2 tar inputs are upgraded to v3 on the way in, so containers can be mixed freely;
# filter file.rez --samplers cpu_usage,scheduler -o slim.rez drops tables whose sampler is not listed
#   (both containers; on a v3 archive a sampler's group tables are dropped together);
# annotate file.rez --queries kpis.json embeds KPIs into each recording's manifest (--queries required for .rez).

# MCP - AI analysis server or CLI commands
# Status - agent health check (exits 1 if any sampler is degraded/failed/pmu-limited)
target/release/rezolus status                       # defaults to http://localhost:4241
target/release/rezolus status web-01                # bare host/host:port is normalized
target/release/rezolus status --json

target/release/rezolus mcp                                                    # stdio server
target/release/rezolus mcp describe-recording file.parquet                    # describe recording
target/release/rezolus mcp describe-metrics file.parquet                      # list all metrics
target/release/rezolus mcp detect-anomalies file.parquet                      # exhaustive anomaly detection
target/release/rezolus mcp detect-anomalies file.parquet "cpu_usage"          # targeted anomaly detection
target/release/rezolus mcp query file.parquet "sum(rate(cpu_cycles[1m]))"     # PromQL query
# query prints an acquisition-window uncertainty band [lo, hi] beside rate()/irate() values
# (scalar ops scale the band, e.g. rate(x)*k; series-op-series combines both operands'
# bands by interval arithmetic; non-rate/non-histogram queries have no band to show)
target/release/rezolus mcp analyze-correlation file.parquet "metric1" "metric2"
target/release/rezolus mcp extract-features file.parquet             # structured feature record (JSON)
target/release/rezolus mcp describe-recording multi.rez              # lists the recordings + their selectors, not an error
target/release/rezolus mcp query multi.rez "sum(rate(cpu_cycles[1m]))" --recording source=redis  # pick one recording
# --recording key=value (repeatable, ANDed) is on all six subcommands, and the stdio server's six
#   tools take an optional recording object, e.g. {"source": "redis"}, same semantics;
# it must name exactly one recording: none or several is an error listing the candidates, never a
#   first match.
```

## Architecture

### Operating Modes

The binary operates in seven modes via subcommands:

1. **Agent** (`src/agent/`) - Default. Collects system metrics via samplers.
2. **Exporter** (`src/exporter/`) - Pulls from agent's msgpack endpoint, exposes Prometheus metrics.
3. **Recorder** (`src/recorder/`) - Writes metrics to disk. Auto-detects Rezolus vs Prometheus sources. The output extension picks the format (`.rez` default, `.parquet`, `.raw`); `--format` is the explicit form and contradicting the extension is an error. Defaults to `rezolus.rez` when no `-o` is given, falling back to `rezolus.parquet` for a Prometheus source only when nothing pinned the format (endpoint count never triggers it). Several rezolus endpoints in one run write one multi-recording `.rez`, a recording each. Supports `--metadata key=value`; `--label key=value` tags every `.rez` recording in the run, so `--endpoint url,source=name` is what distinguishes two endpoints (see "`.rez` archive format" below).
4. **Hindsight** (`src/hindsight/`) - Maintains a rolling `.rez` v3 buffer on disk (the streaming v3 writer with retention: everything older than `duration` is evicted each tick) for post-incident snapshots. The buffer is readable live by the viewer/MCP/`parquet metadata`; a snapshot is a `VACUUM INTO` copy taken without pausing the recording.
5. **Viewer** (`src/viewer/`) - Web dashboard with PromQL query engine and TSDB (from `metriken-query` crate). Supports parquet files, `.rez` archives (a 2-recording `.rez` renders as an A/B baseline/experiment comparison; >2 shows the first two unless `--baseline k=v` / `--experiment k=v` name which recordings fill the slots — repeatable, ANDed, subset match, the same selector semantics as the MCP `--recording` flag, and each must name exactly one recording or the run is refused with a listing), live agent connections, and upload-only mode. Generates service KPI dashboards from `ServiceExtension` metadata.
6. **MCP** (`src/mcp/`) - AI analysis tools (anomaly detection, correlation, PromQL queries, feature extraction). Runs as stdio server or one-shot CLI commands. `query` prints acquisition-window uncertainty bands `[lo, hi]` beside `rate()`/`irate()` values (scalar ops scale the band; series-op-series combines both operands' bands by interval arithmetic, and operands from *different* acquisition tables have their bands widened to the union of both spans first — identical edges mean the same read, so the common case widens by nothing; non-rate/non-histogram queries have no band). See `docs/journal/2026-08-21-cross-table-uncertainty.md`. `extract-features` emits a deterministic, versioned overview record (JSON) summarizing a recording's Rezolus-native features.
7. **Parquet** (`src/parquet_tools/`) - File operations: `metadata` (inspect; on a `.rez`, describes the manifest), `annotate` (add service extension KPIs; on a `.rez`, `--queries` embeds them into each recording's manifest), `combine` (merge multi-source files, build an A/B tarball, or assemble single-recording `.rez` into a multi-recording `.rez`), `filter` (drop columns not needed by KPIs; on a `.rez`, `--samplers` drops every table whose sampler is not listed), `convert` (raw msgpack recording, plain or zstd, into parquet — the offline complement to `record -o out.raw`), `snapshot` (a complete standalone copy of a `.rez` that is still being written — `cp` leaves SQLite's `-wal` sidecar behind and silently ends early). Those four accept `.rez` inputs; `convert` takes raw input only and emits parquet only.

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

The `.rez` format (`crates/rez/`: `src/rez.rs`, `src/reader.rs`) is a container holding many parquet tables plus a `manifest.json`, rather than a single parquet file. Two container shapes exist and both are readable:

- **v2 (tar)** — a tar archive with `manifest.json` and one table per sampler (`<recording>/<sampler>.parquet`).
- **v3 (SQLite)** — a SQLite container (`crates/rez/src/rez_sqlite.rs`, `crates/rez/src/rez_v3_writer.rs`) with a real WAL: rows land in the WAL and are periodically sealed into parquet segments. This is what `hindsight` maintains as its rolling buffer, and it is readable live while a writer is still appending.

Each sampler records at its own cadence. Table granularity depends on the snapshot format the agent produced:

- From a **SnapshotV2** agent, a table holds a whole sampler and each metric carries its own acquisition-window columns (`<m>:window_begin`/`<m>:window_width`).
- From a **SnapshotV3** agent, tables are per *acquisition group* and keyed `<sampler>/<group>` (see "Acquisition Groups" in `docs/principles.md`, principle 18). Because a group is by definition one read with one window, the table carries a single table-level window pair — bare `:window_begin`/`:window_width` with no metric id — applying to every metric in it. `table_sampler()` splits a table key at the first `/`, so the sampler stays the manifest/filter unit, and the reader unions a sampler's group tables (dispatch by metric name — no timestamp join, no column concatenation, no null filling).

Either way the query engine consumes the window columns to compute `rate()`/`irate()` uncertainty bounds.

Because each sampler records at its own cadence, a query spanning *two samplers of materially different cadence* (measured row spacing differing by at least 2x, with the slower coarser than the step) is evaluated at the **slow sampler's own row timestamps** rather than on the uniform `start + k·step` grid — otherwise nearly every point lands where the slow sampler never read and its value is merely held forward. `RezReader::cross_cadence_eval_timestamps` picks those timestamps and passes them as `QueryOptions::eval_timestamps`; cadence is measured from the rows (never `interval()`, which reports one nominal value for the whole archive) and per *sampler*, since a group that skips ticks is sparse within its sampler's cadence rather than a second cadence. Single-sampler queries and `RateMode::Raw` are untouched.

A `.rez` requires every endpoint to be rezolus/msgpack to produce (not Prometheus), but any number of them. The manifest carries per-recording label sets (`source`/`host` auto-populated, plus any `record --label k=v`, which applies to every recording in the run); a multi-recording `.rez` — built live by `record --endpoint a --endpoint b -o out.rez`, or offline by `parquet combine` — drives the viewer's A/B comparison, aliasing baseline/experiment from each recording's `arm`/`host` labels. The seal stagger keys on sampler + the recording's canonical label set (`seal_policy::recording_stagger_key`), so two recordings of the same agent do not seal in lockstep; identical label sets warn at startup.

### `.rez` lives in its own crate

`crates/rez` holds the whole format — container (v2 tar, v3 SQLite), writers,
and the `RezReader` that presents an archive as a `metriken_query::MetricsSource`.
It is a workspace crate rather than a module of the binary because `rezolus` is
**binary-only** (no `lib` target), so nothing can depend on it: a reader living
there was reachable by the server viewer and by nothing else, which is why the
static-site WASM viewer has never opened a `.rez`.

The binary re-exports the modules under the paths call sites already use
(`crate::recorder::rez`, `crate::rez_reader`, …), so `.rez` code reads the same
from inside `rezolus`.

Fixture builders (`rez::rez::recorder_tests_support`, and the tar writer
`rez_stream`) sit behind the crate's `test-support` feature rather than
`#[cfg(test)]` — a `#[cfg(test)]` module in `rez` is invisible to the binary
crate's tests, which build most `.rez` fixtures in this repo. The binary enables
that feature as a dev-dependency.

**The `write` feature (default on) is what separates the two halves.** With it
off, `crates/rez` is a *reader*: container, catalog, WAL-tail materialization,
`RezReader`. That configuration is the one that compiles for
`wasm32-unknown-unknown`, and the reason it has to exist is narrow — `metriken`'s
registry (`metriken-core`) declares a `linkme` distributed slice, and `linkme`
gates on a fixed `target_os` list that `unknown` is not in. So the read path
must not name a `metriken`/`metriken-exposition` type at all, which is why the
archive owns:

- `rez::window::Window` — the acquisition window a segment stores. Converted
  from `metriken::Window` once, at ingest.
- `rez::schema::{GroupSchema, MetricDesc}` — a group's membership as carried in
  a WAL row. Its serde field order is the on-disk msgpack, so it is pinned
  byte-for-byte against the producer's type by a test.
- `rez::rez::{Cell, CellValue}` — the builder's input. Rows reach a table from
  an agent snapshot *and* from this archive's own WAL, and only the first has
  snapshot values to borrow.

`cargo check -p rez --no-default-features --target wasm32-unknown-unknown` is a
CI step for exactly this reason; nothing else catches a metriken type creeping
back onto the read path.

### Service Extensions

Service-level KPI dashboards are defined in `src/viewer/service_extension.rs` (`ServiceExtension`/`Kpi` structs). They allow the viewer to generate custom dashboard sections from PromQL queries embedded in parquet metadata. The `parquet annotate` command validates and embeds these. Templates live in `src/parquet_tools/templates/`.

### Static Site Viewer (WASM)

The `site/` directory hosts a browser-only viewer deployed to GitHub Pages. It shares the `src/viewer/assets/` frontend (via symlinks) with the server-backed viewer, but loads recordings directly in the browser through a WASM module.

The WASM crate lives at `crates/viewer/`. It targets `wasm32-unknown-unknown` and builds via `./crates/viewer/build.sh`; output goes to `site/viewer/pkg/` where the frontend imports it as `../pkg/wasm_viewer.js`. It is a workspace member like any other, but it cannot depend on `rezolus` itself — that is a **binary-only** crate with no `lib` target — so anything both viewers need lives in a shared crate (`dashboard`, `rez`) instead. See the `viewer-parity` skill.

**It opens both parquet files and `.rez` archives.** A `.rez` is recognized by content, not by filename, exactly as the CLI recognizes it, and a 2-recording archive fills the A/B slots — the same mapping `rezolus view` makes. Above two, the first two are shown and the rest are named in `WasmCaptureRegistry::notices()`, which the frontend logs; the server viewer's equivalent warning goes to a terminal nobody is watching in a browser.

One thing the browser cannot have: SQLite's `-wal` **sidecar**. A `.rez` carries its own unsealed rows in a `wal` TABLE inside the file, and those travel with an upload — but SQLite also commits into a sidecar file not yet checkpointed into the archive, and an upload is one file. `rezolus view archive.rez` opens by path with the sidecar beside it and sees further. This only differs for a copy taken from under a running recorder, and the gap used to be large: measured at **123 ticks (~2 minutes at a 1s interval)** on a 2000-tick recording, with a sidecar larger than the archive. Two things bound it now — the writer folds the sidecar into the archive every `rez_v3_writer::CHECKPOINT_INTERVAL` (10s), so a plain copy is at most that stale rather than unbounded; and `rezolus recording snapshot <archive> -o out.rez` reads through the sidecar for an exact copy, without pausing the writer.

The size-based autocheckpoint (4 MiB) and the time-based one answer different questions and both are needed: bytes bound the sidecar's disk footprint, time bounds how much of the recording a copy can be missing. A quiet recording takes hours to accumulate 4 MiB, and that is exactly the case where a copy is silently useless. The viewer also says so rather than rendering short in silence: `RezReader::complete()` is false for an unfinalized archive, and `WasmCaptureRegistry::notices()` reports it.

Two constraints shape how that works, and both are runtime failures rather than compile errors if broken:

- **No `metriken`.** Its registry declares a `linkme` distributed slice, and `linkme` has no wasm32 implementation — hence `crates/rez`'s `write` feature, which the viewer turns off. CI checks `cargo check -p rez --no-default-features --target wasm32-unknown-unknown`.
- **No threads.** `std::thread::spawn` compiles for wasm32 and panics at runtime, so the reader's parallel footer probe has a sequential wasm arm.

Building the bundle needs a clang that can target wasm32 (zstd and SQLite are compiled from C). Apple's does not; Homebrew LLVM does — `CC_wasm32_unknown_unknown=/opt/homebrew/opt/llvm/bin/clang`.

`crates/viewer` is `crate-type = ["cdylib", "rlib"]` so its behavior is testable natively (`cargo test -p viewer`, a CI step); `tests/wasm_viewer_*.test.mjs` drive the built bundle from node.

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
