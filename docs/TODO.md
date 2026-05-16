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
