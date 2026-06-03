# Project TODO / backlog

Non-blocking follow-ups that don't belong to any in-flight PR. BPF
sampler design rules + their own backlog live in `docs/principles.md`;
this file is for everything else (dashboard, viewer, tooling).

## Dashboard / metriken

- **Single quantiles() call for count + mean + percentiles.** A
  metriken histogram column already carries the full distribution;
  `quantiles()` exposes the total count and can yield the mean, so
  emitting/querying separate count and mean series alongside the
  percentile distribution is redundant. The dashboard chart generation
  (`crates/dashboard/src/dashboard/*.rs`) and the viewer's
  histogram/percentile query paths should derive count, mean, and the
  percentile distribution from that one column + one call instead of
  parallel metrics. Reduces parquet columns and query fan-out.

## Viewer

- **Customizable report title + browser tab title.** Let the user set
  a report title (likely alongside the existing Notebook/Report
  preamble/notes machinery in `selection.js`, persisted in the
  selection payload like `tagline`/`note`). When viewing the Report or
  Notebook routes, overwrite `document.title` with
  `Report` / `Notebook` plus `: <optional title>` when one is set
  (e.g. `Report: vLLM prefill regression`); restore the default title
  on other routes.
