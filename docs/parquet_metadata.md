# Parquet File-Level Metadata

Rezolus parquet files carry key/value metadata in the parquet footer. The
viewer, MCP, and downstream tools rely on these keys to interpret the data,
distinguish recordings, build dashboards, and combine files. This document
describes each key and how to update it.

The canonical list lives in [src/parquet_metadata.rs](../src/parquet_metadata.rs);
this doc is the prose companion to that file.

## Inspecting metadata

```bash
# Human-readable file-level metadata
target/release/rezolus parquet metadata -i file.parquet --file

# Full JSON (includes nested per-source metadata, parsed)
target/release/rezolus parquet metadata -i file.parquet --json

# Pull a single key (auto-pretty-prints if the value is JSON)
target/release/rezolus parquet metadata -i file.parquet --field source
target/release/rezolus parquet metadata -i file.parquet --field per_source_metadata
```

## Single-source vs combined files

A "single-source" file comes from one recording (one rezolus agent, or one
service endpoint). A "combined" file is produced by `parquet combine` and may
hold multiple rezolus nodes and/or multiple service instances.

Several keys live at the **top level** in single-source files but get nested
under [`per_source_metadata.<source>.<sub_key>`](#per_source_metadata) in
combined files. Where applicable this is called out below.

## Top-level keys

### `source`

Identifies the recording's source(s).

- Single-source file: a flat string, e.g. `"rezolus"`, `"llm-perf"`, `"vllm"`,
  `"sglang"`.
- Combined file: a JSON array of source names, e.g. `["rezolus","vllm"]`
  (deduplicated and sorted).

**Set at record time:** Inferred automatically from the endpoint type; can
also be overridden via `--metadata source=<name>`. For non-rezolus endpoints
the source is what you tell it (or what's inferred from labels) — this is what
[`parquet annotate`](#service_queries) keys off of when picking a built-in
template.

**Set or replace post-recording with `parquet annotate --source`:**

```bash
# Add a source label to a file that has none (so the bare-annotate
# template-lookup flow works)
target/release/rezolus parquet annotate file.parquet --source vllm

# Replace an existing source — refused without --overwrite to prevent
# silent mislabelling
target/release/rezolus parquet annotate file.parquet --source sglang --overwrite
```

Setting `--source` is a no-op when the file already has the same value.
Replacing a different value requires `--overwrite`. Used alone, `--source`
*only* rewrites the `source` key — the template flow is not auto-triggered.
To set the source and apply the matching template in one step, follow up
with bare `parquet annotate file.parquet`, or pass `--queries`/`--filter`
in the same invocation.

### `version`

Agent/tool version string of the source that produced this file. Single-source
only — when files are combined this moves to
`per_source_metadata.<source>.<id>.version`.

**Set at record time** by the recorder/agent. Not user-editable.

### `sampling_interval_ms`

Collection interval in milliseconds, written as a decimal string (e.g.
`"1000"`).

This must be **identical across all inputs** before `parquet combine` will
accept them — it's also what `combine` uses to quantize timestamps to a
common grid. Mismatched intervals fail validation up front.

**Set at record time** via the `--interval` flag on `rezolus record`.

### `systeminfo`

JSON-serialised hardware summary fetched from the rezolus agent's
`/systeminfo` endpoint. Display-only — used by the viewer and MCP to render
the host summary panel.

In combined files, the *first* rezolus node's value is kept at the top level
for viewer compatibility. A copy of each node's `systeminfo` is stashed into
`per_source_metadata.rezolus.<node>.systeminfo` so multi-node combined files
don't lose per-host data.

**Set at record time** by the recorder when scraping a rezolus endpoint. Not
user-editable.

### `descriptions`

JSON map of metric name → help text, used by `mcp describe-metrics` and the
viewer tooltip layer. In combined files this is union-merged across all
inputs (first writer wins on conflicts).

**Set at record time** by the recorder (fetched from
`/metrics/descriptions`). Not user-editable.

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

**Update with `parquet annotate`:**

```bash
# Use the built-in template that matches the file's `source`
target/release/rezolus parquet annotate file.parquet

# Use a custom JSON file
target/release/rezolus parquet annotate file.parquet --queries my_kpis.json

# Also drop columns the KPIs don't touch (saves space)
target/release/rezolus parquet annotate file.parquet --filter

# Remove the annotation
target/release/rezolus parquet annotate file.parquet --undo
```

Annotation validates each KPI by running its PromQL query against the file's
data and sets `available: true|false` on each KPI accordingly.

### `node`

Hostname/VM identifier for rezolus agent data. In combined files this lives
under `per_source_metadata.rezolus.<node>.node`.

The `parquet combine` step requires every rezolus input to have a unique node
label. If the metadata key is missing, the filename stem is used as a
fallback for rezolus files.

**Set at record time:**

```bash
# Explicit
target/release/rezolus record --node web01 http://localhost:4241 web01.parquet

# Generic key=value form (equivalent)
target/release/rezolus record --metadata node=web01 http://localhost:4241 web01.parquet
```

**Update an existing file with `parquet annotate --node`:**

```bash
# Set or replace the node attribute on a file recorded without one
target/release/rezolus parquet annotate file.parquet --node web01
```

This is useful for service recordings where you want to record which host
the service ran on (informational; combine still keys service columns by
`instance`), or for retroactively labelling a rezolus file before
`parquet combine`. The flag rewrites only the `node` key — it does not
auto-apply a service-extension template.

### `instance`

Process/container identifier for service data (vllm, llm-perf, sglang, ...).
In combined files this lives under
`per_source_metadata.<source>.<instance>.instance`.

When `parquet combine` sees multiple files for the same source: either every
file carries an `instance` label (must be unique within the source), or none
do (combine auto-assigns `"0"`, `"1"`, ... in input order). Mixed is rejected.

**Set at record time:**

```bash
target/release/rezolus record --instance primary http://vllm-host:8000/metrics primary.parquet
# or: --metadata instance=primary
```

### `pinned_node`

The default rezolus node the viewer should focus on when opening a combined
file with multiple nodes. Only meaningful in combined files.

**Set at combine time:**

```bash
target/release/rezolus parquet combine \
    web01.parquet web02.parquet llm-perf.parquet \
    -o combined.parquet \
    --pinned web01
```

Validation rejects `--pinned <name>` if no rezolus input has a matching `node`
label.

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
| `rezolus record --node`, `--instance`, `--metadata k=v` | Anything written at recording time. The catch-all `--metadata` can set any top-level key. |
| `rezolus parquet annotate` | Adds/replaces/removes top-level `service_queries`; with `--node NAME` sets/replaces top-level `node`; with `--source NAME` (`--overwrite` to replace) sets/replaces top-level `source`. |
| `rezolus parquet combine --pinned` | Sets `pinned_node` on the output. |
| `rezolus parquet combine` | Merges and re-derives `source`, `descriptions`, `per_source_metadata`, etc. from the inputs. |

For anything not covered above, the path is: read the current file with
`parquet metadata --json`, write a small Rust binary that uses
[`rewrite_parquet`](../src/parquet_tools/mod.rs) (or the `arrow` /
`parquet` crates directly) to set the key, and emit a new file. The
parquet writer always rewrites the entire footer, so there's no way to
patch a single key in place.

## Example: `parquet combine`

A worked example showing exactly what moves where. We start with three
single-source recordings — two rezolus agents on different hosts and one
vllm service instance — and combine them.

### Inputs

**`web01.parquet`** — produced by
`rezolus record --node web01 http://web01:4241 web01.parquet`:

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

**`vllm.parquet`** — produced by
`rezolus record --instance primary --metadata role=service http://vllm:8000/metrics vllm.parquet`,
then annotated with `rezolus parquet annotate vllm.parquet`:

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

### Command

```bash
target/release/rezolus parquet combine \
    web01.parquet web02.parquet vllm.parquet \
    -o combined.parquet \
    --pinned web01
```

### Inspecting the result

```bash
# Top-level keys only
target/release/rezolus parquet metadata -i combined.parquet --file

# Full structured view (parses nested JSON values)
target/release/rezolus parquet metadata -i combined.parquet --json

# Drill into a single key
target/release/rezolus parquet metadata -i combined.parquet --field source
target/release/rezolus parquet metadata -i combined.parquet --field per_source_metadata
target/release/rezolus parquet metadata -i combined.parquet --field pinned_node
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
| `per_source_metadata` | Deep-merged. Pre-existing entries from already-combined inputs are preserved; new sub-entries are added per node/instance. |

Schema changes alongside the metadata: every metric column is renamed to
`<node-or-instance>::<metric>` (e.g. `web01::cpu_usage`, `primary::tokens`)
with `node`/`instance`/`source` labels added to the column-level metadata.
The `timestamp` and `duration` columns come from the first rezolus input
(or first input if none is rezolus) and are not prefixed.

## Writer-side knobs (not metadata, but related)

These are file-format settings every rezolus parquet writer applies. They're
not in the KV map but matter when producing files that combine cleanly with
others:

- **Row group size:** `MAX_ROW_GROUP_SIZE = 50_000` (matches
  `metriken-exposition`'s default). All rezolus tools enforce this so
  combined files don't end up with one giant row group.
- **Compression:** ZSTD on every column.

If you write parquet from another tool that you intend to feed into
`parquet combine`, match these settings to keep behaviour predictable.
