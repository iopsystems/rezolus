# Display-mode decimation — mean vs. median for the line (discussion)

- **Opened:** 2026-07-21
- **Status:** OPEN — discussion. No implementation planned until this settles;
  the labeling half shipped independently (swatch row, PR #1021).
- **Context:** follow-on question from reviewing the #1017 uncertainty bands:
  "why is the line drawn at the median, not the mean?" This entry records the
  tradeoff so the next person doesn't re-derive it, and frames the open choice.

## Where the median comes from

Display-mode decimation ([entry](2026-07-13-viewer-display-decimation.md),
PR #1006) reduces each time bucket to a per-bucket boxplot
`{min, lo, median, hi, max}` (metriken-query `src/display.rs`,
`Reducer::Boxplot`; inner band quantiles configurable, default IQR). The chart
draws the **median** as the line, the IQR as the inner fill, min–max as the
outer envelope (`src/viewer/assets/lib/charts/boxplot.js`
`buildBoxplotSeries`). Since PR #1021 the swatch row labels the line "median"
on any chart where bands are visible.

## The case for the median (the shipped design)

- **Division of labor.** The line answers "what was the system mostly doing";
  the envelope answers "what were the excursions". A bucket of
  `[80, 95, 100, 100, 102, 103, 105, 110, 150, 400]` has median 102.5 and mean
  134.5 — the mean lands *above the inner band's top edge*, a level no sample
  was near. With a mean line, a spike distorts the line *and* appears in the
  envelope: counted twice, typical level lost.
- **Geometric invariant.** `p25 ≤ median ≤ p75` by construction, so the line
  cannot escape its own inner band. A mean line outside the band reads as a
  rendering bug.
- **Spikes are not lost.** The outer envelope is exact per-bucket min/max —
  "decimation is lossless for extremes" was the headline guarantee of the
  decimation effort (and why M4 was a measured NO-GO there).

## The case for the mean

- **Conservation.** `mean × bucket-width = Σ samples` exactly. Integrating a
  mean line over the visible window yields the true total (bytes moved, ops
  served). The median line under-counts bursty workloads — a chart whose
  area-under-line a user mentally reads as "total work" is quietly wrong for
  skewed buckets, and rates/counters are precisely where that reading is
  natural.
- **Ecosystem expectation.** Prometheus-family UIs downsample by averaging;
  users arriving from there may assume mean semantics.

Scope note: the conservation argument applies to **additive quantities**
(counter rates, additive gauges). For percentile series (p99 lines etc.) a
mean-over-time has no conservation meaning and the robust median is strictly
the right reducer — any change should exclude them.

## What does NOT mitigate this (a conflation to avoid)

The chart-header **Mean/Total toggle** (`src/viewer/assets/lib/charts/line.js`
`toggleTotal`, `promql_query_total`) switches aggregation *across series*
(per-entity mean vs. fleet sum). It has nothing to do with the per-bucket
time statistic. The real mitigations today are: the exact min/max envelope,
drill-down refetch (zooming converges to native resolution, where
`min == median == max` and the question vanishes), and the "median" swatch
label (#1021).

## Open questions

1. **Should the wire carry a per-bucket mean?** One more f64 column in the
   display blob (`dashboard::display_wire` / `data.js decodeDisplayBinary`)
   makes a mean available without re-querying. Cheap; the question is what
   consumes it.
2. **If carried, where does it surface?** Candidates, least→most invasive:
   tooltip row ("median X · bucket mean Y"); a per-chart line toggle; mean
   line by default for rate charts only. A second always-on line was already
   rejected aesthetically for compare mode ("muddy filled bands" reasoning in
   the decimation entry) — the same clutter argument applies here.
3. **Do any downstream surfaces read decimated medians as means?** Report/
   notebook statistics and A/B compare deltas: the compare divergence band
   (`buildDivergenceBand`) shades the gap between two *medians* — for
   asymmetric burstiness, a median-gap can understate a mean-gap. To verify
   before deciding: whether selection/notebook stats recompute from raw
   queries (fine) or from display arrays (would inherit median semantics).
4. **Tooltip honesty, independent of the choice:** the tooltip shows the
   median value unlabeled (band series are tooltip-suppressed). A "(median)"
   qualifier is a one-line change and worth doing regardless.

## Current leaning (not a decision)

Keep the median as the drawn line (the robustness + invariant arguments are
about *reading* the chart, which is the chart's job), and treat conservation
as a data question: carry the per-bucket mean in the wire, surface it in the
tooltip, and revisit a mean-line mode only if a concrete workflow (totals
eyeballing over long windows) demonstrates need. Decide after Q3 is verified.

## Reopen / decision criteria

Settle when either (a) a user-visible mis-read traced to median-vs-mean is
reported (that's evidence for Q3), or (b) the next decimation-adjacent effort
touches the wire format — bundling the extra column then is nearly free.
