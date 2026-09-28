# Checks with verdicts, stored in the recording

- **Opened:** 2026-09-28
- **Status:** OPEN — design, nothing built.

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
  "query": "histogram_quantile(0.99, rate(request_latency[1m]))",
  "type": "histogram",
  "check": { "above": 5e6, "for": "10s", "severity": "fail" }
}
```

`check.above` / `check.below` is the threshold on the query's value;
`check.for` is the minimum duration the condition must hold before it counts
(the Prometheus alerting-rule shape, which users already know). `severity` is
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
