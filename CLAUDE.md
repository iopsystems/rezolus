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

# Recorder - capture metrics to a .dendro archive (the only format 6.x writes)
target/release/rezolus record                                                           # localhost:4241 -> rezolus.dendro
target/release/rezolus record --url http://localhost:4241 -o out.dendro --label run=42  # per-sampler archive
target/release/rezolus record --endpoint http://a:4241,arm=baseline --endpoint http://b:4241,arm=experiment -o ab.dendro  # a concurrent A/B: arm= labels that endpoint's recording
target/release/rezolus record --endpoint http://web-01:4241 --endpoint http://web-02:4241 -o fleet.dendro  # one recording per endpoint
target/release/rezolus record --url http://host:9090/metrics -o out.dendro --metadata source=llm-perf
target/release/rezolus record --endpoint http://agent:4241 --endpoint http://svc:9090/metrics,source=svc -o run.dendro  # agent streamed, Prometheus scraped, one archive
target/release/rezolus record --endpoint http://gpu:8000/metrics,source=vllm,template=vllm.json -o run.dendro  # KPI template into that recording's service_queries
#   template= (TOML: template = "FILE") is read and checked in RecordingConfig::from_args (EndpointConfig::load_template ->
#   parquet_tools::annotate::load_queries: ServiceExtension + check_unit_systems), refused with exit 2 before recording;
#   endpoint_spec puts it in the spec's metadata, which outranks a streamed producer's handshake service_queries.
#   A producer can send its own template in the handshake (cachecannon and llm-perf do from their next releases);
#   crates/dashboard/templates is the fallback for recordings without one and the source of template= files for
#   vLLM/SGLang/Valkey. A config file's relative template = is read from the config file's directory; load_queries
#   refuses a file over 1 MiB (MAX_DASHBOARD_BYTES), for annotate --queries too.
target/release/rezolus record -o bench.dendro -- ./bench.sh   # record for the command's lifetime; writes run_start/run_end events
#   (program name only; --record-command-line adds the full argument list)
# Detection by stream (metriken_recorder::detect, the generic half; src/recorder/agent_probe.rs,
# rezolus's Probe hook): a URL whose path is / or /metrics/stream is asked for its stream first;
# a handshake means streamed. Any other answer goes to the hook's not_streamable(url, At, StreamAnswer)
# -> Verdict: it identifies a Rezolus agent by /status, else the / banner; an agent is Pending on
# a 5xx/408/429/timeout/no connection and refused otherwise (`unstreamable_agent`); neither route
# answering is Pending; anything else that answered is scraped as Prometheus at /metrics once
# /metrics answers 2xx (Pending until then). Late detection and enrich run on spawned tasks. Any
# other path is scraped at its URL. protocol=stream (also rezolus, msgpack) streams only,
# protocol=prometheus scrapes only. /metrics/binary is not read. Identity (metriken-recorder's):
# labels user --label > hook (`enrich`: source=rezolus and host from /systeminfo for an agent) >
# handshake labels; source = user source= > handshake source label > hook's > inferred from URL;
# metadata endpoint role=/template= > user --metadata > recorder keys (sampling_interval_ms, producer_epoch) > hook
# (version, systeminfo, descriptions) > handshake metadata; --metadata producer_epoch(s)= is
# refused (config.rs), and a pinned key survives a restart and a scrape's HELP text. The startup enrich runs concurrently
# per streamed endpoint, bounded by the scrape timeout (warning on fallback); a late activation's
# enrich runs on its own task (EndpointStatus::Activating) and the recording is added on the
# first tick after it returns. Restarts take the hook's enrich over the new handshake's metadata,
# and the hook's merge (descriptions: union). The output must end in .dendro (default
# rezolus.dendro); -o ending in .rez/.parquet/.raw or anything else, --format other than dendro
# (a hidden flag kept so `--format dendro` still parses) and --separate (hidden) are refused in
# src/recorder/config.rs (`check_output`, `check_format`, `refuse_separate`) with exit 2,
# naming a 5.x recorder (`RELEASE_5X` "or later", src/recorder/mod.rs) and quoting the path as given
# (-o, positional OUTPUT, or the config file's output). The same for a --config
# file's output/format/separate. Any number of endpoints, rezolus or Prometheus, each its own
# recording in one archive. A Prometheus scrape becomes one acquisition group per target
# (`prometheus/scrape`), windowed by the real HTTP round trip.
# Also: --metadata key=value (repeatable, every recording), --label key=value (repeatable;
# source/host auto-populated), --interval, --duration; an endpoint's role= is its recording's
# `role` metadata key.
# Every streamed endpoint is recorded from /metrics/stream and every Prometheus endpoint scraped
# (src/recorder/mod.rs `run`, over metriken-recorder's `detect` and `Recorder`); both feed one
# ArchiveWriter, one commit per tick. A streamed endpoint is never switched to a scrape. There is no scrape path
# for an agent: one that cannot serve the stream (older than 5.21.0, V2 agent, 404/409/foreign content type, handshake that does not decode) is refused
# with its version, pointing at a 5.x recorder (`unstreamable_agent`). At startup that is
# before the archive exists (exit 1); an agent that comes up later, or is refused on a
# reconnect, is marked Refused and left out from then on (a reconnect keeps the rows it had),
# the rest finalize, and the run exits 1 (`Event::Refused`, then `refusal_summary`). An unreachable one (or a 5xx/408/429 /
# handshake timeout) is retried each tick, and a stream that drops (or goes silent for the
# interval the agent serves plus the scrape timeout) is reconnected after one interval, at least
# a second. The stream interval is negotiated: an --interval below the agent's TTL is served,
# and recorded, at the TTL. A wrapped command that
# exits on its own waits for each agent's frame stamped at or after the exit (bounded by
# interval + scrape timeout). When every endpoint has been refused the run ends; each refused
# endpoint gets a line.
# Groups of slots arrive in the long layout (/metrics/stream?layout=long: values keyed by
# occupant, each occupant's labels once on <group>/occupants; metriken_storage::StreamDecoder
# and SourceRecorder::stage_streamed); other groups carry their schema when it changes. An
# agent that serves only the wide layout is recorded through the same decoder. The agent sends
# no index frames.
# The agent serves the stream through metriken-exposition's StreamSubscription, as
# application/vnd.metriken.replication.v1+dendro with x-metriken-frame-interval,
# x-metriken-update-floor and x-metriken-layout (WIRE_NAMES). Recorder, hindsight and the
# live viewer also accept the vnd.rezolus / x-rezolus-* names that agents 5.21.0 through
# 6.0.0-alpha.44 serve (src/recorder/stream.rs ACCEPTED_WIRE_NAMES); 5.x `record --stream`
# and 6.0-alpha consumers through alpha.44 refuse a current agent's stream by its content type. --stream was
# removed before 6.0.0 (clap's unknown-argument error).

