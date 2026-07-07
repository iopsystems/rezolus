# Viewer chart & heatmap UX

- **Opened:** 2026-04-19
- **Status:** SHIPPED — merged (see PRs)
This entry covers per-chart rendering and interaction improvements shipped
between April and June 2026: subgroup layout, quantile-heatmap base chart,
chart-control chrome, single-device suppression, narrow-screen resilience,
tooltip-driven event annotations, and the histogram rate|mean card pair.

**Scope boundary:** the quantile heatmap as used in A/B compare mode
belongs to the compare-mode arc — see `2026-04-21-ab-compare-mode.md`
(once written). This entry covers only the base chart introduced in #868.

---

## What shipped

### Subgroup layout + plot-width control (#864 prerequisite, landed earlier as part of the arc)

**Problem:** within a section group, charts flowed into a flat 2-column CSS
grid with no way to express semantic clustering, force a new row, or
suppress a per-device chart when the recording contains only one device.

**What landed:**

- `crates/dashboard/src/plot.rs` — `PlotWidth { Half, Full }` (Half is
  the serde default, elided from JSON), `SubGroup { name, description,
  plots }`, `Group::subgroups: Vec<SubGroup>`. Legacy `Group::plot_promql*`
  calls delegate to `tail_subgroup_mut()` (lines 134–164 of `plot.rs`) so
  existing dashboard call sites compile unchanged.
- `src/viewer/assets/lib/viewer_core.js` line 521–522 — compat shim that
  promotes a legacy `attrs.plots` to a single unnamed subgroup; active code
  walks `attrs.subgroups` and renders one `div.subgroup` per subgroup with
  optional `h3.subgroup-title` and `p.subgroup-description`.
- CSS: `.group .subgroup`, `.subgroup-title`, `.subgroup-description`,
  `.chart-cell.full-width { grid-column: 1/-1 }` in `style.css`.

