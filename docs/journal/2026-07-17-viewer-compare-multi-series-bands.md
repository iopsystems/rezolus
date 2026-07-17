# Measurement uncertainty — viewer bands on compare & multi-series charts

- **Opened:** 2026-07-17
- **Status:** LANDED (rezolus `measurement-uncertainty-phase-1`). Uncertainty
  bands (rate/histogram value bounds) now render on **compare (A/B overlay)**
  line charts and on **percentile multi-series** charts, not just single-series.
  The data layer previously discarded `intervals` for every non-single-series
  render (`plot.intervals = null` in the multi branch); it now carries a
  per-capture / per-series band through to the renderers, which reuse the
  existing `buildBandSeries` translucent-stack from the single-series work.
- **Arc:** [measurement uncertainty](2026-07-08-measurement-uncertainty.md).
  Round 3 (final) of the post-rate follow-on sequence (histogram coverage →
  correlation → **compare/multi-series bands**).
- **Owner:** Brian Martin
- **Repos:** rezolus (`~/workspace/rezolus`) — `src/viewer/assets/lib/`.

This entry is both the design spec and the outcome record.

## Why

The rate() round added a translucent uncertainty band behind a **single-series**
line (`buildBandSeries`, `charts/line.js`). But the moment a chart carried more
than one line — an **A/B compare overlay** (baseline vs experiment) or a
**multi-series** panel (per-core, per-cgroup, per-quantile) — the band vanished.
The cause was structural, not cosmetic: `applyResultToPlot`'s multi-series branch
set `plot.intervals = null` and never repopulated it, and the compare capture
extractors never carried a band. So the two chart families where uncertainty most
changes a *comparison* conclusion (is B actually faster than A? is p99's rise
real or bucket noise?) showed bare lines.

## Scope decision

Compare-line bands are the headline and unambiguous (2 series, clean). Multi-series
is a UX fork: these charts range from ~4 quantile lines to 30+ per-core series, and
a per-series translucent band on 30 lines is a wash. **Chosen (user):** render
multi-series bands **only for percentile charts** (`histogram_quantiles`, few
lines) — reusing the existing `isPercentileChart` gate — while still *carrying*
per-series bands in the data layer so a future opt-in could draw them for
categorical multis. Per-core/cgroup fleets stay clean. (Alternatives — band every
multi series, or defer multi entirely — were rejected.)

## Data flow

Bands ride `MatrixSample.intervals` (populated only for `.rez`/live sources with
acquisition windows; a plain `.parquet` has none, so nothing renders — unchanged).

- **Single-series** (unchanged): `applyResultToPlot` → `plot.intervals` →
  `line.js` seriesList[0].intervals → `buildBandSeries`.
- **Compare A/B line** (new): each capture carries its band —
  `extractBaselineCapture` reads `spec.intervals`; `extractExperimentCapture` gets
  it from `promqlResultToLinePair`, which now returns `intervals: parseIntervals`.
  `overlayLine` passes `intervals` into each `multiSeries[i]`; `line.js` already
  maps `buildBandSeries` over every series, so the two capture lines each get their
  band. The band is parallel to `valueData` (value uncertainty, time-independent),
  so it survives the compare time-`rebase` untouched.
- **Multi-series** (new, data layer): `applyResultToPlot`'s multi branch collects
  `parseIntervals(item)` per series into `plot.series_intervals`, parallel to
  `series_names` (nulls for non-rate). `multi.js` renders them via
  `buildBandSeries` **only when `isPercentileChart`**, prepended behind the lines
  (`z:1`).

## Files

- `src/viewer/assets/lib/data.js` — `promqlResultToLinePair` returns `intervals`;
  multi branch populates `plot.series_intervals` (and clears it in the
  single-series / no-data branches to prevent ghosting).
- `src/viewer/assets/lib/viewer_core.js` — `extractBaselineCapture` /
  `extractExperimentCapture` (line style) set `cap.intervals`.
- `src/viewer/assets/lib/charts/compare.js` — `overlayLine` carries
  `baseline.intervals` / `experiment.intervals` into `multiSeries`.
- `src/viewer/assets/lib/charts/multi.js` — imports `buildBandSeries`; builds a
  band per percentile series from `spec.series_intervals`; `series:
  [...bandSeries, ...series]`.

## Testing

`tests/compare_bands.test.mjs` (pure-JS, node test runner; a 2-line
`getComputedStyle`/`document` shim lets `compare.js` import without jsdom — its
`colormap.js` reads CSS custom props at load):

- `promqlResultToLinePair` extracts / omits `intervals`.
- Compare-line overlay carries each capture's band into `multiSeries`; omits when
  captures have none.
- Multi-series result populates `series_intervals` parallel to names; nulls when
  no bands.

`node --test tests/*.mjs`: 170 pass (the one failure,
`wasm_viewer_histogram_kpis`, is the pre-existing gitignored-WASM-artifact gap,
unrelated). `multi.js` import-checked for syntax + no import cycle
(multi → line → data; data imports neither).

## Fit with the arc

Closes the viewer side of the measurement-uncertainty arc: every chart family the
dashboards actually use — single-series, A/B compare, and percentile multi — now
shows the honest band when the source carries windows. The uncertainty *values*
come from the earlier rounds (rate/irate + propagation; histogram bucket
resolution); this round is purely about **surfacing** them where more than one
line shares an axis.

## Deferred / reopen conditions

- **Categorical multi-series bands** (per-core/cgroup) — data is carried
  (`series_intervals`) but undrawn to avoid a band wash. Reopen with an opt-in
  per-chart toggle if a user wants them.
- **Compare multi/scatter/heatmap bands** — only the compare **line** overlay is
  banded. The `splitMultiToSubgroup` / side-by-side-heatmap / scatter compare
  strategies are untouched (heatmaps encode value in color, not a band).
- **Tooltip band readout** — the band renders visually; the tooltip still shows
  only the nominal. Threading `[lo, hi]` into the compare tooltip is the same
  wider `getTooltipFormatter` refactor already flagged in `line.js`.