# Viewer - web dashboard for parquet files, live agents, or upload mode
target/release/rezolus view output.parquet [experiment.parquet] [--listen ADDR]
target/release/rezolus view out.rez [--listen ADDR]                 # archive: one run's recordings = one combined view
target/release/rezolus view fleet.rez --baseline host=web-01 --experiment host=web-02  # pick two arms
target/release/rezolus view http://localhost:4241 [--listen ADDR]   # live agent connection
target/release/rezolus view [--listen ADDR]                         # upload-only mode (no file)
target/release/rezolus view --tui output.parquet                    # terminal UI (parquet or live agent; not upload-only)
target/release/rezolus view --tui http://localhost:4241             # terminal UI, live agent
target/release/rezolus view out.dendro --registry-templates          # built-in KPI templates over the recording's own
#   A source's KPI template is its metadata's `service_queries` (parquet footer, .rez recording, dendro source),
#   ahead of the built-in registry, which is consulted only for a source without one; --registry-templates
#   inverts that for sources the registry knows. Shared lookup: crates/dashboard/src/source_templates.rs
#   (both shells; WASM: WasmCaptureRegistry::set_prefer_registry_templates, no site UI). A combined parquet's array
#   `source` keys a top-level template by its service_name. `dashboard` is the section template for the built-in
#   sections: a Rezolus agent sends it in its stream handshake (src/agent/exposition/http/mod.rs `handshake_metadata`,
#   revision `rezolus <version>`), dendro sources keep it, and both viewers expand it (<= 1 MiB and it parses) or the
#   embedded one (crates/dashboard/src/rezolus_template.rs); --registry-templates picks the embedded one here too.

# Hindsight - rolling .dendro buffer for incident analysis (an output not ending in .dendro is refused)
target/release/rezolus hindsight config/hindsight.toml

