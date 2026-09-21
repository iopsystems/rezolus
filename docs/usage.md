# Rezolus usage guide

[Back to the project overview](../README.md)

- [Agent](#agent) and [Exporter](#exporter)
- [Hindsight](#hindsight) and its [HTTP endpoint](#http-endpoint-optional)
- [Recorder](#recorder): [formats](#output-formats), [labels](#tagging-a-recording), [multiple endpoints](#several-endpoints-in-one-rez), [durability](#rez-durability)
- [Viewer](#viewer), [recording tools](#recording-tools), and [MCP](#mcp-server)
- [Source-build helper](#source-build-capture-helper) and [Docker](#docker)

Rezolus ships as a single binary that runs in several roles. On packaged Linux
installations,
the first three can run as managed services; the rest are on-demand subcommands.

## Agent

The core component. It collects performance metrics from the system using eBPF,
perf events, NVML/GPM, and traditional sources, and serves them over HTTP. The
agent listens on `0.0.0.0:4241` by default, so the Exporter, Recorder, and Viewer
can all read from it — locally or across the network.

Individual samplers can be enabled, disabled, or retuned in the agent config.

```bash
# edit the agent config
sudo editor /etc/rezolus/agent.toml
# restart to apply
sudo systemctl restart rezolus
```

## Exporter

Transforms collected metrics for Prometheus compatibility and exposes them on a
Prometheus-compatible endpoint. It can summarize histogram distributions down to
a few percentiles to cut storage cost, or expose full histogram buckets when you
need them.

Set the exporter interval to match your scrape interval: too short and summary
metrics won't cover the gap between scrapes; too long and metrics go stale.

```bash
sudo editor /etc/rezolus/exporter.toml
sudo systemctl restart rezolus-exporter
```

## Hindsight

Hindsight continuously collects telemetry into a local rolling buffer on disk.
Save the retained history after an incident without reproducing the workload.
It must be running before the event; configure its lookback and disk budget
before enabling it. It bounds retention, not the cost of collection or writes.

The buffer is an ordinary `.rez` recording trimmed to the configured lookback,
so you can open it with `rezolus view` or the MCP tools _while it is being
written_, and a snapshot is a consistent point-in-time copy taken without
pausing the recording — however it is triggered, by signal or over HTTP.

Hindsight is **disabled by default**. Review the config before enabling it.

```bash
sudo editor /etc/rezolus/hindsight.toml
sudo systemctl enable rezolus-hindsight
sudo systemctl start rezolus-hindsight
# trigger a save of the buffer to the output file
sudo systemctl kill -sHUP rezolus-hindsight
```

Hindsight can also expose an optional HTTP endpoint for remote buffer
management — see [HTTP Endpoint](#http-endpoint-optional) below.

## Recorder

Records metrics to disk for benchmarking, lab tests, or offline workload
characterization. It auto-detects Rezolus agent vs Prometheus sources and
supports custom file-level metadata. The endpoint must already be running;
`record` does not launch an agent.

Like `perf record`, it can wrap a workload and capture for exactly its lifetime,
finalizing when the command exits:

```bash
rezolus record -- ./my-benchmark --threads 8
```

By default this records the local agent (`http://localhost:4241`) into
`rezolus.rez`. Override the endpoint with `--url` and the output with `-o`:

```bash
rezolus record --url http://host:4241 -o run.rez -- ./driver
```

Or record a fixed window instead, until `--duration` elapses or you press
ctrl-c:

```bash
rezolus record --interval 1s --duration 15m --url http://localhost:4241 -o run.rez
```

When wrapping a command, `--duration` also acts as a safety cap: if the command
outlives it, recording stops and the command — along with any worker processes
it spawned — is terminated. The positional `<URL> <OUTPUT>` form still works but
is deprecated in favor of `--url`/`-o`.

### Output formats

The output path's extension picks the format, so `--format` is rarely needed;
with no `-o` at all the recording goes to `rezolus.<ext>` for the format in
play, which by default means `rezolus.rez`.

| Extension | What it is | When |
| --- | --- | --- |
| `.rez` | **Default.** An archive with separate acquisition groups and their cadences/windows. Holds one *recording* per endpoint. | One or more Rezolus or Prometheus endpoints, including mixed inputs. |
| `.parquet` | One columnar table on a single uniform clock. | Uniform tabular export or other Parquet tooling. |
| `.raw` | The msgpack snapshots as scraped, concatenated. | Capture now, decide later — convert with `rezolus recording convert`. |

Passing a `--format` that contradicts the extension (say `--format parquet` with
`-o out.rez`) is an error. Both Rezolus and Prometheus endpoints can be recorded
to `.rez`; a Prometheus scrape gets an acquisition window spanning the HTTP
request and response. Multiple endpoints remain separate recordings in the
same archive.

`--separate` with multiple endpoints requires parquet or raw: it requests one
file per endpoint. If the output format was left at its default, that option
selects parquet; an explicit `.rez` output instead produces an error.

A `.rez` output path must not already exist — the recorder refuses rather than
truncate, since the archive is committed as it goes and has no staging file. A
parquet or raw output is overwritten. There is no `--force`.

When wrapping a command, rezolus passes its stdio straight through and exits
with the command's own status, so `rezolus record -o bench.rez -- ./bench.sh &&
analyze bench.rez` gates on the benchmark; the exception is the `--duration`
cap, which exits 124 if it has to kill the command.

### Tagging a recording

`-m/--metadata k=v` writes file-level metadata and applies to every format.
`-l/--label k=v` applies to `.rez` only — it tags the recording inside the
archive (`source` and `host` are filled in for you), and a two-recording `.rez`
drives the viewer's A/B comparison, which is what `--label arm=redis` is for:

```bash
rezolus record --url http://localhost:4241 -o out.rez --label arm=redis
```

`--label` applies to *every* recording the run produces, so in a multi-endpoint
run it cannot tell two endpoints apart — see below.

### Several endpoints in one `.rez`

A `.rez` holds one *recording* per endpoint, so several endpoints can be
captured in a single invocation and land in one archive:

```bash
rezolus record --endpoint http://web-01:4241 --endpoint http://web-02:4241 -o fleet.rez
```

A two-recording `.rez` opens in the viewer as an A/B comparison. Concurrent
captures share the same time period; captures taken sequentially can also
differ in background load, not just the experimental change.

Each recording is identified by its label set. For a Rezolus endpoint, `host`
is filled in from the
agent's system info and `source` from the endpoint's `source=` modifier, so two
agents on *different* hosts are already distinguishable. Two on the **same**
host are not — and `--label` cannot separate them, because it applies to every
recording alike. Give each endpoint its own `source=`:

```bash
rezolus record \
  --endpoint http://localhost:4241,source=redis \
  --endpoint http://localhost:4242,source=valkey \
  -o ab.rez
```

If two recordings end up with identical labels, the recorder warns at startup:
nothing downstream can tell them apart, and they will also seal their segments
in lockstep. Rezolus and Prometheus endpoints can coexist in one archive
(see [Output formats](#output-formats)).
`--separate` does not apply to `.rez`, which already keeps each endpoint as its
own recording inside the one archive.

### `.rez` durability

`.rez` uses a SQLite container with incrementally committed samples and sealed
Parquet segments. Ctrl-c and SIGTERM interrupt the wait between samples and
finalize the still-open segments, rather than rewriting the entire recording.
There is no `.partial` staging file. After an unclean stop, inspect the archive's
completion status and retained time range before treating it as a complete run:

```bash
rezolus recording metadata -i out.rez   # reports "not cleanly finalized"
```

For a file still being written, use `rezolus recording snapshot live.rez -o
incident.rez` to include committed data in SQLite's sidecar; copying only the
main file can miss recent samples.

The previous tar container is no longer written. Archives recorded by older
releases still open everywhere, and `rezolus recording upgrade old.rez` converts
one to the current container — as does rewriting it with `combine`, `filter` or
`annotate`, all of which read either container and emit the current one.

## Viewer

`rezolus view` runs a Rust HTTP server that reads recordings or streams a live
agent, evaluates PromQL queries, and serves the web dashboard. Run it locally
to keep processing on your machine. It supports A/B comparisons, diff heatmaps,
and quantile heatmaps. A remote agent must be reachable from the viewer server.

```bash
# open a recording
rezolus view run.rez
# A/B compare two recordings
rezolus view baseline.rez experiment.rez
# stream live from an agent
rezolus view http://localhost:4241
# upload-only mode (no file argument)
rezolus view
```

Prefer the terminal? Pass `--tui` to render in the terminal instead of the
browser — a curated live overview plus a drill-down browser of the same
sections. It works with a recording or a live agent URL (not upload-only
mode), and serves no HTTP, so `--listen` and the `--proxy-*` flags don't apply.
A/B compare remains browser-only.

```bash
# explore a recording in the terminal
rezolus view --tui run.rez
# stream a live agent in the terminal UI
rezolus view --tui http://localhost:4241
```

Keys: `Tab`/`o` toggle overview ↔ browser, `j`/`k` move/scroll, `Enter`/`l`
open, `Esc`/`h` back, `[` / `]` change the time window, `r` refresh, `?` help,
`q` quit.

The same web dashboard is also available as a browser-only static site under
[`site/viewer/`](../site/viewer/), powered by the
[`crates/viewer`](../crates/viewer) WASM module. It runs the PromQL query engine
client-side, so uploaded `.parquet` and `.rez` recordings never leave the browser.

## Recording tools

`rezolus recording` inspects and transforms `.parquet` files and `.rez`
archives. `parquet` remains a compatibility alias for the command. Operations
and flags differ by format; check `rezolus recording <subcommand> --help`.

- **Metadata** — inspect file-level and column-level metadata, geometry, and
  schema.
- **Annotate** — embed service extension KPI definitions for custom viewer
  dashboards.
- **Combine** — merge a Rezolus parquet with service-level parquet files,
  joining on timestamps to produce a unified multi-source recording, or package
  captures as a multi-recording `.rez`. A/B tarballs are a legacy option.
- **Convert** — turn a raw msgpack recording (from `record -o out.raw`) into
  parquet. The input may be plain or zstd-compressed; which one it is is
  detected from the file's contents, not its name.
- **Filter** — drop parquet columns not referenced by service KPIs, or select
  samplers from a `.rez` archive.
- **Upgrade** — convert older tar-based `.rez` archives to the SQLite container.
- **Snapshot** — copy a live `.rez` consistently, including committed data in
  SQLite's sidecar. Prefer this over copying an active file with `cp`.

```bash
rezolus recording metadata -i rezolus.parquet
rezolus recording annotate rezolus.parquet --queries ext.json
rezolus recording combine rezolus.parquet service.parquet -o combined.parquet
rezolus recording convert rezolus.raw.zst              # writes rezolus.parquet
rezolus recording filter rezolus.parquet -o slim.parquet
rezolus recording snapshot live.rez -o incident.rez
rezolus recording upgrade old.rez
```

`convert` infers the sampling interval from the median gap between snapshot
timestamps; pass `--interval` to override it. It warns on stderr (without
failing) when the sampled gaps have no dominant cadence, or when they are closer
together than the whole milliseconds `sampling_interval_ms` can hold. A raw recording carries no
`systeminfo` or metric descriptions — the recorder fetches those from the
agent's `/systeminfo` and `/metrics/descriptions` endpoints while recording — so
supply them with `--systeminfo` / `--descriptions` if you saved them. Afterwards
`rezolus recording annotate --systeminfo` can still add the hardware summary, but there is
no annotate route for descriptions. A `.rez` archive cannot be produced from a
raw recording: it needs the per-sampler cadence and acquisition windows that a
raw snapshot stream never carried.

## MCP Server

Exposes Rezolus recordings to LLM-based assistants over the Model Context
Protocol, with tools for querying metrics via PromQL, detecting anomalies, and
analyzing correlations — useful for AI-guided performance investigation. Runs as
a stdio MCP server or as one-shot CLI commands.

```bash
rezolus mcp                                                  # stdio server
rezolus mcp detect-anomalies run.rez                 # anomaly detection
rezolus mcp query run.rez "sum(rate(cpu_cycles[1m]))"
```

`extract-features` distills a whole recording into one deterministic, versioned
JSON record — the structured input for an AI-assisted bottleneck assessment,
rather than a series of ad-hoc queries. For every metric it reports summary
stats, noise classification, anomalies, regime shifts, and acquisition-window
uncertainty; alongside that it reports top cross-metric correlations, resource
rankings, and subsystem coverage. Requires a recording of at least 10 seconds:

```bash
rezolus mcp extract-features run.rez   # parquet or .rez recordings
```

The six stdio tools are `describe_recording`, `describe_metrics`,
`extract_features`, `detect_anomalies`, `analyze_correlation`, and `query`.
CLI names use hyphens. For an investigation, describe the recording and metrics
first, confirm the source and time range, extract features, then query specific
hypotheses. Missing metrics are not zero, and correlations do not establish
causation.

### Multi-recording archives

A `.rez` built from several endpoints (see "Several endpoints in one `.rez`"
above) holds one *recording* per endpoint. Every MCP tool reads one recording
at a time, so run `describe-recording` with no selector to see what the
archive holds and the flag that picks each one:

```
$ rezolus mcp describe-recording ab.rez
ab.rez holds 2 recordings with data (a multi-host or A/B archive):
  - host=web-01, source=redis
    select with: --recording source=redis
  - host=web-01, source=valkey
    select with: --recording source=valkey

Run any tool with one of the selectors above to analyze that recording.
```

Then pass `--recording key=value` (repeatable, ANDed) to any of the six
subcommands to analyze that one recording:

```bash
rezolus mcp query ab.rez "sum(rate(cpu_cycles[1m]))" --recording source=valkey
```

A selector must name exactly one recording: matching none or several is an
error that lists the candidates, never a guess at which one you meant. The
stdio server's six tools take the same selector as an optional `recording`
object instead of repeated flags, e.g. `{"source": "valkey"}`.


## HTTP endpoint (optional)


Hindsight can optionally expose an HTTP endpoint for remote buffer management.
Enable it by adding a `listen` address to the configuration:

```toml
listen = "127.0.0.1:4242"
```

Available endpoints:

- `GET /status` — returns buffer status: the time range actually retained, rows
  and segments per sampler, on-disk size, and whether retention has started
- `GET /dump` — downloads the buffer as a `.rez` archive
- `POST /dump/file` — writes the buffer to the configured output file

The `/dump` and `/dump/file` endpoints support query parameters for time
filtering. A `.rez` segment is an immutable parquet blob, so a filtered dump
keeps any segment overlapping the range rather than splitting one: you may get
a little more than you asked for, and `/dump/file` reports the span it actually
wrote.

| Parameter | Description                             | Example                       |
| --------- | --------------------------------------- | ----------------------------- |
| `last`    | Relative time range                     | `?last=5m`                    |
| `start`   | Start time (Unix timestamp or RFC 3339) | `?start=2024-01-01T12:00:00Z` |
| `end`     | End time (Unix timestamp or RFC 3339)   | `?end=2024-01-01T13:00:00Z`   |

Examples:

```bash
# check buffer status
curl http://localhost:4242/status

# download last 5 minutes as a .rez archive
curl -o dump.rez "http://localhost:4242/dump?last=5m"

# download a specific time range using RFC 3339 datetime
curl -o dump.rez "http://localhost:4242/dump?start=2024-01-01T12:00:00Z&end=2024-01-01T13:00:00Z"

# trigger a dump to the configured output file
curl -X POST http://localhost:4242/dump/file
```


## Source-build capture helper

After installing the [build prerequisites](installation.md#build-prerequisites),
run these commands from the repository root:

```bash
cargo build --release
sudo scripts/rezolus-capture --rezolus ./target/release/rezolus --duration 60s
```

The helper starts an agent if necessary, records to parquet, and launches the
viewer. Its default agent config is `config/agent.toml`; use `--agent-config`
if you run it elsewhere. For service metrics, pass paired `--endpoint` and
`--source` flags. This helper's parquet workflow is separate from the recorder's
`.rez` default. Run `scripts/rezolus-capture --help` for its full options.

## Docker

```bash
docker run --rm -it --privileged \
  -p 8080:8080 -v "$(pwd)/data:/data" \
  ghcr.io/iopsystems/rezolus:latest \
  rezolus-capture --duration 60s
```

See [Docker usage](../docker/README.md) for system and service captures.