**Single-device suppression (#864):** `crates/dashboard/src/plot.rs` lines
1–24 introduce `unique_label_count()` and `metric_unique_label_count()`.
`cpu.rs` calls the file-local `has_multiple_cpus()` (line 6); `gpu.rs`
calls `has_multiple_gpus()` (line 17); both gate `if multi_cpu` /
`if multi_gpu` branches that emit the per-device charts. On a single-CPU
or single-GPU recording the per-device chart degenerates to the aggregate,
so the branch is skipped — leaving one full-width summary chart instead of
an identical pair.

**Unavailable charts (#859):** `src/viewer/assets/lib/viewer_core.js`
lists plots with no data at the bottom of their section rather than
silently dropping them, so a user can see what metrics a section would
show if data were present.

---

### Quantile heatmap — base chart (#868)

**Problem:** histograms were only visualized as a percentile line chart.
For latency distributions with strong tails (e.g. runqueue, block IO) a
heatmap gives better signal.

**What landed:** `src/viewer/assets/lib/charts/quantile_heatmap.js` — a
new chart type that renders a 2-D heatmap of quantile values over time.
Two spectrum modes — Full (all quantiles) and Tail (p50–p100) — selectable
via the chart's control row. The chart is driven by the existing
`histogram_quantiles(...)` PromQL path; no backend changes.

This is the base chart. Its use inside A/B compare mode (side-by-side
heatmaps with a shared color scale) ships separately in the compare-mode
arc.

---

### Chart-control chrome: icon-only top-right buttons (#869) and vertical color legend (#870)

**#869:** Chart controls (expand, select, compare toggle) moved from
inline text links to icon-only buttons in the top-right corner of each
chart cell. This reclaims vertical space and gives the title row room to
breathe. Implemented in `src/viewer/assets/lib/ui/chart_controls.js` and
consumed via line 5 of `viewer_core.js`.

**#870:** `src/viewer/assets/lib/charts/color_legend.js` — a vertical
color-gradient bar with tick labels, shared by `heatmap.js` and
`quantile_heatmap.js`. Without it, heatmaps had no scale reference. The
legend attaches to `.heatmap-legend-bar` (line 158 of `color_legend.js`)
and populates `.heatmap-legend-ticks` (line 219) with labeled stops.

---

### Narrow-screen resilience (#925)

**Problem:** on viewports below ~1200 px the chart title row would
overflow or the Full/Tail spectrum toggle would disappear behind the
chart edge.

**What landed (#925):** `style.css` — `@media (max-width: 1199px)` rules
(around line 2341) ensure the title row wraps and the spectrum controls
remain accessible. The fix is pure CSS; no JS changes. This was prompted
by the comparison-mode workflow where both panes sit side by side on a
1440 px display.

---

### Event annotations — recorder side (#914) and tooltip-driven creation (#929)

**#914 (recorder side):** `crates/dashboard/src/events.rs` gains a
`chart_id: Option<String>` field on `Event` (serde `default` + `skip_if_none`
keeps existing payloads byte-identical). `crates/report-save/src/lib.rs`
gains `events: Vec<Event>` on `ReportPayload` (also `serde(default)`) and
writes a `KEY_EVENTS` footer key when the payload carries events —
`save_single_parquet` and `save_combined_ab_tarball` both updated.

**#929 (tooltip side):** Three new pure-JS modules:

- `src/viewer/assets/lib/events_store.js` — singleton `EventsStore` with
  `seedFromMetadata`, `add`, `all`, `clear`, `subscribe`, and
  `filterForChart({ chartId, scope })`. Seeded from `fileMetadata.events`
  on load in both the server bootstrap (`src/viewer/assets/lib/script.js`)
  and the WASM bootstrap (`site/viewer/lib/script.js`).
- `src/viewer/assets/lib/charts/event_markers.js` — `buildMarkLine(events)`
  returns an ECharts `markLine` config (ns → ms timestamp conversion, one
  vertical dashed line per event). Returns `null` for empty input.
- `src/viewer/assets/lib/event_form.js` — mithril popover anchored to the
  frozen-tooltip `+ Add Event` link. Pre-fills timestamp from the frozen
  x-axis position (captured via `echart.convertFromPixel` in `chart.js`),
  source/node/instance from `spec.opts`, and chart-scoped checkbox (default
  ON). ESC and outside-click dismiss.

`src/viewer/assets/lib/charts/chart.js` subscribes to the `eventsStore`
and calls `_applyEventMarkers()` on every store change, merging the
`markLine` onto `series[0]` via a non-merging `setOption`.
`src/viewer/assets/lib/selection.js` includes `events: eventsStore.all()`
in the save payload so events survive Save as Report round-trips.

**Scope filtering:** per the spec, `filterForChart` excludes an event when
(a) its `chart_id` is set and doesn't match, or (b) a populated scope
field (source/node/instance) doesn't match the chart's scope. A missing
scope field on the event matches anything for that dimension — so a global
event (no fields set) appears on every chart.

**Test coverage:** Rust units for `chart_id` round-trip, `ReportPayload`
events deser (including default-empty), and `KEY_EVENTS` write/no-write in
both single-parquet and A/B tarball paths. Pure-JS `node:test` suites in
`tests/events_store.test.mjs` (10 tests: seed, add, filter, subscribe,
unsubscribe, clear) and `tests/event_markers.test.mjs` (4 tests: null on
empty, ns→ms conversion, name field, missing-timestamp skip). Viewer smoke
(`tests/viewer_smoke.sh`) green throughout.

---

### Histogram rate | mean card pair (#938)

**Problem:** histogram latency charts showed percentile distributions but
no rate (how often events occur) and no mean. Rate counters existed for
some families (blockio, syscall) but had no mean companion; histogram-only
families (tcp_packet_latency, scheduler runqueue/off-cpu/running) had
neither.

**What landed:**

- `crates/dashboard/src/plot.rs` — `RateSource { Counter(String),
  FromHistogram }` enum and `SubGroup::histogram_rate_mean(title_stem,
  id_stem, selector, rate, mean_unit, unit_system)`. The helper appends a
  half-width rate card then a half-width mean card. Rate query is the
  verbatim counter string for `Counter(q)`, or
  `sum(irate(histogram_count(selector)[5m]))` for `FromHistogram`.  Mean
  query is always `histogram_mean(selector)`. Both cards are
  `PlotWidth::Half` (default), so they render side-by-side ahead of the
  percentile chart.

- Wired into `blockio.rs` (Latency + Size, `RateSource::Counter` from
  `blockio_operations{op}`), `syscall.rs` (Overall + per-op, replacing the
  standalone rate plot with the helper — same counter query, plus new mean),
  `scheduler.rs` (runqueue latency, off-cpu, running — all
  `RateSource::FromHistogram`; `scheduler_runqueue_wait` is Σ wait-time,
  not event count, and was explicitly excluded), `network.rs`
  (`tcp_packet_latency`, `FromHistogram`).

- `histogram_mean` and `histogram_count` are PromQL functions added to the
  `metriken-query` crate (external dep, cross-repo prerequisite); the
  dashboard-gen string-level unit tests passed before the dep bump because
  they only assert on query string content, not on query evaluation.

**Known caveat:** `histogram_count` is derived from a lock-free histogram
snapshot and can undercount. Acceptable only in the `FromHistogram` fallback
role where no accurate counter exists; documented in the `RateSource` enum
comment.

**Test coverage:** two Rust unit tests in `plot.rs`
(`histogram_rate_mean_counter_source_emits_two_half_width_plots`,
`histogram_rate_mean_from_histogram_derives_rate_from_count`) assert plot
count, width elision, query strings, and `metric_type`. Per-dashboard
assertions in `blockio.rs`, `syscall.rs`, `scheduler.rs`, `network.rs`
verify the generated JSON contains the new query strings and that
`scheduler_runqueue_wait` does not become a `histogram_count` source.

---

## Key decisions (cross-cutting)

- **Layout decisions at generation time, not JS-side reactive.** Subgroup
  membership and plot width are baked into dashboard JSON when
  `dashboard::generate()` runs against the live `Tsdb`. The `== 1` collapse
  idiom (single device) falls to the expanded layout on an empty Tsdb (debug
  dump) and on live-mode pre-connect, so the collapse fires only when exactly
  one device is positively known. Live-mode layout is not regenerated if a
  second device appears later — accepted limitation for label sets that are
  stable within a session (NICs, block devices, GPUs).

- **`histogram_rate_mean` does not auto-detect pairing; every call site is
  explicit.** Authoring the mapping once, reviewed, avoids a situation where
  a wrong counter silently pairs with the wrong histogram.

- **Events are chart-scoped by default (checkbox ON) when added from a
  tooltip.** The design chose to err toward narrow scope: a marker added
  while looking at a specific chart is most likely intended for that chart.
  The user explicitly unchecks to make it global.

---

## Deferred / reopen conditions

- **Tick-label design review.** `line.js` delegates X-axis ticks entirely to
  ECharts auto-tick with a formatter override; `heatmap.js` and
  `histogram_heatmap.js` style ticks differently; `histogram_heatmap.js`
  hard-codes `splitNumber: 5`. The formatter picked per chart type (relative
  vs. absolute, s/m/h/day) is inconsistent across file mode (long span,
  fixed) and live mode (short growing span). The overlap symptom observed in
  live mode (2026-06-21) is one visible failure of this underlying
  inconsistency. A proper fix requires a span-aware `minInterval` +
  width-bounded `splitNumber` cap + matching formatter, shared across chart
  types via `src/viewer/assets/lib/charts/util/`. Reopen when fixing
  visible tick overlap or starting a chart-rendering quality pass.

- **Single-quantiles-call consolidation.** Count, mean, and percentile
  distributions are all derivable from one `metriken quantiles()` call on a
  single histogram column. The current `#938` design emits separate
  `histogram_mean` and `histogram_count` queries alongside `histogram_quantiles`.
  Consolidating to one call reduces parquet columns and dashboard query
  fan-out. Reopen when touching dashboard chart generation or the viewer's
  histogram/percentile query paths.

- **In-chart label filtering.** Users have no way to hide series by label
  predicate (e.g. "exclude GPU=0") or auto-hide flat/inactive series. On
  recordings where one GPU is never used, the aggregate silently includes the
  dead series. Reopen when working on the chart toolbar or after fielding
  further user reports about misleading averages.

- **Edit/delete existing event annotations.** Event markers are read-only
  post-creation in v1; users must re-save with a modified payload or use
  `parquet annotate --add-events` / `--clear-events` outside the viewer.
  Reopen if a dedicated `/events` management UI is requested.
