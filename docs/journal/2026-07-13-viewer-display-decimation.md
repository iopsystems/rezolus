# Viewer display-mode decimation (min/max envelope + drill-down)

- **Opened:** 2026-07-13 (arc began ~2026-07-08; the interim revert shipped as v5.16.1).
- **Status:** IN REVIEW — **PR #1006** (iopsystems/rezolus, draft until browser-verified).
  Depends on **metriken #115** (merged, `bba87e2`) → **metriken-query 0.12.0** (published to
  crates.io via #116). Row-group prerequisite shipped separately in **#1003** (`372f6a63`).
- **Design record:** the design lived in the working session; this entry absorbs it. No
  separate spec doc to delete.

## Problem

`rezolus view` fetched every metric at native step and let echarts decimate client-side.
On a long recording that is a large payload of noisy points, and — critically — **echarts
`sampling: 'lttb'` drops spikes**, so a rare latency excursion could vanish from the chart.
An interim attempt to fix payload size by decimating via PromQL `step` **broke rendering
entirely** (histograms ignore `step`, so charts showed no data until zoomed in); that was
reverted to native-full-range and shipped as **v5.16.1** (`dae54d18`, #995) to stop the
bleeding. The real fix had to decimate *without* losing spikes.

## Goal

Decimate server-side to a representation that (a) is small, (b) never drops a spike, and
(c) drills down to native detail on zoom — the same on the axum server viewer and the WASM
static site.

## Key decisions

- **Min/max *envelope*, not a single decimated line.** The server reducer
  (`metriken-query` `query_range_display`, `metriken-query/src/display.rs`,
  `Reducer::Boxplot`) reduces a matrix to per-bucket `{t,min,lo,median,hi,max}`. The chart
  draws a **median line + inner IQR band + outer min/max band**. The outer band carries the
  spikes the median smooths away, so decimation is *lossless for extremes* — this is what
  made "smooth *and* honest" possible, and it's why we did **not** pursue M4 (see NO-GOs).
- **Binary transport, zero-copy decode.** Both series and histogram-heatmap bodies are a
  `[u32 len][JSON header][pad 8B][f64/u32 columns]` blob (`dashboard::display_wire`), decoded
  as `Float64Array`/`Uint32Array` **views** over the buffer (`data.js` `decodeDisplayBinary`
  / `decodeHeatmapBinary`) — no JSON parse of the large arrays.
- **Decimate-then-refetch drill-down.** A full-range fetch is budget-decimated; a zoom sets a
  range override and refetches the narrower window (finer resolution). Line/scatter charts
  swap in the sharper data via a **merge `setOption`** (`chart.js` `_zoomRefine` +
  `base.js sameSeriesShape`) so the line sharpens in place with no notMerge teardown flash.
- **Adaptive budget = chart pixel width, capped by min-samples-per-bucket.**
  `displayBudget()` (`data.js`) = `min(pixelBudget, max(48, native/5))`. The pixel cap fixed
  over-dense fills (viewport×DPR over-fetched ~4× on half-width retina charts); the
  min-samples cap makes the envelope *engage and smooth* jittery per-second signals at
  moderate windows (~5 samples/bucket at a 5-minute window) instead of drawing raw grass,
  while deep zooms (<~4 min) fall back to native.
- **Histogram bucket heatmaps: budget-strided, not native.** `histogram_heatmap(metric,
  stride)` is strided to ~budget columns over the current range and refetched on drill-down —
  the same decimate-then-refetch model, so a 24 h heatmap loads ~500 cols instead of 86 400.
- **WASM parity via the shared `dashboard` crate.** The display query + binary encoders live
  in `dashboard::display_wire`, called by both the axum `range_query_display`
  (`src/viewer/routes.rs`) and the WASM `Viewer::query_range_display`
  (`crates/viewer/src/lib.rs`), with a matching `queryRangeDisplay` in the WASM copy of
  `viewer_api.js`. Byte-identical output by construction — the static site gets real
  decimation, not a JSON fallback. (See the `viewer-parity` skill; `viewer_api.js` is a
  hand-maintained copy, not a symlink.)
- **Robustness: cancellation + LOD cache.** Rapid zooms `AbortController`-cancel the
  superseded window's requests, and a generation guard discards any late response. Decoded
  results are cached per query by `[start,end]` extent and served (clipped) when a tile covers
  the window at sufficient resolution.

## Prerequisite: finer row groups (#1003, `372f6a63`)

`MAX_ROW_GROUP_SIZE` 50 000 → 1800 (`src/parquet_metadata.rs`). Measured on a real recording:
a **5-minute histogram window dropped from ~474 ms to ~20 ms (~24×)** because `rg_classify`
can skip non-overlapping row groups. Independent of the display work but what makes drill-down
feel instant. ~1.75× file-size cost, flagged for the maintainer; merged.

## Measured NO-GOs (banked so they aren't re-paid)

1. **Strided-median fast read** — reading medians with a stride to skip row groups was a
   **measured no-op**: the recorder wrote 2 giant row groups, so there was nothing to skip.
   *Mechanism:* row-group skipping needs many row groups. *Reopen:* on files written with
   finer row groups (now the default post-#1003) it could matter — re-measure.
2. **Histogram cumulative-window quantiles fast path** — computing a window's percentiles by
   subtracting two cumulative histogram snapshots was **decode-bound, not compute-bound**; the
   cost was reading the histogram columns, which the subtraction didn't avoid. *Reopen:* only
   with a columnar histogram decode that's materially cheaper.
3. **Web Worker for decode** — the display binary decode is **already zero-copy**
   (`Float64Array` views over the fetched buffer), microseconds. A worker would add
   structured-clone/transfer overhead to save nothing. *Reopen:* if the decode ever becomes a
   copy.
4. **Web Worker for heatmap aggregation** — after budget-striding, the aggregation is
   ~1–2 ms, and the render is already chunked (echarts custom series `progressive: 5000`,
   `histogram_heatmap.js`). A naive worker's structured-clone of the triples costs about what
   the loop costs. *Reopen:* a *measured* >16 ms aggregation on a real recording; the right
   fix then is a binary heatmap fetch, not a worker.
5. **M4 decimation** — M4 keeps min/max/first/last per pixel column so a *single* line stays
   pixel-identical to full resolution. Our min/max **band already provides** the pixel-accurate
   extremes (M4's core guarantee), and the median is *intentionally* smoothed — M4 would add
   nothing over the envelope. *Reopen:* only for a chart type that draws a single raw line with
   no band. (The one real gap — `multi` charts drawing median-only — was closed by giving them
   the outer band, not M4.)

## Testing

- **Synthetic data with known properties** — `examples/gen_display_testdata.rs` writes single
  + A/B parquets: a gauge with 1-sample spikes at **off-grid** seconds (so a point-sampled
  query steps over them and only the envelope catches them), a bursty counter, and an H2
  latency histogram with tail spikes. `tests/display_synthetic.{sh,test.mjs}` boots the viewer
  and asserts the guarantees exactly: spike survives decimation, envelope ordering
  `min≤lo≤median≤hi≤max`, budget scales `n`, window refetch is finer + shows the raw spike,
  latency p50≈1 ms, heatmap binary decodes, A/B regression detectable. 8/8 pass.
- Unit tests: binary decode round-trips (`display_binary_decode`, `display_heatmap_binary`),
  tile-cache clip/coverage (`display_tile_cache`), display eligibility + boxplot series.
- `tests/viewer_smoke.sh` green after the `display_wire` refactor.
- **Browser verification (2026-07-15):** file-based compare mode verified across gauges (all
  six `ab_*` scenarios), counters, and percentiles — see the validation-pass section below.
  **Live mode also eyeballed (2026-07-15) — fine.** Still not done: an *automated* live-path
  test (needs a mock agent) and a WASM-runtime parity test. (`crates/viewer/build.sh` wasm-pack
  flag conflict fixed and merged in #1007.)

## Landed after the initial writeup

- **A/B compare-mode line-envelopes** — each capture's compare overlay now draws a min/max
  envelope as thin, capture-colored lines (no fill — two filled bands muddy on overlap):
  baseline from `spec.boxplot`, experiment via a per-capture display fetch
  (`data.js queryRangeDisplayForCapture`), rendered by `boxplot.js buildEnvelopeLines`,
  wired through `overlayLine` (`charts/compare.js`) and `extract*Capture` (`viewer_core.js`).
  The median line and its envelope share the display fetch's time grid so they stay aligned.

## A/B compare validation pass (2026-07-15)

Synthetic A/B eyeball scenarios (`AB_SCENARIOS` in `examples/gen_display_testdata.rs` —
regression, improvement, overlap, spread, spike-in-one-capture, crossover; each a gauge with
deterministic per-second wiggle so decimation buckets carry a real min/max) drove a browser
validation pass that surfaced **four bugs no unit test caught**, all now fixed and
regression-tested (`tests/compare_display_strip.test.mjs`, `tests/boxplot_series.test.mjs`):

1. **Compare overlay clobbered by single-capture display bands.** `runQuery` attaches the
   baseline's decimated `spec.boxplot`; `line.js`/`scatter.js` prioritize it over the compare
   `multiSeries`, so the experiment was dropped (gauges) / the bands mixed into the split lines
   (percentiles). Fix: `compare.js stripDisplay()` removes `boxplot`/`boxplotDecimated` from
   every line/scatter compare spec (the per-capture envelope rides in `multiSeries[].boxplot`).
2. **Legend/tooltip marker colors from echarts' default palette.** `buildEnvelopeLines` /
   `buildBoxplotSeries` set only `lineStyle.color`; the marker is read from `itemStyle.color`.
   Fix: set `itemStyle.color` on the median series.
3. **Baseline/experiment on mismatched time grids.** Baseline decimates from its native step to
   the budget grid; the experiment was fetched at the coarse ~500-point `queryRangeFromMeta`
   step (native < budget ⇒ no decimation), so the overlaid series coincided only every few
   samples and the axis tooltip flickered between one series and both. Fix: fetch the experiment
   boxplot *and* the percentile display at the native step (`viewer_core.js`,
   `app.js queryRangeFromMeta.interval`); the percentile scatter builds its series from the
   decimated display (aligned grid), not the raw per-second matrix.
4. **Envelope anchored on the wrong grid.** `overlayLine` rebased each envelope on the matrix
   `timeData[0]`, but the decimated boxplot starts at a bucket-center time — shifting the
   experiment ~1 s off the baseline (residual tooltip flicker after fix #3). Fix: `entryFor`
   anchors each entry on the grid it actually draws.

Also landed: the **A/B divergence band** (`boxplot.js buildDivergenceBand`, computed in
`compare.js divergenceBandFor`, rendered in `line.js`) — a neutral, semi-transparent fill
shading the gap between the two overlaid medians (line + percentile paths). It encodes
difference by *area* (colorblind-safe — no third hue on the blue/green pair), collapses to a
thin line on agreement, and is drawn only on a coincident x-grid (returns null otherwise, so it
never fills across samples taken at different times — which is why the grid-alignment fixes had
to land first).

*Detour worth banking:* the "no data" that appeared mid-session was a **stale ES-module load**,
not a code bug — the viewer serves `lib/*.js` with no `Cache-Control`/`ETag`, so a soft refresh
after a rebuild mixes old and new modules. A hard reload cleared it. Cache headers on the
viewer's JS assets are now a backlog item.

## Deferred (mirrored to `docs/backlog.md`)

- **Live mock-agent + synthetic-live** — live mode connects to an agent msgpack endpoint, not
  a parquet; needs a mock server replaying synthetic snapshots to test the live path (and a
  decision on default rolling window + TSDB retention).
- **Automated browser testing** — drive the viewer headless (Chrome CDP) and assert rendered
  chart options; the synthetic data + scriptable viewer make this tractable now. (A throwaway
  raw-socket CDP driver worked for `Runtime.evaluate` but the app holds a persistent connection
  that hangs `--dump-dom`/load-idle waits — a real harness needs to poll, not wait for idle.)
- **Automated live-path test** — a mock agent replaying synthetic msgpack snapshots, to test
  live mode without a real agent (manual eyeball done 2026-07-15; automation still open).

**Follow-ups landed 2026-07-15** (both traced from this entry's deferred list):

- **Cache headers on the viewer's JS assets** — the embedded-asset handlers (`src/viewer/
  routes.rs` `lib`/`index`) now send an ETag (a hash of the bytes) + `Cache-Control: no-cache`
  and honor `If-None-Match` with a `304`, so the browser revalidates every load and can't serve
  the stale/mixed module set that cost a debugging detour this session (dev-mode `ServeDir`
  already did this). Verified 200→304→200.
- **`reloadCurrentSection` client-only-route guard** — it no longer server-loads
  `/data/<route>.json` for client-only `source/` routes (`src/viewer/assets/lib/app.js`),
  killing the per-selection 404 + console error; the metric browser fetches its own catalog.