# Parquet tools - file operations on parquet recordings
target/release/rezolus recording metadata -i file.parquet             # show file/column metadata
target/release/rezolus recording metadata -i file.parquet --json      # JSON output
target/release/rezolus recording metadata -i file.parquet --field source
target/release/rezolus recording annotate file.parquet                # add service extension KPIs
target/release/rezolus recording annotate file.parquet --queries ext.json
target/release/rezolus recording annotate out.rez --event 'time=2026-05-12T15:23Z,kind=deploy,description=rollout'  # .rez: embed a timeline event into each recording
target/release/rezolus recording check out.rez                        # evaluate the KPI checks the recording carries; exit 1 on FAIL, 2 on ERROR
target/release/rezolus recording check file.parquet --queries kpis.json --json   # checks from a file instead, verdicts as JSON
target/release/rezolus recording check fleet.rez --recording host=web-01 --annotate  # one recording; write FAIL/WARN windows as kind=check events
# A check is `"check": {"above"|"below": <n>, "for": "10s", "severity": "fail"|"warn", "quantile": 0.99}` on a KPI
# (crates/dashboard/src/service_extension.rs). Values with an acquisition band are compared by the band; a
# straddle is INDETERMINATE, which never fails. See docs/journal/2026-09-28-checks-with-verdicts.md.
target/release/rezolus recording convert rezolus.raw.zst               # raw msgpack -> parquet (writes rezolus.parquet)
target/release/rezolus recording convert rezolus.raw -o out.parquet --interval 250ms
target/release/rezolus recording convert rezolus.raw --systeminfo sysinfo.json --descriptions help.json
# convert auto-detects zstd by magic bytes; interval is inferred from snapshot
# timestamps unless --interval is given; -m/--metadata key=value tags the file;
# --force overwrites an existing output. Raw input only (not .rez, not parquet).
target/release/rezolus recording combine a.parquet b.parquet -o combined.parquet       # row-merge multi-source
target/release/rezolus recording combine baseline.rez experiment.rez --ab baseline=baseline.rez experiment=experiment.rez -o ab.rez  # A/B compare (PREFERRED): stamps arm on each input
target/release/rezolus recording combine a.parquet b.parquet --ab baseline=redis experiment=valkey -o out.parquet.ab.tar  # A/B tarball (legacy; parquet-only inputs)
target/release/rezolus recording filter file.parquet -o slim.parquet   # drop columns not needed by KPIs
target/release/rezolus recording upgrade old.rez                       # v1/v2 tar .rez -> v3 SQLite (in place)
target/release/rezolus recording upgrade old.rez -o new.rez            # ...or to a new file
target/release/rezolus recording upgrade --to dendro in.rez -o out.dendro  # any .rez -> dendro archive (new file only;
#   `view` and `mcp` read it through metriken-storage's src/rez/catalog.rs, long tables and their `<table>/occupants`
#   streams included (format and relabel: metriken-storage); `recording` metadata, annotate,
#   check, snapshot, filter and combine (into a .dendro) read it too; RezDb::open refuses one by name). Segment/WAL/caller-row bytes are
#   copied verbatim, only live WAL rows, u64 timestamps checked into i64. metriken-storage's src/rez/to_dendro.rs.
target/release/rezolus recording snapshot live.rez -o incident.rez     # complete copy of an archive still being written
# .rez archives: metadata describes the manifest (recordings, labels, tables + cadence; V3 group
#   tables appear as <sampler>/<group>);
# combine a.rez b.rez -o out.rez assembles single-recording .rez into a multi-recording .rez (opens combined);
#   --ab baseline=a.rez experiment=b.rez stamps arm on each input's recordings (opens A/B; a conflicting
#   existing arm is refused before copying, naming the input; .rez/.dendro outputs and parquet inputs to .rez);
#   one archive input: --ab baseline=0 experiment=1 labels its recordings by position (an old archive's twins);
#   the viewer fills the slots by arm value (dashboard::combined::arm_slots), not archive order: one recording
#   per arm; every other recording is shown beside (dashboard::dashboard::add_beside_sections: its own sections at
#   <route>@<key>, single capture, queries carrying __capture__="<key>" that combined::Routed, the baseline's
#   source, answers from it); combine refuses an --ab result arm_slots would refuse;
#   v1/v2 tar inputs are upgraded to v3 on the way in, so containers can be mixed freely;
#   combine a.parquet b.parquet -o out.rez ingests parquet inputs too (one recording each, split by
#   sampler); a parquet has no acquisition windows, so those recordings are WINDOWLESS (no rate band).
#   Inputs must be all .rez or all .parquet;
# filter file.rez --samplers cpu_usage,scheduler -o slim.rez drops tables whose sampler is not listed
#   (both containers; on a v3 archive a sampler's group tables are dropped together);
#   filter file.rez --metrics cpu_usage,cpu_frequency -o slim.rez keeps only those metric COLUMNS,
#   re-encoding each table's segments (timestamp/window sidecars always kept; a table with none of
#   the kept metrics is dropped); --samplers and --metrics compose, at least one is required;
# annotate file.rez --queries kpis.json embeds KPIs into each recording's manifest (--queries required for .rez).

# MCP - AI analysis server or CLI commands
# Status - agent health check (exits 1 if any sampler is degraded/failed/pmu-limited)
target/release/rezolus status                       # defaults to http://localhost:4241
target/release/rezolus status web-01                # bare host/host:port is normalized
target/release/rezolus status --json

