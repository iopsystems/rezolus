# Multi-Endpoint Recorder Design

## Problem

`rezolus record` currently supports a single URL. In practice, users need to
capture metrics from multiple sources simultaneously — e.g. a Rezolus agent
(msgpack) alongside a service's Prometheus endpoint — and want coordinated
timestamps and a single output artifact.

## Goals

- Record from N endpoints concurrently with aligned timestamps
- Support mixed protocols (Rezolus msgpack and Prometheus text)
- Combined output by default; separate files as an option
- Graceful handling of late-joining and transiently-unavailable endpoints
- Full column-level provenance (which endpoint/source each metric came from)
- Backward compatible: bare `rezolus record <url> <output>` still works

## Non-Goals

- Per-endpoint scrape intervals (all endpoints share a single interval)
- Live schema negotiation or metric filtering at record time
- Streaming output (parquet is written at finalization, same as today)

---

## 1. CLI & Configuration

### TOML config (production)

```toml
[recording]
interval = "1s"
output = "recording.parquet"
# format = "parquet"      # default
# separate = false         # default: combined output

[[endpoints]]
url = "http://localhost:4241/vars"
source = "rezolus"
# role = "agent"           # optional, stored in per_source_metadata

[[endpoints]]
url = "http://localhost:9090/metrics"
source = "vllm"
protocol = "prometheus"    # optional: override auto-detection
# role = "service"
```

### CLI shorthand (testing)

```bash
# Single endpoint — backward compatible, identical to today
rezolus record http://localhost:4241 output.parquet

# Multiple endpoints
rezolus record \
  --endpoint http://localhost:4241,source=rezolus \
  --endpoint http://localhost:9090/metrics,source=vllm \
  --separate \
  output.parquet

# Config file
rezolus record --config recording.toml
```

### Flag behavior

- `--metadata`, `--interval`, `--duration`, `--format` apply globally and
  override TOML values.
- `--separate` produces one parquet per endpoint:
  `{output_stem}_{source}.parquet`.
- Missing `source` name is inferred from hostname (e.g. `localhost-4241`) with
  a **warning to stderr**:
  `warn: no source name specified for http://localhost:4241, using "localhost-4241"`

---

## 2. Scraping Loop

### Connection management

Single shared `reqwest::Client` created at startup. Reqwest's internal
connection pool handles per-endpoint HTTP/1.1 keep-alive automatically —
long-lived connections are reused across scrape ticks.

### Tick alignment

The existing aligned-sleep logic is preserved. All endpoints are scraped on the
same clock tick.

### Concurrent fan-out

```
tick → join_all(endpoints.map(|ep| scrape(client, ep)))
     → collect results
     → write to buffer(s)
```

Each `scrape()` returns `Result<Snapshot, ScrapeError>` where `ScrapeError`
carries the endpoint URL for logging.

### Error handling

- **Per-tick failure**: log warning, skip that endpoint for this tick, continue.
  Other endpoints are not affected.
- **Persistent failure**: the endpoint stays in the active set and is retried
  every tick. No exponential backoff — the aligned tick is the natural retry
  interval.

### Per-endpoint scrape latency

Wall-clock time for each endpoint's HTTP round-trip is tracked and logged at
debug level.

---

## 3. Auto-Detection & Late Joiners

### Startup probe

Each endpoint is probed once at startup:

1. Try Rezolus msgpack (`/vars`)
2. Fall back to Prometheus text (`/metrics`)
3. If `protocol` is set in config, skip probing

### Best-effort startup

- Endpoints that respond: protocol detected, systeminfo/descriptions fetched,
  scraping begins immediately.
- Endpoints that fail: logged as warning, entered into **pending** state.
- **At least one endpoint must succeed** or the recorder exits with an error.

### Pending endpoint retry

Each tick, pending endpoints get a probe attempt alongside regular scrapes.
Once an endpoint responds, its protocol is detected, metadata is fetched, and
it joins the active scrape set.

```
info: endpoint http://host:9090/metrics (vllm) now available, starting capture
```

### Late-joiner columns

Columns appear in the schema from the first tick they have data. Earlier rows
have nulls for those columns — parquet handles this natively with nullable
columns.

---

## 4. Column & File Metadata

### Column-level metadata

Every metric column carries:

| Key           | Value                              |
|---------------|------------------------------------|
| `metric_type` | `counter` / `gauge` / `histogram`  |
| `source`      | logical source name (e.g. `vllm`)  |
| `endpoint`    | scrape URL                         |
| (existing)    | metric labels, grouping power, etc |

### File-level metadata

- `source` — JSON array of all source names: `["rezolus", "vllm"]`
- `per_source_metadata` — per-source map:

```json
{
  "vllm": {
    "version": "...",
    "role": "service",
    "service_queries": [...],
    "first_sample_ns": 1713100000000000000,
    "last_sample_ns":  1713103600000000000
  },
  "rezolus": {
    "version": "...",
    "role": "agent",
    "first_sample_ns": 1713099900000000000,
    "last_sample_ns":  1713103600000000000
  }
}
```

`first_sample_ns` and `last_sample_ns` are updated on every successful scrape
per endpoint. They enable downstream tooling to show per-source time coverage
and support future `parquet trim` operations.

---

## 5. Output Modes

### Combined (default)

- Single parquet file with all endpoints' columns merged into one schema.
- Identically-named metrics from different endpoints coexist as separate
  columns, disambiguated by `source`/`endpoint` column metadata.
- Schema merge runs inline at finalization — same logic as `parquet combine`
  but without the disk round-trip.
- `per_source_metadata` populated from each endpoint's state.

### Separate (`--separate`)

- One parquet per endpoint: `{output_stem}_{source}.parquet`
- Each file is self-contained with its own schema and metadata.
- Timestamps are aligned (same tick), so files are combinable later with
  `rezolus parquet combine`.

---

## 6. Graceful Shutdown

The existing two-phase Ctrl+C signal handling is preserved:

1. **First Ctrl+C** → `STATE` set to `TERMINATING`. Scrape loop exits.
   Finalization runs to completion: flush all endpoint buffers, merge schemas
   if combined mode, attach metadata, write parquet file(s).
2. **Second Ctrl+C** → hard exit via `process::exit(2)`.

This guarantees that a recording interrupted by Ctrl+C still produces valid
output, regardless of output mode.

---

## Per-Endpoint State

```rust
struct Endpoint {
    url: Url,
    source: String,
    role: Option<String>,
    protocol: Protocol,           // Msgpack | Prometheus
    systeminfo: Option<String>,   // fetched at probe time (Rezolus only)
    descriptions: Option<String>, // fetched at probe time
    first_success: Option<u64>,   // nanos since epoch
    last_success: Option<u64>,    // nanos since epoch
    state: EndpointState,         // Active | Pending
}
```
