---
name: rezolus-mcp
description: Investigate a Rezolus performance recording (.parquet, .rez, .dendro) through the rezolus MCP server. Discovery before query, verdicts and marks written back, links to the viewer.
---

# Investigating a Rezolus recording

The `rezolus` MCP server opens a recording file and answers about it.
Every tool that reads a recording takes `parquet_file`, the path to a
`.parquet`, `.rez` or `.dendro` file (the name is historical; the format is
detected by content). A `.rez` is Rezolus's archive, which can hold several
recordings; a `.dendro` is the newer archive container and reads the same
way. `viewer_link` reads no recording and takes no path. Ask the person for
the file if you were not given one; `rezolus record -o out.rez` writes one.

## Workflow

1. **`describe_recording`** first. It says what the file holds: source,
   agent version, time range, duration, and, for a multi-recording `.rez`,
   the list of recordings with the `recording` selector that picks each.
2. **`describe_metrics`** before any query. Metric names, types and labels
   come from the recording, not from memory. A counter is queried with
   `rate(name[1m])`, a gauge directly, a histogram with
   `histogram_quantile(0.99, name)`.
3. **`extract_features`** for the overview: per-metric stats, noise class,
   anomalies, regime shifts, uncertainty, correlations, resource rankings.
   Read it before forming hypotheses; it needs at least 10 s of data.
4. **`query`**, **`detect_anomalies`**, **`analyze_correlation`** to test
   specific hypotheses. `detect_anomalies` and a check need ONE series:
   aggregate with `sum()` / `avg()` or add label matchers.
5. Write back what you found (below), and hand the person a link.

Rules that hold on every recording:

- A missing metric is not zero. A sampler that was off or a kernel without
  the hook leaves the metric absent; say so rather than reading it as idle.
- `rate()` values carry an uncertainty band `[lo, hi]` from the acquisition
  window: in `query` JSON it is the `bands` (and `intervals`) list aligned
  with `values`. A difference inside the band is not a difference. A
  recording with no acquisition windows (for example a parquet combined into
  a `.rez`) has no band, which says nothing about its precision.
- Correlation does not establish causation. Report it as co-movement.
- On a multi-recording `.rez`, pass the `recording` selector
  (e.g. `{"source": "redis"}`) to every call. Matching none or several is
  an error that lists the candidates; it never guesses. The one exception
  is `run_checks`, which without a selector evaluates every recording and
  gives each its own verdicts.

## Writing back

- **`add_event`** marks an instant or a range (`duration`) in the
  recording; the viewer draws it on the timeline. Give a `kind`
  (`finding`, `spike`, `incident`, `deploy`), a one-line `description`,
  and put the evidence (the query, the numbers) in `details`. `source`
  defaults to `mcp`, so your marks can be filtered or removed as a group.
  `timestamp` is RFC 3339 or Unix seconds as a JSON number (never a
  digit-only string). The reply carries the event id; adding an event
  with an `id` already present changes nothing, so retrying is safe (a
  `kind=check` event is the exception: it replaces a stored check event
  with the same id).
- **`run_checks`** evaluates the recording's KPI checks (or a
  ServiceExtension object you pass as `queries`) and returns each verdict
  with its violation windows; `annotate: true` writes them as events.
- **`export_query`** writes a range query as CSV or parquet (long form:
  series, timestamp, value, lo, hi) for analysis outside PromQL. It works
  only when the server was started with `--export-dir`. `filename` is a
  bare name (no directories) and an existing file is never overwritten;
  `step` defaults to 1 s; results over a million rows are refused, so raise
  `step` or aggregate; a heatmap is refused, so export
  `histogram_quantile(...)` of it.
- **`viewer_link`** builds a link into a running `rezolus view`: a
  `section` (`cpu`, `memory`, ..., or `service/<name>`), optional
  `chart_id`, `from`/`to`, and the rest of the view state. With
  `viewer_url` (or a server started with `--viewer-url`) it is a full URL;
  otherwise append the returned query and fragment to the viewer's
  address.
- **`remove_events`** exists only when the server was started with
  `--allow-mutating`. It removes by `ids`, `kind` and/or `source`; an empty
  filter is refused.

## Reporting

Lead with the finding and the instant it happened, then the evidence
(metric, query, values with their bands), then what you wrote back (event
ids) and the link. State what could not be measured.