target/release/rezolus mcp                                                    # stdio server
target/release/rezolus mcp install [--scope user|project] [--dry-run] [--no-skill] [server flags]
#   registers the server with Claude Code via `claude mcp add` (project scope writes .mcp.json
#   when `claude` is absent) and installs the rezolus-mcp skill (src/mcp/skill/SKILL.md,
#   embedded; replaced only when the file there is ours). src/mcp/install.rs.
target/release/rezolus mcp describe-recording file.parquet                    # describe recording
target/release/rezolus mcp describe-metrics file.parquet                      # list all metrics
target/release/rezolus mcp detect-anomalies file.parquet                      # exhaustive anomaly detection
target/release/rezolus mcp detect-anomalies file.parquet "cpu_usage"          # targeted anomaly detection
target/release/rezolus mcp query file.parquet "sum(irate(cpu_cycles[1m]))"     # PromQL query
# query prints an acquisition-window uncertainty band [lo, hi] beside rate()/irate() values
# (scalar ops scale the band, e.g. rate(x)*k; series-op-series combines both operands'
# bands by interval arithmetic; non-rate/non-histogram queries have no band to show)
target/release/rezolus mcp analyze-correlation file.parquet "metric1" "metric2"
target/release/rezolus mcp extract-features file.parquet             # structured feature record (JSON)
target/release/rezolus mcp describe-recording multi.rez              # lists the recordings + their selectors, not an error
target/release/rezolus mcp query multi.rez "sum(irate(cpu_cycles[1m]))" --recording source=redis  # pick one recording
# --recording key=value (repeatable, ANDed) is on all six subcommands, and the stdio server's
#   tools take an optional recording object, e.g. {"source": "redis"}, same semantics;
# it must name exactly one recording: none or several is an error listing the candidates, never a
#   first match. With no selector, an archive of one run is read combined (crate::viewer::opens_combined;
#   series carry their recording's labels); extract-features and a comparison (arm) need one.
# The stdio server also has write tools with no CLI form, in tiers by what they can destroy:
#   add_event (an instant or range event, source=mcp by default, id returned) and run_checks
#   (KPI checks -> verdicts JSON; annotate=true writes them as kind=check events) are always on;
#   remove_events (by ids/kind/source, empty filter refused) needs `rezolus mcp --allow-mutating`
#   and is otherwise neither listed nor callable. add_event/remove_events refuse a multi-recording
#   .rez without a selector (never stamp every arm); run_checks without one evaluates every
#   recording, as the CLI does. timestamp/duration: RFC 3339 or humantime strings, or numbers in
#   seconds; digit-only strings are refused as ambiguous. Handlers: src/mcp/server.rs; the write
#   path is parquet_tools::events::{add_events_selected, remove_events_selected} over the annotate
#   writer, whose report line the reply carries (a v2 tar is upgraded to v3 on the way and says so).
#   export_query (src/mcp/export.rs) writes a range query as long-form CSV/parquet (series,
#   timestamp, value, lo, hi) under `rezolus mcp --export-dir DIR` only, bare filename, never
#   overwrites; viewer_link (src/mcp/link.rs) formats the url_state.js wire form (fragment + query;
#   full URL with `--viewer-url` or a viewer_url argument), pinned to the JS parser by
#   tests/viewer_link_parity.test.mjs.
```

## Architecture

### Operating Modes

The binary operates in seven modes via subcommands:

1. **Agent** (`src/agent/`) - Default. Collects system metrics via samplers.
2. **Exporter** (`src/exporter/`) - Pulls from agent's msgpack endpoint, exposes Prometheus metrics.
3. **Recorder** (`src/recorder/`) - Writes metrics to a `.dendro` archive, the only format 6.x writes, through metriken-recorder's `Recorder` (metriken's `docs/journal/2026-10-12-metriken-recorder.md`): detection, the stream and scrape transports, restart metadata, the tick and the archive are the library's, and `src/recorder/mod.rs` is the CLI, the wrapped command, signals, exit codes and messages. Detects each endpoint by stream (`metriken_recorder::detect`, with the Rezolus `Probe` hook in `agent_probe.rs`): producers serving `/metrics/stream` are streamed and Prometheus endpoints scraped, an agent that cannot stream is refused, pointing at a 5.x recorder, and a streamed producer's `source`, labels and metadata come from its handshake below the user's and the hook's. Defaults to `rezolus.dendro` when no `-o` is given; any output not ending in `.dendro`, `--format` other than `dendro`, and `--separate` are refused (exit 2). Several endpoints in one run write one multi-recording archive, a recording each. Supports `--metadata key=value`; `--label key=value` tags every archive recording in the run, so `--endpoint url,source=name` is what distinguishes two endpoints (see "`.rez` archive format" below).
4. **Hindsight** (`src/hindsight/`) - Maintains a rolling archive buffer on disk (the archive writer with retention: everything older than `duration` is evicted each tick) for post-incident snapshots. It is a stream consumer, like `record`: a metriken-recorder `Recorder` with one streamed source (`Transport::Stream`, the Rezolus probe hook) and `retain` set to the lookback, which writes each interval at the agent's own stamp; `/status` counts passes and the retention bound from its `Committed` events. An agent that cannot stream is refused at startup. The buffer is readable live by the viewer/MCP/`recording metadata`. It is a dendro archive (`buffer.rs`'s `HindsightBuffer` remains as the tests' direct writer); an `output` not ending in `.dendro` is refused at config load (`config/general.rs` `check_output`), as `record` refuses one. Its snapshots go through dendro's `copy_sources_into` on the read handle, which seals the live tail into the copy (a fully sealed artifact, no WAL rows), and a ranged one starts one occupant restatement period early so long tables keep their labels.
5. **Viewer** (`src/viewer/`) - Web dashboard with PromQL query engine and TSDB (from `metriken-query` crate). Supports parquet files, archives (`.dendro`, `.rez`), live agent connections and upload-only mode. An archive of several recordings opens as ONE combined view unless it is a comparison (`dashboard::combined::layout`, one function both shells call: A/B is opt-in: when `--baseline`/`--experiment` (`?compare=1` on the static site) are given or any recording carries `arm`, from `record --endpoint url,arm=` or `recording combine --ab`; combined otherwise, nothing inferred from timing). Combined: `RezReader::combine` joins the recordings (every series under its recording's labels, metriken-storage `series_labels`), `dashboard::dashboard::build_combined_context` instantiates each template once per recording that carries it (the first instance at the template's routes, the others at `<route>@<key>`, `Section::instance` naming the host for the sidebar's per-instance groups), and every query of an instance is restricted to its recording by `combined::restrict_query` (a token-level rewrite adding the recording's discriminating label matchers to every selector: `k="v"` for a label it has, `k!="v"`/`k!~"v1|v2"` for one only other recordings carry), rendered against `combined::Scoped` (the recording's catalog and header). A comparison: >2 recordings shows the first two unless `--baseline k=v` / `--experiment k=v` name which recordings fill the slots — repeatable, ANDed, subset match, the same selector semantics as the MCP `--recording` flag, and each must name exactly one recording or the run is refused with a listing. Live agent connections are a stream consumer (`src/viewer/live.rs` records `/metrics/stream` into a temporary dendro archive through a metriken-recorder `Recorder` with no retention and reads it through `rez::live::LiveReader`, which reopens after each `Committed` event because an open `ArchiveReader`'s view is fixed; saving copies the archive). A `.dendro` or `.rez` v3 file opened while unfinalized (a running hindsight buffer, a `record` in progress) is followed: `src/viewer/follow.rs` wraps every recording shown (baseline, experiment and any further capture) in a `LiveReader`, and a thread reopens the archive and hands each reader its recording (`LiveReader::replace`, matched by full label set). Each reopen of a dendro archive first reads the catalog's `sources` once (before `open_recordings`, so a source the catalog reads as finalized is finalized in the open that follows) and judges each followed recording's writer with a `dendro::archive::HeartbeatWatch` (matched by exact label set): any `Live` → reopen every 2 s with no backoff, however slowly rows arrive; otherwise (`Stopped`: killed, paused, or a `cp`/`recording snapshot` copy; or `Unknown`: a `.rez`, or a dendro source with no heartbeat) → 2 s while the newest row advances, and after a reopen that finds no new row the wait doubles, capped at 60 s (`follow::MAX_WAIT`); a `Stopped` writer that beats again is `Live` again. The follow ends only when every recording is finalized, when the file is removed (hindsight removes its buffer on SIGTERM/SIGINT without finalizing it), or when the baseline is replaced. Do not end a follow on time since the last row or on `Stopped`: a writer may sample once a minute, `interval()` reads 1 s for an archive with too few rows, and a paused writer reads `Stopped` until it beats again. Detaching the experiment drops its reader from the follow; replacing the baseline ends it; a comparison whose shown recordings share a label set with another recording is not followed; a combined archive is followed as one `LiveReader::combined`, which joins every recording again on each reopen. `/api/v1/mode` reports `following` (distinct from `live`), and the page polls as in live mode but keeps the file-mode label, `from`/`to` handling and compare; in compare mode `noteRecordingExtent` replaces `experimentQueryRange` when the experiment's range changes, and `CompareChartWrapper` fetches again when that range changes. A finalized file is never reopened. Generates service KPI dashboards from `ServiceExtension` metadata.
6. **MCP** (`src/mcp/`) - AI analysis tools (anomaly detection, correlation, PromQL queries, feature extraction). Runs as stdio server or one-shot CLI commands. `query` prints acquisition-window uncertainty bands `[lo, hi]` beside `rate()`/`irate()` values (scalar ops scale the band; series-op-series combines both operands' bands by interval arithmetic, and operands from *different* acquisition tables have their bands widened to the union of both spans first — identical edges mean the same read, so the common case widens by nothing; non-rate/non-histogram queries have no band). See `docs/journal/2026-08-21-cross-table-uncertainty.md`. `extract-features` emits a deterministic, versioned overview record (JSON) summarizing a recording's Rezolus-native features.
7. **Parquet** (`src/parquet_tools/`) - File operations: `metadata` (inspect; on a `.rez`, describes the manifest), `annotate` (add service extension KPIs or timeline events; on a `.rez`, `--queries` embeds KPIs and the event flags embed events into each recording's manifest), `combine` (merge multi-source files, assemble a multi-recording `.rez` from `.rez` OR `.parquet` inputs — the preferred A/B form; parquet inputs become windowless recordings — or build a legacy A/B tarball), `filter` (drop columns not needed by KPIs; on a `.rez`, `--samplers` drops every table whose sampler is not listed and `--metrics` trims metric columns, re-encoding segments), `convert` (raw msgpack recording, plain or zstd, into parquet — the offline complement to a 5.x `record -o out.raw`), `snapshot` (a complete standalone copy of a `.rez` that is still being written — `cp` leaves SQLite's `-wal` sidecar behind and silently ends early). Those four accept `.rez` inputs; `convert` takes raw input only and emits parquet only.

### Sampler Architecture

Samplers live in `src/agent/samplers/{category}/`. Each sampler:
- Has platform-specific implementations (`linux/`, `macos/`)
- Registers via `linkme` distributed slice (`SAMPLERS` in `src/agent/samplers/mod.rs`)
- Implements the `Sampler` trait with `name()` and `refresh()` methods

Samplers with eBPF programs (Linux only) have a `mod.bpf.c` file alongside the Rust module. The BPF programs are compiled at build time by `build.rs`.

BPF-enabled samplers: `blockio/blockio`, `cpu/{bandwidth,perf,tlb_flush,usage}`, `network/{interfaces,traffic}`, `scheduler/scheduler`, `syscall/syscall`, `tcp/{connect_latency,packet_latency,receive,retransmit,traffic}`.

### eBPF Build System

`build.rs` compiles BPF programs using libbpf-cargo:
- Architecture-specific vmlinux.h headers in `src/agent/bpf/{x86_64,aarch64}/`
- Output skeletons go to `$OUT_DIR/{sampler}_{program}.bpf.rs`
- Requires clang for BPF compilation

### Parquet File Format

Parquet files produced by 5.x recorders use a columnar layout from `metriken-exposition`:
- **`timestamp`** (UInt64) - Nanoseconds since Unix epoch. Present in every file.
- **`duration`** (UInt64, nullable) - Snapshot collection duration in nanoseconds.
- **Metric columns** - One per metric: counters (UInt64), gauges (Int64), histograms (List&lt;UInt64&gt;).
- **Column metadata** - Each field carries `metric_type` ("counter"/"gauge"/"histogram"/"timestamp"/"duration") and metric labels. `docs/labels.md` is the audit of which keys are storage keys (`metric`, `metric_type`, `unit`, histogram config: consumed on load, never labels), which are labels, and the `__` rule for internal labels (`__name__`, `__run__`, planned `__incarnation__`: identity and matchable, hidden from listings and legends).

File-level metadata keys are defined in `src/parquet_metadata.rs`:
- `source` - Recording source: `"rezolus"` (single) or `["rezolus","llm-perf"]` (combined).
- `version` - Version of the **agent that was recorded**, not the recorder's own build; read from the agent's `/status` (or its `/` banner for agents older than 5.16.0), or for another streamed producer from the `version` key of its stream handshake's metadata. Absent for a Prometheus source and for recordings written by recorders that predate the capture. In a `.dendro` or `.rez` it is per-recording, so a multi-host or multi-build archive stays unambiguous.
- `producer_epoch` - An opaque id (v4 UUID) minted once per agent **process**, so it changes when and only when every cumulative counter restarted from zero together. Carried on every snapshot and on `/status`, and it is the source uuid of the agent's `/metrics/stream` handshake: the recorder and hindsight compare each reconnect's handshake with the epoch on record (`note_epoch` in src/recorder/mod.rs), which is how they notice a restart between two connections — values alone cannot distinguish a counter that reset from one that wrapped. A 5.x scraping recorder read it from each snapshot instead. Source-wide: it says nothing about a single counter that wrapped or that a sampler zeroes on read. Name and semantics follow dendro's `keys::PRODUCER_EPOCH`.
- `sampling_interval_ms` - Collection interval in milliseconds.
- `systeminfo` - JSON hardware summary from agent.
- `descriptions` - JSON map of metric name to help text. Present in single-source files; combined files nest this under `per_source_metadata.<source>.descriptions` instead.
- `per_source_metadata` - Per-source map with `version`, `role` ("service"/"loadgen"), `service_queries` (ServiceExtension KPI definitions), and `descriptions` (metric name → help text, combined files only).

### `.rez` Archive Format

The `.rez` format (metriken-storage's `src/rez/`: `rez.rs`, `rez_sqlite.rs`, `open.rs`; rezolus's `crates/rez/src/reader.rs`) is a container holding many parquet tables plus a `manifest.json`, rather than a single parquet file. Two container shapes exist and both are readable:

- **v2 (tar)** — a tar archive with `manifest.json` and one table per sampler (`<recording>/<sampler>.parquet`).
- **v3 (SQLite)** — a SQLite container (metriken-storage's `src/rez/rez_sqlite.rs` and `src/rez/rez_v3_writer.rs`) with a real WAL: rows land in the WAL and are periodically sealed into parquet segments. This is what a 5.x `hindsight` maintains as its rolling buffer (6.x keeps a `.dendro`), and it is readable live while a writer is still appending.

Each sampler records at its own cadence. Table granularity depends on the snapshot format the agent produced:

- From a **SnapshotV2** agent, a table holds a whole sampler and each metric carries its own acquisition-window columns (`<m>:window_begin`/`<m>:window_width`).
- From a **SnapshotV3** agent, tables are per *acquisition group* and keyed `<sampler>/<group>` (see "Acquisition Groups" in `docs/principles.md`, principle 18). Because a group is by definition one read with one window, the table carries a single table-level window pair — bare `:window_begin`/`:window_width` with no metric id — applying to every metric in it. `table_sampler()` splits a table key at the first `/`, so the sampler stays the manifest/filter unit, and the reader unions a sampler's group tables (dispatch by metric name — no timestamp join, no column concatenation, no null filling).

Either way the query engine consumes the window columns to compute `rate()`/`irate()` uncertainty bounds.

A segment carries no declared sampling interval and needs none: the rows carry timestamps, and metriken-query reads them. (Until 0.24.0 it rounded every timestamp to a grid derived from the file's `sampling_interval_ms`, defaulting to 1000ms when absent — which a `.rez` segment always is. A sub-second archive therefore read back wrong, ten rows of a second collapsing onto one instant, until the reader stopped rounding.) `RezReader::interval()` is a measured figure — a table's catalog span over its gaps, rounded to whole ms — and it is only a hint: the staleness bound and the step the viewer opens at. Nothing is rounded to it, so it does not have to be exactly right.

Because each sampler records at its own cadence, a query spanning *two samplers of materially different cadence* (measured row spacing differing by at least 2x, with the slower coarser than the step) is evaluated at the **slow sampler's own row timestamps** rather than on the uniform `start + k·step` grid — otherwise nearly every point lands where the slow sampler never read and its value is merely held forward. metriken-query (`src/archive.rs`, the private `cross_cadence_eval_timestamps`, reached through `eval_timestamps_for` and the query methods) picks those timestamps and passes them as `QueryOptions::eval_timestamps`; cadence is measured from the rows (never `interval()`, which reports one nominal value for the whole archive) and per *sampler*, since a group that skips ticks is sparse within its sampler's cadence rather than a second cadence. Single-sampler queries and `RateMode::Raw` are untouched.

A `.dendro` (and a `.rez` from a 5.x recorder) holds any number of endpoints, rezolus or Prometheus. A Prometheus target is converted on the way in: one scrape is one request and one response, so it becomes a single V3 acquisition group named `prometheus/scrape`, windowed by the real HTTP round trip (metriken-recorder's `PrometheusConverter`). That is why it is a group rather than a flat V2 snapshot — a V2 table carries `<name>:window_begin`/`<name>:window_width` per value column, which would triple the schema width of every Prometheus table. The manifest carries per-recording label sets (`source`/`host` auto-populated, plus any `record --label k=v`, which applies to every recording in the run); a multi-recording archive of one run (`record --endpoint a --endpoint b`) opens as one combined view; one whose recordings carry `arm` (`record --endpoint url,arm=baseline`, `recording combine --ab`) drives the viewer's A/B comparison, aliasing baseline/experiment from each recording's `arm`/`host` labels. The seal stagger keys on sampler + the recording's canonical label set (`seal_policy::recording_stagger_key`), so two recordings of the same agent do not seal in lockstep; identical label sets, and label sets that differ only in case, warn when the recording is added (metriken-recorder).

### `.rez` lives in its own crate

The `.rez` format is `metriken-storage`'s (feature `rez`, with `write` for the
v3 writer and the `.rez`-to-dendro conversion, `test-support` for the tar
fixture writer): container (v2 tar, v3 SQLite `RezDb`), writers, and opening an
archive by content (`metriken_storage::rez::open`). The archive reader is
storage's `ArchiveReader`; queries over it are `metriken-query`'s, which
implements `MetricsSource` for it under its `archive` feature (`crates/rez`
turns that on unconditionally, since the wasm viewer's read path needs it too;
the workspace declares metriken-query with `default-features = false`). See metriken's
`docs/journal/2026-10-09-storage-scan.md`. `crates/rez` keeps `RezReader` and
`LiveReader`, which implement `MetricsSource` and so cannot live in storage,
and re-exports storage's `rez` modules at the paths call sites use
(`rez::rez_sqlite`, `rez::rez_v3_writer`, ...). The `caller_rows` identity index
a 5.x `record --stream -o out.rez` wrote is not read; such a table logs a
warning and keeps the labels its segments carry.
It is a workspace crate rather than a module of the binary because `rezolus` is
**binary-only** (no `lib` target), so nothing can depend on it: a reader living
there was reachable by the server viewer and by nothing else; the static-site
WASM viewer reads `.rez` through `crates/rez`.

The binary re-exports the modules under the paths call sites already use
(`crate::recorder::rez`, `crate::rez_reader`, …), so `.rez` code reads the same
from inside `rezolus`.

Fixture builders (`rez::rez::recorder_tests_support`, and the tar writer
`rez_stream`) sit behind `test-support` (storage's, which `rez`'s turns on)
rather than `#[cfg(test)]`: a `#[cfg(test)]` module in another crate is
invisible to the binary crate's tests, which build most `.rez` fixtures in this
repo. The binary enables that feature as a dev-dependency.

