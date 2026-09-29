# Checks with verdicts, stored in the recording

- **Opened:** 2026-09-28
- **Status:** BUILT (this PR): `Kpi.check`, `rezolus recording check` with
  `--queries`/`--recording`/`--annotate`/`--json`, band-aware verdicts. Family
  checks and the in-viewer button remain open. Deviations from the design are
  listed at the end of the Design section.

## Problem

Nothing in the tree can say a recording passed or failed. What exists, and
what each stops short of:

- **Service KPIs** (`crates/dashboard/src/service_extension.rs`) are PromQL
  queries with a role, a title and a chart type, embedded by
  `recording annotate --queries`. The `ServiceExtension` struct carries an
  `slo: Option<serde_json::Value>` field; every shipped template sets it to
  `null` and nothing reads it. A KPI draws a chart. It does not have a
  threshold.
- **`mcp detect-anomalies`** (`src/mcp/anomaly_detection/`) runs MAD, CUSUM
  and FFT over one series and prints findings. It is exploratory: the query
  is chosen at call time, the output is text, and nothing is written back.
- **Events** are stored in the manifest and drawn in the viewer, but every
  event is authored by a person or by `annotate`.
- **The notebook and report** hold a person's notes over pinned charts.

So a nightly job that records a benchmark has to script its own pass/fail
with `mcp query` and keep the verdict somewhere else. When someone opens the
recording a month later, the recording does not say what was checked or what
the answer was.

## Goal

A check is a stored expression with a threshold. Running the checks over a
recording produces verdicts. Verdicts are written into the recording as
events, so the viewer draws them where they fired, and the manifest carries
the check text that produced them.

## Design

**A check is a KPI with a condition.** Extend `Kpi` rather than add a new
entity. Two optional fields:

```json
{
  "role": "latency",
  "title": "p99 request latency",
  "query": "request_latency",
  "type": "histogram",
  "check": { "above": 5e6, "quantile": 0.99, "for": "10s", "severity": "fail" }
}
```

`check.above` / `check.below` is the threshold on the query's value;
`check.quantile` picks the quantile of a histogram KPI, whose query names
the raw histogram (the check wraps it in `histogram_quantile` itself);
`check.for` is the minimum duration the condition must hold before it counts
(the shape of a Prometheus alerting rule's `for`). `severity` is
`fail` or `warn`. A KPI without `check` is a chart as today, so every existing
template is unchanged. The `slo` field is removed or repurposed in the same
change; leaving a second null slot beside `check` invites drift.

**Evaluate in the query engine, not in the frontend.** A new
`rezolus recording check <file> [--queries kpis.json]` evaluates every check
over the recording through `metriken_query`, the same engine `mcp query`
uses. It reads the checks from the file's own manifest unless `--queries`
overrides. Output: one line per check with pass/fail/warn and the first
violation window, exit status 1 on any `fail`. This is what CI runs.

**Verdicts become events.** With `--annotate`, each violation is written as
an event with `kind = "check"`, `timestamp` at the window start,
`duration_ns` for the window, `description` from the KPI title, and `details`
carrying the check JSON verbatim. The event path already exists
(`KEY_EVENTS`, `annotate --add-events`), the viewer already draws events, and
range rendering is the first item in
[events as ranges](2026-09-28-events-ranges-and-alignment.md). A check that
passes writes nothing; the manifest's KPI list is the record of what was
checked.

**Embedding the check text in the event is the versioning.** A template edited
after the run does not change what an old recording claims, because the
event carries the condition it was evaluated with. No separate version
number.

**Uncertainty is respected.** `rate()` and `irate()` values carry an
acquisition-window band `[lo, hi]`. A check on a rate compares against the
band: `above` fires only when `lo` exceeds the threshold, `below` only when
`hi` is under it. A point whose band straddles the threshold is neither, and
that is reported as its own state (`indeterminate`) rather than folded into
pass. This is the one place the design differs from the alerting-rule shape,
and it is the point of having the bands.

**Family checks.** Once
[a baseline from many recordings](2026-09-28-baseline-from-many-recordings.md)
exists, a check may say `{"outside_family_sigma": 2, "for": "30s"}` against
the family band. Same evaluation path, same event output. Not part of the
first cut.

*Built (this PR).* `Check` (`above`/`below`, `quantile`, `for`, `severity`)
is `Kpi.check` in `crates/dashboard/src/service_extension.rs`; `slo` is
removed from `ServiceExtension` and from every template. Exactly one of
`above`/`below` is enforced at deserialization (`serde(try_from)`), so
`annotate --queries`, `check --queries` and the viewer all refuse an invalid
check with the same message; unknown keys inside `check` are refused too, so
a misspelled `for` cannot become a check that never fires. `for` is parsed by
a small hand-written grammar (`10s`, `1m30s`, `500ms`, `1.5s`, a bare number
of seconds) rather than `humantime`, since the crate builds for wasm32; it
serializes back as that string. `rezolus recording check` is
`src/parquet_tools/check.rs`: it reads the recording's KPIs through
`viewer::metadata::service_extensions_from_metadata`, the parquet-footer
loader split so it takes a metadata map and therefore also serves a `.rez`
recording's manifest (the viewer's path-based loader now calls it), evaluates
each check with one `query_range` over the whole span on a uniform grid whose
step is the recording's sampling interval capped at 1 s, and scans the single
series for runs. The comparison is strict (a value equal to the threshold
passes). A run ends at a point in another state and at a gap of more than
1.5 steps between points: the engine emits no point where the recording has
no data, so without the gap rule `for` counted unobserved time (a gauge above
threshold for 2 s, the agent down for five minutes, above again for 2 s, read
as one 5m04s window). An interpolated point (`MatrixSample.interpolated`, a
value across an unread span with no band) is indeterminate. Verdict lines are
`PASS|WARN|FAIL|INDETERMINATE|ERROR`; exit 2 on any ERROR, else 1 on a
`fail`-severity FAIL, else 0; `--json` emits the same as an array, and with
`--annotate` the annotation report goes to stderr so stdout stays one array.
`--annotate` writes each FAIL/WARN window as a `kind=check` event through the
existing event code (`events::append_to_parquet`; `annotate_rez_v3_at` with a
new per-recording events field). The id is `check:<sha256[..8]>` over the
title, the evaluated query, the condition and the window start; the query is
in the hash because titles repeat across services by design. On a re-run
`events::append_events` replaces a stored `kind=check` event whose id matches
and whose content differs (a window that grew gets its new `duration_ns`),
counts an identical one as unchanged, and counts an id repeated inside one
batch as a duplicate; a window that no longer fires keeps its old event. A
dendro archive evaluates (it reads through the same `RezReader`) but
`--annotate` is refused before evaluation, since nothing in this version
writes one. Deviations from the design above:

