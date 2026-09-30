# Rezolus usage guide

[Back to the project overview](../README.md)

- [Agent](#agent) and [Exporter](#exporter)
- [Hindsight](#hindsight) and its [HTTP endpoint](#http-endpoint-optional)
- [Recorder](#recorder): [formats](#output-formats), [labels](#tagging-a-recording), [multiple endpoints](#several-endpoints-in-one-archive), [durability](#archive-durability)
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

The buffer is an ordinary `.dendro` recording trimmed to the configured
lookback, so you can open it with `rezolus view` or the MCP tools _while it is
being written_, and a snapshot is a consistent point-in-time copy taken without
pausing the recording — however it is triggered, by signal or over HTTP.

An `output` ending in `.rez` keeps the buffer and its snapshots in the archive
format before 6.0, several times larger where threads and cgroups come and go.
Any other `output` is a `.dendro` archive.

Hindsight reads the agent named by `source` over its replication stream
(`/metrics/stream`), which agents from 5.21.0 serve when `snapshot_format` is
`"v3"` (the default). An agent that cannot serve it is refused at startup. If the stream drops, hindsight reconnects
after one interval and keeps its buffer.

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
`rezolus.dendro`. Override the endpoint with `--url` and the output with `-o`:

```bash
rezolus record --url http://host:4241 -o run.dendro -- ./driver
```

Or record a fixed window instead, until `--duration` elapses or you press
ctrl-c:

```bash
rezolus record --interval 1s --duration 15m --url http://localhost:4241 -o run.dendro
```

When wrapping a command, `--duration` also acts as a safety cap: if the command
outlives it, recording stops and the command — along with any worker processes
it spawned — is terminated. The positional `<URL> <OUTPUT>` form still works but
is deprecated in favor of `--url`/`-o`.

### Output formats

The output path's extension picks the format, so `--format` is rarely needed;
with no `-o` at all the recording goes to `rezolus.<ext>` for the format in
play, which by default means `rezolus.dendro`.

| Extension | What it is | When |
| --- | --- | --- |
| `.dendro` | **Default** (since 6.0). An archive with separate acquisition groups and their cadences/windows. Holds one *recording* per endpoint. Groups whose members come and go (threads, cgroups, CPUs) are stored one row per member. Rezolus agents are streamed (5.21.0 or later) and Prometheus endpoints scraped. | One or more Rezolus or Prometheus endpoints, including mixed inputs. |
| `.rez` | The same recordings in the archive format before 6.0, several times larger than a `.dendro`. Every tool still reads and writes it. Scrapes every endpoint. | A consumer that has not moved to `.dendro`, or an agent older than 5.21.0; `rezolus recording upgrade --to dendro out.rez -o out.dendro` converts it afterwards. |
| `.parquet` | One columnar table on a single uniform clock. | Uniform tabular export or other Parquet tooling. |
| `.raw` | The msgpack snapshots as scraped, concatenated. | Capture now, decide later — convert with `rezolus recording convert`. |

Passing a `--format` that contradicts the extension (say `--format parquet` with
`-o out.dendro`) is an error. Both Rezolus and Prometheus endpoints can be
recorded to an archive (`.dendro` or `.rez`); a Prometheus scrape gets an acquisition window spanning the HTTP
request and response. Multiple endpoints remain separate recordings in the
same archive. A `.dendro` takes each Rezolus agent from its replication stream
rather than scraping it, and refuses an agent that cannot serve one; see
[Streaming and scraping](#streaming-and-scraping). The other formats scrape
every endpoint.

`--separate` with multiple endpoints requires parquet or raw: it requests one
file per endpoint. If the output format was left at its default, that option
selects parquet; an explicit `.rez` or `.dendro` output instead produces an
error.

An archive output path (`.dendro` or `.rez`) must not already exist — the recorder refuses rather than
truncate, since the archive is committed as it goes and has no staging file. A
parquet or raw output is overwritten. There is no `--force`.

When wrapping a command, rezolus passes its stdio straight through and exits
with the command's own status, so `rezolus record -o bench.dendro -- ./bench.sh &&
analyze bench.dendro` gates on the benchmark. Two substitutions: the
`--duration` cap exits 124 if it kills the command, and a recording failure
(an endpoint refused mid-run, a write error) exits 1 whatever the command
returned.

A wrapped run is also marked in the recording: a `run_start` event when the
command spawns and a `run_end` event when its exit is observed (both in `.dendro`,
`.rez` and parquet output; raw output has no metadata to carry them), named by the
program alone. Pass `--record-command-line` to store the full argument list in
the `run_start` event's details, since arguments can carry paths or tokens you
may not want in a file you share.

### Tagging a recording

`-m/--metadata k=v` writes file-level metadata and applies to every format.
`-l/--label k=v` applies to archives (`.dendro`, `.rez`) only — it tags the
recording inside the archive (`source` and `host` are filled in for you), and a
two-recording archive drives the viewer's A/B comparison, which is what `--label arm=redis` is for:

```bash
rezolus record --url http://localhost:4241 -o out.dendro --label arm=redis
```

`--label` applies to *every* recording the run produces, so in a multi-endpoint
run it cannot tell two endpoints apart — see below.

### Several endpoints in one archive

An archive (`.dendro` or `.rez`) holds one *recording* per endpoint, so several endpoints can be
captured in a single invocation and land in one archive:

```bash
rezolus record --endpoint http://web-01:4241 --endpoint http://web-02:4241 -o fleet.dendro
```

A two-recording archive opens in the viewer as an A/B comparison. Concurrent
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
  -o ab.dendro
```

If two recordings end up with identical labels, the recorder warns at startup:
nothing downstream can tell them apart, and they will also seal their segments
in lockstep. Rezolus and Prometheus endpoints can coexist in one archive
(see [Output formats](#output-formats)).
`--separate` does not apply to an archive, which already keeps each endpoint as
its own recording.

### Archive durability

`.dendro` and `.rez` both use a SQLite container with incrementally committed samples and sealed
Parquet segments. Ctrl-c and SIGTERM interrupt the wait between samples and
finalize the still-open segments, rather than rewriting the entire recording.
There is no `.partial` staging file. After an unclean stop, inspect the archive's
completion status and retained time range before treating it as a complete run:

```bash
rezolus recording metadata -i out.dendro   # reports "not cleanly finalized"
```

For a file still being written, use `rezolus recording snapshot live.dendro -o
incident.dendro` to include committed data in SQLite's sidecar; copying only the
main file can miss recent samples.

The `.rez` tar container is no longer written. Archives recorded by older
releases still open everywhere, and `rezolus recording upgrade old.rez` converts
one to the current container — as does rewriting it with `combine`, `filter` or
`annotate`, all of which read either container and emit the current one.

### Streaming and scraping

A `.dendro` records each Rezolus agent from its replication stream
(`/metrics/stream`). The agent pushes one frame per `--interval`, carrying
only the acquisition groups it re-read since the last frame, stamped when the
agent sampled rather than when the recorder asked. Which task or cgroup each
slot means travels in each group's schema, and the `.dendro` takes it from
there into its occupant streams. A Prometheus endpoint cannot stream, so it is
scraped each tick, and one run can hold both kinds, each endpoint its own
recording:

```bash
rezolus record --url http://localhost:4241 -o run.dendro
rezolus record --endpoint http://agent:4241 --endpoint http://svc:9090/metrics,source=svc -o run.dendro
```

There is no scrape path for a Rezolus agent into a `.dendro`. An agent that
cannot serve the stream — older than 5.21.0, when `/metrics/stream` shipped, a
V2 agent, a stream route that answers 404 or another refusal (a proxy that
does not route the path, or answers 401 or 403), or one whose handshake does
not decode (a replication protocol version mismatch names both versions) — is
refused with its version. At startup the run is refused before the archive is
created. An agent that comes up later and is refused is left out: the other
endpoints keep recording, the archive is finalized, and the run exits 1. When
every endpoint has been refused, the run ends there. At the end each refused
endpoint gets its own line, saying from when the archive has no rows for it.
Record such an agent to a `.rez` or parquet, which scrape, and convert a
`.rez` afterwards if you want a `.dendro`:

```bash
rezolus record --url http://old-host:4241 -o run.rez
rezolus recording upgrade --to dendro run.rez -o run.dendro
```

An agent that is merely unreachable is retried each tick, as is one that
answers but whose stream fails to open with an error that can change (a 5xx
from a proxy, a handshake that times out). A stream that drops mid-run, or that
produces no frame for the scrape timeout (twice the interval, between 2 s and
10 s), is reconnected after one interval (at least a second), with the drop and the reconnect logged; rows between the two
are lost, as a failed scrape's are. An agent that comes back unable to serve
the stream (a 404, a wrong content type, a handshake that does not decode) is
refused as a late agent is: its recording stops with the rows it had and is
finalized, it is not retried, the other endpoints keep recording, and the run
exits 1.

A wrapped command (`-- <command>`) that exits on its own is followed by a wait
for each agent's frame stamped at or after the exit, at most one interval plus
the scrape timeout (twice the interval, between 2 s and 10 s), so the interval the command exited in is recorded even
when the agent's next frame is seconds away. Other stops (`--duration`,
ctrl-c) wait one interval, at most two seconds.

Streamed rows and the tick's scrapes go through one archive writer and are
committed together, once per tick.

A `.rez` recorded by a 5.x `record --stream` still opens.

## Viewer

`rezolus view` runs a Rust HTTP server that reads recordings or streams a live
agent, evaluates PromQL queries, and serves the web dashboard. Run it locally
to keep processing on your machine. It supports A/B comparisons, diff heatmaps,
and quantile heatmaps. A remote agent must be reachable from the viewer server.

```bash
# open a recording
rezolus view run.dendro
# A/B compare two recordings: combine them into one archive, then view it
rezolus recording combine baseline.dendro experiment.dendro -o ab.dendro
rezolus view ab.dendro
# two parquet files are compared as given
rezolus view baseline.parquet experiment.parquet
# stream live from an agent
rezolus view http://localhost:4241
# upload-only mode (no file argument)
rezolus view
```

A live agent is recorded over its replication stream (`/metrics/stream`,
served by agents from 5.21.0 when `snapshot_format` is `"v3"`, the default)
into a temporary `.dendro` archive under the system temp directory, which
the view reads as it grows and which is deleted when the viewer exits. Save
capture downloads a copy of it as `rezolus-capture.dendro`.

Prefer the terminal? Pass `--tui` to render in the terminal instead of the
browser — a curated live overview plus a drill-down browser of the same
sections. It works with a recording or a live agent URL (not upload-only
mode), and serves no HTTP, so `--listen` and the `--proxy-*` flags don't apply.
A/B compare remains browser-only.

```bash
# explore a recording in the terminal
rezolus view --tui run.dendro
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

### Aligning captures in a comparison

Two recordings rarely start at the same instant, so a comparison draws each
capture on a relative axis: `+0s` is that capture's anchor. By default the
anchor is the capture's recording start. The compare badge's **Align on**
menu lists every event kind any capture carries (a `run_start` written by
`rezolus record -- <command>`, or any kind added with `recording annotate
--event`); choosing one makes each capture's earliest event of that kind
its `+0s`, so two benchmark runs line up on the moment the benchmark
started rather than on when the recorder happened to start. A kind that
some captures lack is listed but not selectable, naming which ones lack it.
A capture whose saved anchor names an event its file does not carry is
drawn from its recording start, and the badge says so. The anchor rides in
the link as `anchor.<capture>=kind:<event kind>` (see below). Alignment
reads the events in each recording's file; an event added in the Notebook
counts only after it is saved into the recording. The diff views (the diff
heatmap, and the quantile diff) pair the two captures' cells by sample
step, shifting one capture by the whole number of steps between the two
anchors; when the anchors differ by a fraction of a step, no cells line up
and those views show the two captures side by side instead and say why.

### A baseline made of many recordings

With three or more captures open (a multi-recording `.rez`, or one assembled
with `recording combine`), the compare badge's **Baseline** menu can switch
from a single capture to a family: every capture except the experiment
becomes one baseline, drawn as a band, mean ± kσ (the menu offers k of 1,
2 or 3; a link may carry any positive k) or min..max, with its mean as a
line, and the experiment as the one line over it. Each member is aligned
by its own anchor, resampled onto the first member's grid so each
contributes one value per bucket whatever its cadence, and the legend
names the member count. The family applies to the line overlays; the
per-CPU, per-cgroup and percentile split charts, and the heatmap views,
still draw one series per capture. This is the nightly-run
question: is tonight's run outside where the last twenty landed. The
setting rides in the link as `family=sigma:2` or `family=envelope`. The
band is a spread over runs, not the acquisition-window measurement band;
the two are never drawn on one chart.

### Linking to a view

A viewer URL reproduces the view it was copied from. The section lives in
the hash (`#/overview`, `#/cpu`, `#/cgroups`, `#/service/<name>`; a chart
expanded from its toolbar is `#/<section>/chart/<id>`); the time range, time
mode, selectors and compare anchors live in the query string, **before** the
hash, so a link pasted to a colleague opens at the same window with the same
filters. Parameters placed after the hash (`#/cpu?from=...`) are not read.
The viewer rewrites the query string as you zoom or change a selector, so
the address bar is always a link to what you see. Both the server viewer and
the static site read the same parameters; the static site adds `capture=`
for the file. Compare mode is not a parameter: it comes from what was
opened (two files, or a two-recording `.rez`), and the `anchor.*` keys only
apply then.

```text
http://127.0.0.1:4200/?from=2026-09-28T14:03:11.250Z&to=2026-09-28T14:05:40Z#/cpu
https://rezolus.com/viewer/?capture=demo.parquet&from=2026-05-10T00:36:00Z&to=2026-05-10T00:37:00Z&time=raw#/scheduler
http://127.0.0.1:4200/?node=web-01&cgroup=/system.slice&cgroup=/user.slice#/cgroups
```

| Parameter | Value | Notes |
| --- | --- | --- |
| `from`, `to` | RFC 3339 UTC (`2026-09-28T14:03:11.250Z`) or Unix seconds (`1759068191.25`, fractions allowed) | Both required, `to` after `from`; clamped to the recording; ignored in live mode |
| `time` | `raw` | Rate points at their real sample timestamps. Absent means Aligned (grid). Ignored in compare mode |
| `node` | node name | Multi-node recordings; must exist in the recording |
| `gpu` | `vendor:id`, repeatable (`gpu=nvidia:0&gpu=nvidia:1`) | Filters the GPU section; a bare id when the sampler set no vendor |
| `cgroup` | cgroup name, repeatable | Selected cgroups on the cgroups section, one key per name |
| `instance` | instance id | A service's instance, scoped by the `#/service/<name>` in the hash |
| `anchor.<capture>` (`baseline`, `experiment`, or a named arm of a multi-recording `.rez`) | signed integer milliseconds, or `kind:<event kind>` | Compare mode only; each key is independent and `0` (no shift) is absent. The value names the instant, measured from that capture's start, that is drawn at `+0s`: `anchor.experiment=1500` puts the experiment's 1.5 s mark at the axis origin, shifting its trace 1.5 s to the left. `anchor.experiment=kind:run_start` uses the earliest `run_start` event in that capture's file instead; a capture with no such event is drawn from its recording start and the compare badge says so |

Values are ordinary query-string values: a `/` may be written as is, and
anything else (`&`, `=`, spaces) percent-encoded as a browser would. A
parameter in the URL wins over what the browser remembered for that key;
keys the URL does not name keep their remembered values, and the address
bar is then rewritten to include them (a remembered compare anchor, for
one), so a copied link carries the effective view. Granularity, pinned
percentiles and the heatmap toggle are not carried. A value the recording
cannot satisfy (a node it does not have, a range outside it, `time=raw` in
compare mode, `from`/`to` in live mode) is dropped with a warning in the
browser console and the link is rewritten without it. A granularity change
resets the window and drops `from`/`to`. Loading a different file resets
the view and clears these keys; on the static site a link therefore needs
its `capture=`, since a file dropped onto the page starts fresh. Everything
else in the query string (`capture=`, `compare=`) is kept, re-encoded as a
browser would.

## Recording tools

`rezolus recording` inspects and transforms `.parquet` files and `.rez`
archives. `parquet` remains a compatibility alias for the command. Operations
and flags differ by format; check `rezolus recording <subcommand> --help`.

- **Metadata** — inspect file-level and column-level metadata, geometry, and
  schema.
- **Annotate** — embed service extension KPI definitions for custom viewer
  dashboards.
- **Check** — evaluate the checks a recording's KPIs carry and report a
  verdict per check. See [Checks](#checks) below.
- **Combine** — merge a Rezolus parquet with service-level parquet files,
  joining on timestamps to produce a unified multi-source recording, or package
  captures as a multi-recording `.rez`. A/B tarballs are a legacy option.
- **Convert** — turn a raw msgpack recording (from `record -o out.raw`) into
  parquet. The input may be plain or zstd-compressed; which one it is is
  detected from the file's contents, not its name.
- **Filter** — drop parquet columns not referenced by service KPIs, or select
  samplers from a `.rez` archive.
- **Upgrade** — convert older tar-based `.rez` archives to the SQLite container.
  With `--to dendro`, write a copy as a dendro archive instead, the container
  rezolus 6.0 is planned to write. No current rezolus reads the result, so it
  is always a new file (`-o`, which must not exist), never a replacement.
- **Snapshot** — copy a live `.rez` consistently, including committed data in
  SQLite's sidecar. Prefer this over copying an active file with `cp`.

```bash
rezolus recording metadata -i rezolus.parquet
rezolus recording annotate rezolus.parquet --queries ext.json
rezolus recording annotate run.rez --event 'time=2026-09-28T14:03:11Z,kind=deploy,description=rollout'
rezolus recording annotate run.rez --event 'time=2026-09-28T14:00Z,duration=90s,kind=warmup,description=warm-up'
rezolus recording check run.rez --annotate
rezolus recording combine rezolus.parquet service.parquet -o combined.parquet
rezolus recording convert rezolus.raw.zst              # writes rezolus.parquet
rezolus recording filter rezolus.parquet -o slim.parquet
rezolus recording snapshot live.rez -o incident.rez
rezolus recording upgrade old.rez
rezolus recording upgrade --to dendro capture.rez -o capture.dendro
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

### Checks

A check is a threshold on a KPI. It lives on the KPI itself, as a `check`
object in the service-extension JSON that `recording annotate --queries`
embeds, so the recording carries what it is judged against. A complete file:

```json
{
  "service_name": "myservice",
  "aliases": ["my-service"],
  "service_metadata": {},
  "kpis": [
    {
      "role": "latency",
      "title": "p99 request latency",
      "query": "request_latency_seconds",
      "type": "histogram",
      "check": {"above": 0.25, "quantile": 0.99, "for": "10s", "severity": "fail"}
    },
    {
      "role": "throughput",
      "title": "Request rate",
      "query": "sum(rate(http_requests_total[10s]))",
      "type": "delta_counter",
      "unit_system": "rate",
      "check": {"below": 100, "severity": "warn"}
    },
    {
      "role": "queue",
      "title": "Queue depth",
      "query": "queue_depth",
      "type": "gauge"
    }
  ]
}
```

The file: `service_name` is required; `aliases` (other `source` names this
template matches) and `service_metadata` are optional. Each KPI needs `role`
(any word; it groups charts on the dashboard), `title` (keep it unique within
the file, since it names the check in the output and in events), `query`
(PromQL) and `type`, one of `gauge`, `histogram` or a counter kind
(`delta_counter`, `counter`); any other `type` is charted as a counter and
evaluated as written. `description`, `unit_system`, `subtype`, `percentiles`,
`subgroup`, `subgroup_description`, `full_width`, `denominator` and `check`
are optional. A KPI without `check` is a chart only.

The check:

- Exactly one of `above` / `below` is the threshold, compared in the query's
  own unit (seconds for the histogram above, requests per second for the
  rate). The comparison is strict: a value equal to the threshold passes.
- `for` is the shortest run that counts (default `0s`, so one point is
  enough). Accepted forms: `500ms`, `10s`, `1m30s`, `2h`, `1d`, `1.5s`, or a
  bare number of seconds.
- `severity` is `fail` (default) or `warn`.
- `quantile`, in (0, 1], is required on a histogram KPI: its query names the
  raw histogram and the check evaluates `histogram_quantile(<quantile>,
  <query>)` itself, so leave `histogram_quantile` out of the query. On any
  other type `quantile` is an error.
- Unknown keys inside `check` are rejected, so a misspelled `for` cannot
  become a check that never fires.

`rezolus recording check <file>` runs every check over the whole recording
through the query engine, the same path `rezolus mcp query` uses, and prints
one line per check followed by a summary:

```
PASS          Queue depth low  above 10
FAIL          p99 request latency  p99 above 0.25 for 10s  [2026-09-28T14:03:11Z..2026-09-28T14:03:41Z, 3 windows]
2 checks: 1 passed, 1 failed, 0 warned, 0 indeterminate, 0 errors
```

Which checks run: the KPIs embedded in the recording; else the built-in
template whose `service_name` or `aliases` match the recording's `source`
metadata (the same lookup the viewer makes), or the templates in
`--templates <DIR>` in place of the built-in set. `--queries kpis.json` runs
that file's checks instead of either and writes nothing to the recording.

How a check is evaluated: the query runs on a grid of one point per step,
where the step is the recording's sampling interval capped at 1 s, and must
yield exactly one series (aggregate with `sum(...)` or add label matchers if
it yields several). A query that matches nothing is an error, not a pass.
Each point is classified against the threshold. Where a value carries an
acquisition-window band (`rate()`, `irate()`, histogram quantiles), the band
is compared rather than the point: `above` fires only when the whole band is
above the threshold and `below` only when it is entirely below. A point
whose band straddles the threshold, or a point interpolated across a span the
recording never observed, is neither, and is classified `INDETERMINATE`.

The run rule: a run is consecutive points that do not pass. A passing point
ends it, and so does a gap of more than 1.5 steps between points, because the
engine emits no point where the recording has no data and `for` must not
count time nobody observed. A run violates only if every point in it
violates; one indeterminate point makes the whole run indeterminate. This is
deliberate: splitting on the straddle would turn 60 s of violation with every
tenth point straddling into runs of 9 s and 1 s, none reaching `for: 30s`,
and report a pass; instead it is one 60 s `INDETERMINATE` window. The step
is the evaluation grid step or the series' own point spacing (the median gap
between its points), whichever is coarser: a 10 s sampler evaluated on a 1 s
grid yields one point per 10 s, each spanning its 10 s, so the 10 s between
them is not a gap, while a real hole still is. A run's span is
`last point - first point + step`, and the run is a window when
`span >= for`. A check with any violating window is `FAIL` (or `WARN` by
severity) even if it also has indeterminate windows, which the line then
counts beside it; with only indeterminate windows it is `INDETERMINATE`;
otherwise `PASS`.

Exit status: `2` if any check could not be evaluated or the command itself
failed; else `1` if any check with severity `fail` failed; else `0`. `WARN`
and `INDETERMINATE` never fail the run. A recording with no checks exits `0`
with a note on stderr.

`--json` prints the verdicts as an array with one object per check:
`recording` (the recording's labels, `.rez` only), `title`, `query` (as
evaluated, so a histogram KPI's is wrapped in `histogram_quantile`), `check`
(as loaded, `for` as a duration string), `status` (`pass`, `warn`, `fail`,
`indeterminate`, `error`), `windows` and `indeterminate` (each entry has
`start` and `end` as RFC 3339, `start_ns`, `duration_ns` and `points`), and
`error` (present on `error` only).

`--annotate` writes each `FAIL`/`WARN` window into the recording as a range
event: `kind=check`, `timestamp` at the window start, `duration_ns` for its
span (so the event ends one step after the last violating point), the KPI
title as `description`, and `details` carrying the condition plus the check
JSON on a second line. That JSON is the version record: editing the template
later does not change what an old recording claims. The event id is a hash of
the title, the evaluated query, the condition and the window start, so
running the checks again overwrites a check event of the same id whose
content differs (a window that grew, or an event edited in the viewer),
adds nothing for one that did not change, and leaves a window that no longer
fires with its old event. Only a stored `kind=check` event is overwritten;
an event of another kind that carries the same id is kept.
The viewer draws these as shaded bands. On a multi-recording `.rez` every
recording is checked and each line is prefixed with the recording's labels;
with `--annotate` each recording's events go into that recording only.
`--recording k=v` narrows the run, and the write, to one recording. A v1/v2
(tar) `.rez` is rewritten to v3 (SQLite) in place, as `annotate` does. A
dendro archive is read-only here: its checks run, but `--annotate` is
refused. With `--json`, the annotation report goes to stderr so stdout stays
one JSON array.

## MCP Server

Exposes Rezolus recordings to LLM-based assistants over the Model Context
Protocol, with tools for querying metrics via PromQL, detecting anomalies, and
analyzing correlations — useful for AI-guided performance investigation. Runs as
a stdio MCP server or as one-shot CLI commands.

### Setup with Claude Code

```bash
rezolus mcp install                                    # user scope: every project
rezolus mcp install --scope project --export-dir ./exports   # this directory's .mcp.json
rezolus mcp install --dry-run                          # say what would be done
```

`install` registers this binary as the `rezolus` server through `claude mcp
add` and installs the `rezolus-mcp` skill (`~/.claude/skills/rezolus-mcp/` or
`.claude/skills/rezolus-mcp/`), which carries the workflow: describe the
recording and its metrics before querying, extract features before forming
hypotheses, the recording selector, and what to write back. In Claude Code,
`/mcp` lists the server and `/rezolus-mcp` loads the skill; `claude mcp list`
checks the connection from a shell. Re-running replaces the entry, which is
how the server's flags (`--allow-mutating`, `--export-dir`, `--viewer-url`,
all accepted by `install`) are changed. Without `claude` on `PATH`, project
scope writes `.mcp.json` directly and user scope prints the command to run.
A skill file that is not this skill, or a symlink, is never overwritten.
Claude Code resolves a server name local, then project, then user; when
another scope's `rezolus` entry would win, `install` says so and prints the
`claude mcp remove` command. A project-scope server is approved in Claude
Code the first time it opens in that directory.

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

The six read tools are `describe_recording`, `describe_metrics`,
`extract_features`, `detect_anomalies`, `analyze_correlation`, and `query`.
CLI names use hyphens. For an investigation, describe the recording and metrics
first, confirm the source and time range, extract features, then query specific
hypotheses. Missing metrics are not zero, and correlations do not establish
causation.

### Write tools

The stdio server also writes, in two tiers decided by what a tool can
destroy. The additive tools are always on: the worst a wrong call does is add
an event.

- `add_event` marks an instant or a range in the recording. It takes
  `timestamp` (RFC 3339, or Unix seconds as a JSON number; a digit-only
  string is refused as ambiguous), `description`, and optionally `kind`,
  `duration` (`30s`, or seconds as a number), `details`,
  `node`, `instance` and `id`. `source` defaults to `mcp`, so agent-written
  events can be filtered or removed as a group later. The event lands through
  the same manifest update `recording annotate --event` uses and the viewer
  draws it on the next open. Adding an event whose `id` is already present is
  a no-op (a `kind=check` event replaces a stored check event with the same
  id, so a verdict that grew is rewritten); the reply carries the id (minted
  as `mcp:<uuid>` when not given).
- `run_checks` evaluates the recording's KPI checks, the `check` blocks that
  `recording annotate --queries` embeds, or a ServiceExtension object passed
  as `queries`, and returns every verdict with its violation windows and a
  summary. With `annotate: true` the windows are written into the recording
  as `kind=check` events, exactly as `rezolus recording check --annotate`.
- `export_query` runs a PromQL range query over the whole recording and
  writes the rows as CSV (default) or parquet, long form: one row per series
  and timestamp with columns `series`, `timestamp` (Unix seconds), `value`,
  `lo`, `hi` (the `rate()` uncertainty band, else empty). It is the way out
  of PromQL: the agent gets rows it can load into whatever it has. Files
  land only under the directory the server was started with, under a bare
  `filename` (or one derived from the query), never over an existing file,
  and never through a symlink planted there. A result over a million rows is
  refused with the count and a hint to raise `step` or aggregate:

  ```bash
  rezolus mcp --export-dir /tmp/rezolus-exports
  ```

  Without the flag the tool refuses every call, naming it.
- `viewer_link` builds a link that opens a running viewer at a `section` (or
  one `chart_id`) with the view state set: `from`/`to`, `time`, `node`,
  `gpu`, `cgroup`, `instance`, `family` and compare `anchors`, the same keys
  as "Linking to a view". A service section is `service/<name>`. It opens
  nothing and reads no recording. The reply carries the hash fragment and
  the query string; with `viewer_url` in the call, or `rezolus mcp
  --viewer-url http://127.0.0.1:4200`, it carries a full URL to hand to the
  person. The address must be `http://` or `https://` with no fragment; one
  that already has a query (the static site's `?capture=demo`) gets the view
  keys appended to it.

The mutating tools can take another person's events out of a shared
recording, so they are off unless the operator starting the server says
otherwise:

```bash
rezolus mcp --allow-mutating
```

- `remove_events` drops events by `ids`, `kind` and/or `source` (every given
  field must match; `source: "mcp"` removes everything an agent wrote). An
  empty filter is refused, since clearing every event is `recording annotate
  --clear-events`, typed by a person.

Without the flag `remove_events` is neither listed nor callable; a call
answers with the flag's name. On a multi-recording `.rez`, `add_event` and
`remove_events` need the `recording` selector below and refuse to write to
every arm; `run_checks` without a selector evaluates every recording and
gives each its own verdicts, as `recording check` does. The reply of a
write carries the writer's report line, which is where a v1/v2 tar archive
says it was upgraded to v3 on the way.

### Multi-recording archives

A `.rez` built from several endpoints (see "Several endpoints in one archive"
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
stdio server's tools, read and write alike, take the same selector as an
optional `recording` object instead of repeated flags, e.g.
`{"source": "valkey"}`.


## HTTP endpoint (optional)


Hindsight can optionally expose an HTTP endpoint for remote buffer management.
Enable it by adding a `listen` address to the configuration:

```toml
listen = "127.0.0.1:4242"
```

Available endpoints:

- `GET /status` — returns buffer status: the time range actually retained, rows
  and segments per sampler, on-disk size, and whether retention has started
- `GET /dump` — downloads the buffer as an archive, in the buffer's format
- `POST /dump/file` — writes the buffer to the configured output file

The `/dump` and `/dump/file` endpoints support query parameters for time
filtering. An archive segment is an immutable parquet blob, so a filtered dump
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

# download last 5 minutes as a .dendro archive
curl -o dump.dendro "http://localhost:4242/dump?last=5m"

# download a specific time range using RFC 3339 datetime
curl -o dump.dendro "http://localhost:4242/dump?start=2024-01-01T12:00:00Z&end=2024-01-01T13:00:00Z"

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
`.dendro` default. Run `scripts/rezolus-capture --help` for its full options.

## Docker

```bash
docker run --rm -it --privileged \
  -p 8080:8080 -v "$(pwd)/data:/data" \
  ghcr.io/iopsystems/rezolus:latest \
  rezolus-capture --duration 60s
```

See [Docker usage](../docker/README.md) for system and service captures.