**The `write` feature (default on) is what separates the two halves.** With it
off, `crates/rez` is a *reader*: container, catalog, WAL-tail materialization,
`RezReader`. That configuration is the one that compiles for
`wasm32-unknown-unknown`, and the reason it has to exist is narrow: `metriken`'s
registry (`metriken-core`) declares a `linkme` distributed slice, and `linkme`
gates on a fixed `target_os` list that `unknown` is not in. Storage's read path
names only `metriken-model` and `metriken-types` types (`Window`,
`GroupSchema`, the WAL rows), and its writers read `metriken-model` snapshots,
so no `.rez` code needs the registry; `rez`'s `write` feature still turns on
`metriken-storage/stream`, which does.

`cargo check -p rez --no-default-features --target wasm32-unknown-unknown` is a
CI step for exactly this reason; nothing else catches a registry type creeping
back onto the read path.

### Service Extensions

Service-level KPI dashboards are defined in `src/viewer/service_extension.rs` (`ServiceExtension`/`Kpi` structs). They allow the viewer to generate custom dashboard sections from PromQL queries embedded in parquet metadata. The `parquet annotate` command validates and embeds these. Templates live in `src/parquet_tools/templates/`.

### Built-in sections and section templates

The built-in sections (Overview, Exceptions, CPU, ...) are rendered from a section template (`crates/dashboard/src/section_template.rs`, schema 1), not by the Rust generators in `crates/dashboard/src/dashboard/`. Which template: `dashboard::source_templates::resolve_dashboard_template` takes the source's `dashboard` metadata key (sent by the agent in its stream handshake) when it is at most `MAX_DASHBOARD_BYTES` (1 MiB) and parses, else the embedded `rezolus_template::json()`, parsed once; parsed recording templates are cached by text. A recording's template with no showable section falls back too. `dashboard::dashboard::build_template_context` lists the template's sections less `is_reserved_route` ones (the page's own routes in `CLIENT_ROUTES`, and any route of more than one segment, which covers `/service/` and `/source/`), and `render_section` expands a built-in route with the viewer parameter `throughput` from the throughput KPI; `/service/` routes stay on the `ServiceExtension` generators. A section that cannot expand is an empty view with `metadata.template_error`, which each `viewer_api.js` copy logs once. The sidebar groups sections by route (`ui/section_groups.js`), and `SectionContent` mounts the query explorer and the client-only views by route (`ui/client_views.js`); `SectionTemplate::parse` refuses a body section at `/query`. `/api/v1/file_metadata` (`capture_registry.rs` `for_page`) and WASM `file_metadata_json` leave `dashboard` out. The server viewer (`src/viewer/metadata.rs` `capture_dashboard_template`, `state.rs` `LazySectionStore::generate`) and the WASM viewer (`crates/viewer/src/lib.rs` `dashboard_template`, `get_section`) both go through these. The generators stay: `crates/dashboard/src/section_template_tests.rs` checks the template renders byte-identical JSON to them, and `src/viewer/dashboard_queries_test.rs` grows its fixture from their reads.