- **Histogram KPIs take `check.quantile`.** The design's example originally
  put `histogram_quantile(0.99, ...)` inside the KPI query, but
  `Kpi::effective_query` wraps a histogram KPI's query in
  `histogram_quantiles([...], q)` for the chart, so a query that already
  selects a quantile would render wrong. The check instead evaluates
  `histogram_quantile(<quantile>, <query>)` and the chart is unchanged; a
  histogram KPI whose check has no `quantile`, or whose query already contains
  `histogram_quantile`, is an ERROR naming the problem, and `quantile` on a
  non-histogram KPI is an ERROR rather than a condition text that claims a
  quantile the evaluation never applied. The example above was corrected.
- **`details` is two lines, not the check JSON verbatim:** the condition as
  prose (`<title> <condition>: <severity>`) then the check JSON. The JSON is
  still the version record; the first line is what a tooltip shows.
- **Exit 2 exists** for evaluation errors (a query matching nothing, several
  series, an unreadable file), and outranks exit 1. The design named only
  exit 1.
- **A multi-recording `.rez` with no `--recording` is checked whole**, one
  verdict list per recording with the labels as a line prefix, where the MCP
  tools would refuse and ask for a selector; `--annotate` then writes each
  recording's events into that recording. With `--recording` the flag has the
  MCP semantics (exactly one recording) and `--annotate` writes only into
  that recording.
- **The `for` rule is span `>= for` with span `= last - first + step`.** A
  single point at a 1 s step spans 1 s, so `for: "1s"` accepts one point.
  This is one step more generous than a Prometheus `for`, which needs the
  condition to have held for the duration since it first became true.
- **Cross-cadence evaluation is not used.** `RezReader::query_range_opts`
  passes straight through, and only the viewer supplies
  `QueryOptions::eval_timestamps`; `check` evaluates on the uniform grid.
- **No template gained a check.** Every built-in KPI is a chart still; the
  worked example lives in `docs/usage.md`. The GO condition's "one check in
  each of the vLLM, SGLang and Valkey templates" was not done: a `fail` in a
  shipped template would turn every user's CI red on a threshold nobody
  chose, and a `warn` is noise with nothing behind it.
- **Not measured:** the NO-GO gate on the 9.6 h archive. The cost is one
  `query_range` per check, the same as one `mcp query` each, so it is bounded
  by that entry's numbers rather than by anything new here.

## Not in scope

- Live evaluation against a running agent. The recorder and hindsight are the
  places that would host it; a separate entry when there is demand.
- A review state on a verdict (acknowledged, dismissed). A person can add a
  note in the notebook; a workflow layer is a different product.
- Webhooks or notifications. CI already has those; the exit status is the
  integration point.

## GO / NO-GO

GO when `recording check` runs the vLLM, SGLang and Valkey templates with one
check added to each, on the existing smoke fixtures, and a deliberately
failing threshold produces exit 1, an event in the manifest, and a band in
the viewer. NO-GO condition: if evaluating every KPI over a long recording is
slower than opening the viewer on it, the check needs the lazy reader path
from the reader-memory entry before it ships; measure on the 9.6 h archive
that entry used.

## Deferred / Reopen

- **Checks inside the viewer.** A "Run checks" button that evaluates and
  shows verdicts without the CLI. Reopen after the CLI has users.
- **Rate-band `indeterminate` in CI.** Whether CI should treat it as pass or
  fail is a policy each job sets; the default is to report it and not fail.
  Reopen if jobs need a flag.
- **An `mcp run_checks` tool.** See
  [MCP write-back and tool tiers](2026-09-28-mcp-write-back.md).

## Cross-references

- [Measurement uncertainty](2026-07-08-measurement-uncertainty.md) and
  [cross-table uncertainty](2026-08-21-cross-table-uncertainty.md): the
  bands a check compares against.
- [Long recordings: reader memory](2026-09-23-reader-memory.md): the read
  path a whole-recording check depends on.
