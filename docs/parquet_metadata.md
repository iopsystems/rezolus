# Recording Metadata: Parquet Footer and `.rez` Manifest

Rezolus recordings carry metadata in one of two places depending on format:

- **`.parquet`** — key/value metadata in the single file's parquet footer.
  The canonical key list lives in
  [src/parquet_metadata.rs](../src/parquet_metadata.rs); most of this doc
  describes those keys.
- **`.rez`** — a tar archive (v1/v2: one recording per directory, one parquet
  table per sampler, metadata in a top-level `manifest.json`) or a SQLite
  container (v3: a `recordings` table holding each recording's labels and
  metadata). See [`.rez` archives](#rez-archives-metadata-lives-in-the-manifest)
  below for the mapping; the reader is metriken-storage's `rez` module
  (`rez::rez` for the tar container, `rez::rez_sqlite` for v3), which
  `crates/rez` re-exports.

The viewer, MCP, and downstream tools rely on this metadata to interpret the
data, distinguish recordings, build dashboards, and combine files.

**Rezolus 6.x reads both formats and writes neither.** It writes `.dendro`
archives, where each recording (a source in the archive's catalog) carries
the same keys in its own metadata map, and its identity in its labels. A
`recording` command that sets a key (`annotate`, `check --annotate`) edits a
`.dendro` in place; given a `.rez` or `.parquet` it needs `-o <new>.dendro`,
converts the recording there and edits the copy, and never changes the
original. This document describes the keys as 5.x wrote them, for reading
those files; where it shows how a key is set, the command is 6.x's unless it
says a 5.x binary.

This document covers *file-level* metadata. For per-column field metadata —
which keys are storage keys, which become series labels, and the `__` rule for
internal labels — see [labels.md](labels.md).

## Inspecting metadata

```bash
# Human-readable file-level metadata
target/release/rezolus recording metadata -i file.parquet --file

# Full JSON (includes nested per-source metadata, parsed)
target/release/rezolus recording metadata -i file.parquet --json

# Pull a single key (auto-pretty-prints if the value is JSON)
target/release/rezolus recording metadata -i file.parquet --field source
target/release/rezolus recording metadata -i file.parquet --field per_source_metadata

# .rez: describes the MANIFEST (recordings, labels, per-sampler tables +
# cadence). --file/--field don't apply — there is no single footer.
target/release/rezolus recording metadata -i file.rez
target/release/rezolus recording metadata -i file.rez --json   # full manifest JSON
```

## `.rez` archives: metadata lives in the manifest

The `.rez` format (metriken-storage's `rez` module, read through
`crates/rez/src/reader.rs`) is the 5.x recorder's per-sampler archive:
`manifest.json` plus `<dir>/<sampler>.parquet` tables per recording in the
tar container, the same maps in the v3 container's `recordings` table. A
recording is one endpoint on one host. A 5.x `.rez` holds a rezolus agent's
recordings and, from 5.19.1, Prometheus endpoints' (`prometheus/scrape`
groups), so `source` is whatever each endpoint was named.

Each manifest recording carries two maps:

- **`labels`** — the recording's identity for grouping/aliasing:
  `source` and `host` (auto-populated; `host` from the agent's systeminfo
  hostname) plus any `record --label k=v` (last wins). The viewer's A/B
  aliases baseline/experiment from `arm`/`host` labels. This replaces the
  parquet-combine `node`/`instance`/`pinned_node` machinery — `.rez` has no
  column renaming and no pinned node; label sets distinguish recordings.
- **`metadata`** — mirrors the keys the parquet writer would put in a
  footer: `sampling_interval_ms`, `source`, `systeminfo`, `descriptions`,
  `version` (see [`version`](#version)), plus any `record --metadata k=v`.
  Per *recording*, not per file, so a multi-recording archive assembled from
  several hosts or several agent builds stays unambiguous.

Structural differences from a single parquet file:

- **Per-sampler cadence.** Each table records at its own rate and carries
  `cadence_ns` in its manifest index — the recording-level
  `sampling_interval_ms` is the agent snapshot interval, not a promise about
  every table (e.g. a throttled expensive sampler runs slower).
- **Acquisition-window sidecars.** Every metric column has
  `<metric>:window_begin` / `<metric>:window_width` companions; the query
  engine consumes them for `rate()`/`irate()` uncertainty bounds and readers
  must not treat `:window_*` columns as metrics.
- **Reader-stamped (packed cgroup/task) windows are exposition-derived, not
  a staleness claim.** For mmap-direct cgroup/task counters
  (`task_cpu_usage`, the `cpu_bandwidth` cgroup maps, and similar
  `PackedCounters`-backed metrics), the window brackets the snapshot
  builder's own read of the value out of the BPF map, not any property of when the underlying BPF
  counter last changed — an artifact of how the value is exposed. It is a
  deliberate upper bound on the read span, honest in the same "never
  under-states uncertainty" direction as every other acquisition window,
  and (since the builder marks the window's end immediately after reading
  a group's members, not at eventual publish time) reflects the group's
  own read span rather than whole-walk time — not a claim that the
  counter is only that fresh. Spans vary by group: sparse task metrics on
  the v2 format include a capacity walk and read millisecond-scale. The
  other per-cgroup counters (`CgroupCounters`) are summed in the sampler's
  refresh, and their window brackets that sum.
- **Raw timestamps survive combining.** `recording combine` converts a
  `.rez` input to dendro and copies its rows unchanged, so windows and
  sampling-jitter fidelity are preserved.

What 6.x's `recording` commands do with a `.rez`:

| Command | `.rez` behavior |
|---------|-----------------|
| `recording metadata` | Describes the recordings, their labels and metadata, and each table (`--json` for JSON); `--file`/`--field` don't apply. |
| `recording annotate` | Needs `-o <new>.dendro`: the archive is converted there and every recording of the copy gets the KPIs (`--queries`, required, or `--undo`), the events and `--systeminfo`. `--source`, `--node` and `--filter` are refused. |
| `recording combine` | Converts each `.rez` input to dendro and copies its recordings into one `.dendro`, labels unchanged (`--ab` adds `arm`). |
| `recording filter` | Needs `-o <new>.dendro`; keeps the samplers (`--samplers`) or metrics (`--metrics`) named. |

## Single-source vs combined files

A "single-source" file comes from one recording (one rezolus agent, or one
service endpoint). A "combined" file was produced by a 5.x `parquet combine`
and may hold multiple rezolus nodes and/or multiple service instances.

Several keys live at the **top level** in single-source files but get nested
under [`per_source_metadata.<source>.<sub_key>`](#per_source_metadata) in
combined files. Where applicable this is called out below.

This single/combined nesting — and the `node`/`instance`/`pinned_node`
machinery it supports — is a **`.parquet`-only** concept. A `.rez` never
nests: each recording keeps its own `labels`/`metadata` maps in the manifest,
and combining appends recordings (see
[`.rez` archives](#rez-archives-metadata-lives-in-the-manifest)).

## Top-level keys

### `source`

Identifies the recording's source(s).

- Single-source file: a flat string, e.g. `"rezolus"`, `"llm-perf"`, `"vllm"`,
  `"sglang"`.
- Combined file: a JSON array of source names, e.g. `["rezolus","vllm"]`
  (deduplicated and sorted).

**Set at record time:** Inferred automatically from the endpoint type; can
also be overridden via `--metadata source=<name>`. For non-rezolus endpoints
the source is what you tell it (or what's inferred from labels); the viewer
picks a built-in KPI template by it.

**Not set after recording in 6.x.** A 5.x `recording annotate --source NAME`
(`--overwrite` to replace one) rewrote this key. In a `.dendro` the source is
a label of the recording, set when it is recorded (`record --endpoint
URL,source=NAME`), and 6.x does not relabel a recording: `annotate --source`
is refused. A parquet file's `source` becomes its recording's `source` label
when it is converted (`recording upgrade`, or any edit given `-o`).

**In a `.rez`** the source appears both as a recording label and in the
recording's `metadata` map; it is always `rezolus` (the format required a
rezolus/msgpack endpoint).

### `version`

Agent/tool version string of the source that produced this file. Single-source
only — when files are combined this moves to
`per_source_metadata.<source>.<id>.version`.

**Set at record time** by the recorder, from the *agent it scraped* — not from
the recorder's own build. The two are separate processes and routinely
different versions, and the question this answers ("which build produced these
numbers?") is about the agent. The recorder reads it from the agent's `/status`
endpoint, falling back to the `Rezolus <version> Agent` banner on `/` for
agents older than `/status` (5.16.0). `record --metadata version=...`
overrides it. When the agent restarts mid-recording, an archive's `version`
becomes the new process's (see [`producer_epochs`](#producer_epochs)), unless
it was set with `--metadata`.

**Absent, rather than empty, when there is nothing to record**: a Prometheus
source has no agent, and recorders that predate the capture wrote no version
at all. `mcp describe-recording` renders those as `unknown`.

### `producer_epoch`

An opaque id — a v4 UUID — minted once per agent **process**. It changes when,
and only when, every cumulative counter in the recording restarted from zero
together, which for a process-scoped producer is once per process.

**Why it is needed.** From the values alone, a counter that was *reset* and one
that *wrapped* are identical — both went down — while their arithmetic is not
(`cur` versus `cur + (2^w - prev)`). Every consumer here assumes reset, which is
right for a restart and silently wrong for an overflow. This settles the restart
half for every counter at once. There is also a case no value-based heuristic
can catch: a counter reset to zero that counts past its previous value before
the next observation shows **no drop at all**, and the interval silently
undercounts. An epoch change is visible either way.

**What it does not cover.** A single counter that wrapped, or that a sampler
zeroes on read, did not restart the process — so this key says nothing about it.
That needs a generation per counter, which is row data rather than file
metadata.

**Set at record time** from the agent's `/status`, or, for a recording taken
off the agent's stream, from the stream handshake, which names the process
whose rows arrive. It is re-checked against every scraped snapshot's metadata
and every stream reconnect's handshake, which is how a restart *between* two
readings is caught. Absent for a Prometheus source, and for an agent old enough to predate
it (the `/` banner fallback deliberately yields no epoch rather than inventing a
restart boundary nobody observed).

Name and semantics follow dendro's `keys::PRODUCER_EPOCH`, so an archive written
here reads the same to any consumer of that format.

**When the agent restarts mid-recording**, an archive (`.dendro` or `.rez`,
written by `record` or by `hindsight`) is amended once the first row from the
new process is staged: `producer_epoch` becomes the new epoch,
[`producer_epochs`](#producer_epochs) gains an entry, and `version`,
`systeminfo` and `descriptions` are read again from the new process. A parquet
recording writes its metadata once, when it closes: `producer_epoch` is the
last epoch seen, `version`, `systeminfo` and `descriptions` are what the agent
reported when the recording opened, and there is no history; the recorder's
warning naming both epochs is the record of the restart.

### `producer_epochs`

Every producer epoch an archive observed, in order, as a JSON array:

```json
[
  {"epoch": "0b1c…", "from_ts": 1789425944000000000, "version": "6.0.0"},
  {"epoch": "7f3e…", "from_ts": 1789426011000000000, "version": "6.0.1"}
]
```

`from_ts` is the timestamp of the first row from that process; every cumulative
counter reset there. The last entry is the current epoch, also under
`producer_epoch`. Written only when a restart is seen: absent means the archive
saw one process. The first entry is the epoch the archive opened with, from its
first row; it is left out when the agent restarted before that epoch's first
row was recorded. `version` is what the process reported. It is missing from
an epoch that came and went before its metadata could be fetched, from one
whose metadata could not be read (three tries, one tick apart, each checked
against the epoch `/status` reports), and from the opening epoch when
`version` was set with `--metadata`.

Name, shape, `epoch` and `from_ts` follow dendro's `keys::PRODUCER_EPOCHS`;
`version` is rezolus's addition.

After a restart, `version` and `systeminfo` describe the newest process, and
`descriptions` is the union of every process's descriptions (the newest wins
for a metric in both), so the rows from before the restart keep theirs. A
`version` set with `record --metadata` is kept.

### `sampling_interval_ms`

Collection interval in milliseconds, written as a decimal string (e.g.
`"1000"`).

A 5.x `recording combine` of parquet files required it to be **identical
across all inputs**, and used it to quantize timestamps to a common grid. 6.x
combines into a `.dendro` without either: each recording keeps its own
interval and timestamps.

**Set at record time** via the `--interval` flag on `rezolus record`.

**Viewer assumption — the sampling-jitter chart.** In the simple-capture metric
browser, selecting the synthetic `timestamp` row charts the inter-sample delta;
its "deviation" (jitter) mode needs a nominal cadence to subtract. The viewer
prefers this declared `sampling_interval_ms` — that's the *intended* interval, so
a producer consistently running behind intent shows a steady non-zero offset. But
foreign producers don't always declare it honestly: some hardcode a default (e.g.
`1000`) while actually sampling far faster. The viewer therefore trusts the
declared value **only when it's plausible** — at or below the observed average
interval, since a real target can't be slower than the cadence you actually
achieve. A declared value well above the average (more than 2×) is treated as a
bogus default and the baseline falls back to the average of the real intervals.
Same fallback when the field is absent. See `nominalMsFor` in
`src/viewer/assets/lib/charts/jitter.js`.

**Raw vs. grid-snapped timestamps.** Jitter is only observable on the raw
`timestamp` column. The viewer's charts are evaluated on a step grid
phase-fixed to step boundaries (`docs/rate-time-mode-design.md`), so anything
read through a query — including a hypothetical synthesized inter-sample-delta
metric — is structurally flat and cannot express jitter. An expanded chart's
SHOW SAMPLES (`rate_mode=raw`) returns points at the acquisition timestamps,
but the jitter chart bypasses the query path, reading raw
timestamps directly via `MetricsSource::sample_timestamps()` (served as
`/api/v1/timestamps`) — the direct path works in every mode and without a
rate query. Note also that a **combined parquet file** (from 5.x) had its
timestamps quantized to the common grid **on disk**, so the jitter signal is
gone from it whatever the time mode; run sampling-jitter analysis against the
original single-source file. A 6.x `recording combine` copies rows unchanged,
so raw timestamps and acquisition windows survive.

**In a `.rez`** this key sits in each recording's manifest `metadata` map and
describes the agent snapshot interval; individual sampler tables additionally
carry their own `cadence_ns`, which can differ (throttled samplers).

### `systeminfo`

JSON-serialised hardware summary fetched from the rezolus agent's
`/systeminfo` endpoint. Display-only — used by the viewer and MCP to render
the host summary panel.

In combined files, the *first* rezolus node's value is kept at the top level
for viewer compatibility. A copy of each node's `systeminfo` is stashed into
`per_source_metadata.rezolus.<node>.systeminfo` so multi-node combined files
don't lose per-host data.

**Set at record time** by the recorder when scraping a rezolus endpoint.

**Set or replace post-recording with `recording annotate --systeminfo`,**
which stores it in every recording of a `.dendro`:

```bash
# From a file
target/release/rezolus recording annotate run.dendro --systeminfo sysinfo.json

# Or piped from a live agent
curl -s http://agent:4241/systeminfo | \
    target/release/rezolus recording annotate run.dendro --systeminfo -

# A parquet file is not changed: the result is a new .dendro
target/release/rezolus recording annotate file.parquet --systeminfo sysinfo.json -o file.dendro
```

The value is validated as JSON before being written.

### `descriptions`

JSON map of metric name → help text, used by `mcp describe-metrics` and the
viewer tooltip layer. In combined files this is union-merged across all
inputs (first writer wins on conflicts).

**Set at record time** by the recorder: for a Rezolus agent, fetched from
`/metrics/descriptions`; for a Prometheus endpoint, the scrape's `# HELP`
lines, first seen wins, written once a scrape brings a metric name not seen
before. Not user-editable.

### `selection`

JSON snapshot of a viewer's selection/filter state — what charts were
expanded, what time range was zoomed in, what pins were placed, etc. Optional;
only present if the viewer wrote it back. Combined files preserve the value
from the primary (rezolus) input.

**Set by the viewer** when the user saves a selection. Not normally
user-editable, but can be cleared by re-writing the file without it.

### `service_queries`

JSON document containing a `ServiceExtension` — the KPI dashboard definition
the viewer uses to render the "Service" section for non-rezolus sources.
Schema lives in [src/viewer/service_extension.rs](../src/viewer/service_extension.rs);
templates live in [src/parquet_tools/templates/](../src/parquet_tools/templates/).

In combined files this moves under
`per_source_metadata.<source>.<id>.service_queries` so each instance can
carry its own KPI definitions.

**Update with `recording annotate`:**

```bash
# Embed a KPI file into every recording of an archive
target/release/rezolus recording annotate run.dendro --queries my_kpis.json

# Remove the KPIs
target/release/rezolus recording annotate run.dendro --undo

# A parquet file or .rez is not changed: the result is a new .dendro
target/release/rezolus recording annotate file.parquet --queries my_kpis.json -o file.dendro

# Drop what the KPIs don't touch (a parquet file with neither --samplers
# nor --metrics keeps what its KPIs need)
target/release/rezolus recording filter file.parquet -o slim.dendro
```

Annotation validates each KPI by running its PromQL query against each
recording's data and sets `available: true|false` on each KPI accordingly.
`--queries` is required: the 5.x flow that picked a built-in template from a
parquet file's `source`, and `annotate --filter`, are gone (`--filter` is
refused, naming `recording filter`).

### `node`

Hostname/VM identifier for rezolus agent data. In combined files this lives
under `per_source_metadata.rezolus.<node>.node`.

A 5.x parquet `recording combine` required every rezolus input to have a
unique node label, and used the filename stem when the key was missing. 6.x
combines into a `.dendro`, where recordings are told apart by their labels.

**Set at record time.** A parquet recording comes from a 5.x recorder; 6.x
writes `.dendro`, where the same `--metadata node=web01` sets the key in each
recording's metadata:

```bash
# with a 5.x binary
target/release/rezolus record --url http://localhost:4241 -o web01.parquet --metadata node=web01
# with this one
target/release/rezolus record --url http://localhost:4241 -o web01.dendro --metadata node=web01

# (`record --node` was removed: it was never wired to anything. `--metadata
# node=NAME` writes the same `node` key, which is what combine reads.)
```

**Not set after recording in 6.x.** A 5.x `recording annotate --node NAME`
rewrote this key in a parquet footer; 6.x refuses `--node`, since a
recording's identity is its labels, set when it is recorded (`record
--endpoint URL,host=NAME` or `--label`).

### `instance`

Process/container identifier for service data (vllm, llm-perf, sglang, ...).
In combined files this lives under
`per_source_metadata.<source>.<instance>.instance`.

A 5.x parquet `recording combine` of several files of one source required
either every file to carry an `instance` label (unique within the source) or
none (it assigned `"0"`, `"1"`, ... in input order).

**Set at record time** (a 5.x recorder for parquet; 6.x sets the same key in a
`.dendro` recording's metadata):

```bash
# with a 5.x binary
target/release/rezolus record --url http://vllm-host:8000/metrics -o primary.parquet --metadata instance=primary
```

### `pinned_node`

The default rezolus node the viewer should focus on when opening a combined
file with multiple nodes. Only meaningful in combined files.

**Set at combine time by a 5.x binary** (`recording combine ... -o
combined.parquet --pinned web01`, which rejected a name no rezolus input's
`node` matched). 6.x refuses `--pinned`: it applied to the parquet row-merge,
which 6.x does not write.

### `events`

JSON document describing one-off events attached to the recording —
restarts, config changes, deploys, incidents, anomalies, free-form
markers. Stored at the top level in both single-source and combined files
because each event is self-describing (it carries its own optional
`source` / `node` / `instance` scope) rather than inheriting from
file-level metadata. Schema lives in
[crates/dashboard/src/events.rs](../crates/dashboard/src/events.rs).

Payload shape (canonical, sorted by `timestamp`, deduped by `id`):

```json
{
  "events": [
    {
      "timestamp": 1778598000000000000,
      "description": "vllm restart for new config",
      "kind": "restart",
      "details": "swapped tensor_parallel_size 2 → 4",
      "source": "vllm",
      "node": "gpu01",
      "instance": "0",
      "labels": { "reason": "OOM" },
      "duration_ns": 30000000000,
      "id": "evt-2026-05-12-001"
    }
  ]
}
```

Only `timestamp` (u64 nanoseconds since the Unix epoch) and `description`
are required. `kind` is free-form — conventional values are `restart`,
`config_change`, `deploy`, `incident`, `anomaly`, `marker`, `note`, but
nothing is validated. `duration_ns` lets an event span an interval rather
than a point; `id` is a stable identifier used to dedupe across merges
(later occurrences are dropped on combine).

**Update with `recording annotate`:**

```bash
target/release/rezolus recording annotate run.dendro --add-events events.json

cat events.json | target/release/rezolus recording annotate run.dendro --add-events -

target/release/rezolus recording annotate run.dendro \
    --event 'time=2026-05-12T15:23Z,kind=restart,description="vllm restart",node=gpu01' \
    --event 'time=2026-05-12T16:00Z,kind=marker,description="benchmark start",label.run=ci-42'

target/release/rezolus recording annotate run.dendro --clear-events

target/release/rezolus recording annotate run.dendro --clear-events --add-events new.json

# A parquet file or .rez is not changed: the result is a new .dendro
target/release/rezolus recording annotate file.parquet --add-events events.json -o file.dendro
```

- `--add-events FILE` accepts JSON (`{"events":[...]}`), a bare JSON array,
  a single JSON object, or JSONL. Use `-` to read from stdin.
- `--event KV` is a repeatable inline shorthand
  (`key=value,key=value,...`). Quoted values may contain commas and `=`.
  Free-form labels go through `label.<name>=<value>`. Aliases: `time`,
  `desc`, `duration` (alongside the canonical `timestamp`, `description`,
  `duration_ns`).
- `--clear-events` wipes existing events. Combine with `--add-events` /
  `--event` to "replace": order within one invocation is **clear → file
  events → inline events**.
- Operations are append-by-default. Events are sorted by timestamp on
  write; entries whose non-empty `id` collides with an earlier one are
  dropped (a warning is printed when this happens during an annotate
  invocation).
- Inputs accept RFC3339 timestamps (including the seconds-omitted short
  form `2026-05-12T15:23Z`) and humantime durations (`30s`, `1m30s`);
  canonical storage is always `u64` nanoseconds.

In a `.dendro` each recording keeps its events in its own metadata. A
recording read from several sources (a source and its snapshot, which the
readers present as one recording) shows the union of their events, deduped
by `id`; `remove_events` drops an event from each source's own payload.

## `per_source_metadata`

Top-level key only used in combined files. Value is a nested JSON object:

```json
{
  "rezolus": {
    "web01": { "version": "5.8.3", "node": "web01", "systeminfo": {...} },
    "web02": { "version": "5.8.3", "node": "web02", "systeminfo": {...} }
  },
  "vllm": {
    "0": {
      "version": "0.6.0", "instance": "0", "role": "service",
      "service_queries": {...}, "first_sample_ns": 1700000000000000000,
      "last_sample_ns": 1700000300000000000
    }
  }
}
```

The recorder writes a partial `per_source_metadata` even for single-source
files so it can stash per-source `first_sample_ns`, `last_sample_ns`, and
`role` without polluting the top level.

### Nested keys

| Key | Meaning |
|-----|---------|
| `version` | Agent/tool version that produced this source's data. |
| `service_queries` | `ServiceExtension` JSON for this source's KPI dashboard. |
| `role` | `"service"` (system under test) or `"loadgen"` (benchmark client). |
| `node` | Host where this source ran (rezolus: identity; service: informational). |
| `instance` | Service instance identifier within the source group. |
| `systeminfo` | Per-node hardware summary (rezolus only, populated by combine). |
| `first_sample_ns` | Nanosecond timestamp of the first successful scrape. |
| `last_sample_ns` | Nanosecond timestamp of the last successful scrape. |

## Editing metadata after the fact

There is no general-purpose "set arbitrary file-level key" CLI. The supported
mutators are:

| Tool | What it can change |
|------|--------------------|
| `rezolus record --metadata k=v` | Anything written at recording time; `--metadata` sets any top-level key, `node` and `instance` included. (The dedicated `--node`/`--instance` flags were removed — they never wrote anything.) |
| `rezolus recording annotate` | In each recording of a `.dendro`: adds/replaces `service_queries` (`--queries`) or removes it (`--undo`), sets/replaces `systeminfo` (`--systeminfo PATH`, or `-` for stdin), and appends, inserts or wipes `events` (`--add-events PATH` / `--event KV` / `--clear-events`). |
| `rezolus recording combine --ab` | Adds the `arm` label to each input's recordings (`--ab baseline=a.dendro experiment=b.dendro`), or, with one archive input, to the recordings at the two positions `rezolus mcp describe-recording` prints (`--ab baseline=0 experiment=1`; every source of a recording, such as a source and its snapshot, takes its arm). |

6.x has no tool that edits a `.rez` or a parquet file.

## Example: a 5.x `parquet combine`

A worked example showing what a 5.x `recording combine` of parquet files
moved where, for reading the combined files it wrote (6.x combines into a
`.dendro` instead and writes no combined parquet). We start with three
single-source recordings — two rezolus agents on different hosts and one
vllm service instance — and combine them.

### Inputs

**`web01.parquet`** — produced by a 5.x
`rezolus record --url http://web01:4241 -o web01.parquet --metadata node=web01`:

```json
{
  "source": "rezolus",
  "version": "5.8.3",
  "sampling_interval_ms": "1000",
  "node": "web01",
  "systeminfo": "{\"cpu\": \"...\", \"memory\": \"...\"}",
  "descriptions": "{\"cpu_usage\": \"...\", ...}",
  "per_source_metadata": "{\"rezolus\": {\"web01\": {\"role\": \"service\", \"first_sample_ns\": 1700000000000000000, \"last_sample_ns\": 1700000300000000000}}}"
}
```

**`web02.parquet`** — same shape, with `node: "web02"` and its own
`systeminfo`/`per_source_metadata`.

**`vllm.parquet`** — produced by a 5.x
`rezolus record --url http://vllm:8000/metrics -o vllm.parquet --metadata instance=primary --metadata role=service`,
then annotated with `rezolus recording annotate vllm.parquet`:

```json
{
  "source": "vllm",
  "version": "0.6.0",
  "sampling_interval_ms": "1000",
  "instance": "primary",
  "descriptions": "{\"tokens\": \"...\", ...}",
  "service_queries": "{\"service_name\": \"vllm\", \"kpis\": [...]}",
  "per_source_metadata": "{\"vllm\": {\"primary\": {\"role\": \"service\", ...}}}"
}
```

### Command (5.x)

```bash
target/release/rezolus recording combine \
    web01.parquet web02.parquet vllm.parquet \
    -o combined.parquet \
    --pinned web01
```

### Inspecting the result

```bash
# Top-level keys only
target/release/rezolus recording metadata -i combined.parquet --file

# Full structured view (parses nested JSON values)
target/release/rezolus recording metadata -i combined.parquet --json

# Drill into a single key
target/release/rezolus recording metadata -i combined.parquet --field source
target/release/rezolus recording metadata -i combined.parquet --field per_source_metadata
target/release/rezolus recording metadata -i combined.parquet --field pinned_node
```

### Output: `combined.parquet`

```json
{
  "source": "[\"rezolus\",\"vllm\"]",
  "sampling_interval_ms": "1000",
  "systeminfo": "{\"cpu\": \"...\", ...}",
  "descriptions": "{\"cpu_usage\": \"...\", \"tokens\": \"...\", ...}",
  "pinned_node": "web01",
  "per_source_metadata": "{
    \"rezolus\": {
      \"web01\": {
        \"version\": \"5.8.3\",
        \"node\": \"web01\",
        \"role\": \"service\",
        \"systeminfo\": {\"cpu\": \"...\", ...},
        \"first_sample_ns\": 1700000000000000000,
        \"last_sample_ns\": 1700000300000000000
      },
      \"web02\": {
        \"version\": \"5.8.3\",
        \"node\": \"web02\",
        \"role\": \"service\",
        \"systeminfo\": {\"cpu\": \"...\", ...},
        ...
      }
    },
    \"vllm\": {
      \"primary\": {
        \"version\": \"0.6.0\",
        \"instance\": \"primary\",
        \"role\": \"service\",
        \"service_queries\": {\"service_name\": \"vllm\", \"kpis\": [...]},
        ...
      }
    }
  }"
}
```

### What changed, key by key

| Key | Behavior |
|-----|----------|
| `source` | Promoted from flat string → JSON array, deduplicated and sorted: `["rezolus","vllm"]`. |
| `version` | **Removed from top level.** Each input's version is moved under `per_source_metadata.<source>.<id>.version`. |
| `sampling_interval_ms` | Passed through unchanged. Combine refuses to run if inputs disagree, so this value is shared. |
| `systeminfo` | The first rezolus input's value is kept at the top level (viewer compatibility). Each rezolus node's `systeminfo` is also copied into `per_source_metadata.rezolus.<node>.systeminfo`. |
| `descriptions` | Union-merged across all inputs. First writer wins on key conflicts. |
| `node` | **Removed from top level.** Moves to `per_source_metadata.rezolus.<node>.node`. |
| `instance` | **Removed from top level.** Moves to `per_source_metadata.<source>.<instance>.instance`. |
| `service_queries` | **Removed from top level.** Moves to `per_source_metadata.<source>.<id>.service_queries` so each instance can carry its own KPI definitions. |
| `pinned_node` | New — added by `--pinned web01`. Validated against the actual rezolus nodes seen in the inputs. |
| `selection` | Preserved from the primary (rezolus) input if present; otherwise dropped. |
| `events` | Concatenated across all inputs, sorted by timestamp, deduped by stable `id`. Stays at the top level — each event is self-describing via its own `source`/`node`/`instance` fields. |
| `per_source_metadata` | Deep-merged. Pre-existing entries from already-combined inputs are preserved; new sub-entries are added per node/instance. |

Schema changes alongside the metadata: every metric column is renamed to
`<node-or-instance>::<metric>` (e.g. `web01::cpu_usage`, `primary::tokens`)
with `node`/`instance`/`source` labels added to the column-level metadata.
The `timestamp` and `duration` columns come from the first rezolus input
(or first input if none is rezolus) and are not prefixed.

## Writer-side knobs (not metadata, but related)

These are file-format settings every 5.x rezolus parquet writer applied.
They're not in the KV map but matter when producing parquet files of your own:

- **Row group size:** `MAX_ROW_GROUP_SIZE = 50_000` (matches
  `metriken-exposition`'s default). All rezolus tools enforce this so
  combined files don't end up with one giant row group.
- **Compression:** ZSTD on every column.

If you write parquet from another tool that you intend to read with rezolus
or feed into `recording combine`, match these settings.

## Histogram columns: required column metadata

A histogram column (`<metric>:buckets`, Arrow type `List<UInt64>`) MUST carry
`grouping_power` and `max_value_power` in its column-level metadata, both
parseable as `u8`. If either is missing or unparseable, `metriken-query`'s
parquet reader **silently drops the entire column** from the schema — no
error, no warning; the histogram simply never appears to queries.

The rezolus agent's exposition path stamps these automatically. Anything else
that writes histogram snapshots (external producers, test fixtures built via
`RezRecorder`/`metriken-exposition` builders) must stamp them itself — the
snapshot builders do not add them for you. See `histogram_labels()` in
`src/analysis/extract/golden.rs` for a working example.