### Static Site Viewer (WASM)

The `site/` directory hosts a browser-only viewer deployed to GitHub Pages. It shares the `src/viewer/assets/` frontend (via symlinks) with the server-backed viewer, but loads recordings directly in the browser through a WASM module.

The WASM crate lives at `crates/viewer/`. It targets `wasm32-unknown-unknown` and builds via `./crates/viewer/build.sh`; output goes to `site/viewer/pkg/` where the frontend imports it as `../pkg/wasm_viewer.js`. It is a workspace member like any other, but it cannot depend on `rezolus` itself — that is a **binary-only** crate with no `lib` target — so anything both viewers need lives in a shared crate (`dashboard`, `rez`) instead. See the `viewer-parity` skill.

**It opens both parquet files and `.rez` archives.** A `.rez` is recognized by content, not by filename, exactly as the CLI recognizes it, and a multi-recording archive opens as `rezolus view` opens it: one combined capture (`Viewer::new` and `WasmCaptureRegistry::attach` alike) unless it is a comparison by `dashboard::combined::layout`, or `WasmCaptureRegistry::set_compare_requested(true)` asks for one (the server's `--baseline`/`--experiment`), when its recordings fill the A/B slots and the rest attach after.

Save-as-Report writes the source's container: a dendro source saves a `.dendro` report (`report_save::build_dendro_report`, which seals the source's live tail with metriken-storage's `Encoder`, which has no feature gate and builds for wasm32), a `.rez` source or a parquet compare a `.rez`, a single parquet a `.parquet`.

One thing the browser cannot have: SQLite's `-wal` **sidecar**. A `.rez` carries its own unsealed rows in a `wal` TABLE inside the file, and those travel with an upload — but SQLite also commits into a sidecar file not yet checkpointed into the archive, and an upload is one file. `rezolus view archive.rez` opens by path with the sidecar beside it and sees further. This only differs for a copy taken from under a running recorder, and the gap used to be large: measured at **123 ticks (~2 minutes at a 1s interval)** on a 2000-tick recording, with a sidecar larger than the archive. Two things bound it now — the writer folds the sidecar into the archive every `rez_v3_writer::CHECKPOINT_INTERVAL` (10s), so a plain copy is at most that stale rather than unbounded; and `rezolus recording snapshot <archive> -o out.rez` reads through the sidecar for an exact copy, without pausing the writer.

The size-based autocheckpoint (4 MiB) and the time-based one answer different questions and both are needed: bytes bound the sidecar's disk footprint, time bounds how much of the recording a copy can be missing. A quiet recording takes hours to accumulate 4 MiB, and that is exactly the case where a copy is silently useless. The viewer also says so rather than rendering short in silence: `RezReader::complete()` is false for an unfinalized archive, and `WasmCaptureRegistry::notices()` reports it.

Two constraints shape how that works, and both are runtime failures rather than compile errors if broken:

- **No `metriken`.** Its registry declares a `linkme` distributed slice, and `linkme` has no wasm32 implementation — hence `crates/rez`'s `write` feature, which the viewer turns off. CI checks `cargo check -p rez --no-default-features --target wasm32-unknown-unknown`.
- **No threads.** `std::thread::spawn` compiles for wasm32 and panics at runtime, so the reader's parallel footer probe has a sequential wasm arm.

Building the bundle needs a clang that can target wasm32 (zstd and SQLite are compiled from C). Apple's does not; Homebrew LLVM does — `CC_wasm32_unknown_unknown=/opt/homebrew/opt/llvm/bin/clang`.

`crates/viewer` is `crate-type = ["cdylib", "rlib"]` so its behavior is testable natively (`cargo test -p viewer`, a CI step); `tests/wasm_viewer_*.test.mjs` drive the built bundle from node.

### Key Dependencies

- `metriken` - Metrics registration and exposition
- `metriken-exposition` - Snapshot serialization, and (`stream-tokio`) the `/metrics/stream` route the agent serves
- `metriken-storage` - Archive reader and writer, `.rez`, and (`convert`) msgpack-to-parquet conversion
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
- **macOS**: Limited (CPU usage, GPU, block IO counters and NVMe drive health; no eBPF)
- **Architectures**: x86_64 and ARM64

## Git Conventions

Do not append claude.ai session links to commit messages.

In the commands below, `<remote>` is the remote that points at iopsystems/rezolus: `upstream` in a fork checkout, otherwise `origin`.

Before committing on any PR, run `git fetch --tags <remote>` and then `./scripts/sync-docs-version.sh`. The script sets the sidebar version label in `site/docs/*.html` to the newest stable `v*` tag; when it changes a file, that change goes in the same commit.

Every PR to `main` other than a release PR bumps the pre-release revision in `Cargo.toml` (and `Cargo.lock`) by one from `<remote>/main`'s, including docs-only and dependency-only PRs: run `git fetch <remote> && git show <remote>/main:Cargo.toml | grep '^version'`, then, for example, `6.0.0-alpha.62` becomes `6.0.0-alpha.63`. Only release PRs change the major, minor or patch version.

Commit subjects and PR titles are conventional, `type(scope): description`. Open every PR other than a release PR as a draft.

## Releases

Cut stable releases with `/cs:release`. What it cannot read from the repository:

- `main` carries 6.x. `release/5.x` carries 5.x; a 5.x patch release is a release PR into `release/5.x`.
- `release.toml` sets the commit message to `release: prepare v{{version}}`, `publish = false`, and leaves tagging and pushing to the workflow. `tag-release.yml` calls `brayniac/rust-workflows` `tag-release.yml@v1` on pushes to `main` and `release/5.x`. After tagging from `main` it pushes the next development version (`X.Y.(Z+1)-alpha.0`) straight to `main`; after tagging from `release/5.x` it does no bump, and that branch keeps the released version until its next release PR.
- `release.yml` runs on every `v*` tag. It builds the .deb and .rpm packages and the GitHub release. A tag containing `-` or `+` is marked as a pre-release and skips the package-registry uploads. A stable tag goes to the apt and yum registries; it becomes the GitHub "Latest" release and updates the Homebrew formula only when it is the highest stable tag, so a 5.x patch cut after 6.0 does neither. Nothing is published to crates.io.
- A prerelease tags the current `main` version by hand, with no release PR: on `<remote>/main`, with a clean tree and a `Cargo.toml` version containing `-`, check `git ls-remote --tags <remote> v<version>` returns nothing, then `git tag -a v<version> -m "Prerelease v<version>"` and `git push <remote> v<version>`. Do not tag a stable version by hand; that goes through a release PR.
