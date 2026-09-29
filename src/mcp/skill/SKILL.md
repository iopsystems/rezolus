---
name: rezolus-mcp
description: Investigate a Rezolus performance recording (.parquet, .rez, .dendro) through the rezolus MCP server. Discovery before query, features before hypotheses, one recording at a time.
---

# Investigating a Rezolus recording

The `rezolus` MCP server opens a recording file and answers about it.
Every tool takes `parquet_file`, the path to a `.parquet`, `.rez` or
`.dendro` file (the name is historical; the format is detected by content).
A `.rez` is Rezolus's archive, which can hold several recordings; a
`.dendro` is the newer archive container and reads the same way. Ask the
person for the file if you were not given one; `rezolus record -o out.rez`
writes one.

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
   specific hypotheses. `detect_anomalies` needs ONE series: aggregate with
   `sum()` / `avg()` or add label matchers.

Rules that hold on every recording:

- Report an absent metric as missing. A sampler that was off or a kernel
  without the hook leaves the metric out of the recording.
- `rate()` values carry an uncertainty band `[lo, hi]` from the acquisition
  window: in `query` JSON it is the `bands` (and `intervals`) list aligned
  with `values`. A change smaller than the band was not measured. A
  recording with no acquisition windows (for example a parquet combined
  into a `.rez`) has no band, which says nothing about its precision.
- Report a correlation as two series moving together, without a claim
  about which one drives the other.
- On a multi-recording `.rez`, pass the `recording` selector
  (e.g. `{"source": "redis"}`) to every call after `describe_recording`.
  Matching none or several is an error that lists the candidates; it never
  guesses.

## Reporting

Lead with the finding and the instant it happened, then the evidence
(metric, query, values with their bands). State what could not be measured.
To mark a finding in the recording itself, the person can run
`rezolus recording annotate <file> --event 'time=<RFC 3339>,kind=finding,description="<text>"'`.
Keep the quotes around the description: a comma inside it otherwise splits
the event. On a multi-recording archive, annotate writes the event into
every recording, not only the one you investigated; say so when you suggest
it.
