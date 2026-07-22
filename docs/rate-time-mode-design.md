# Rate time modes (Grid / Raw)

Status: **Phase 0 done. Engine (increment 1) DONE on the metriken fork —
`RateMode`/`QueryOptions`, grid-phase snap, `CounterGridRate` (interpolated
value + interpolated-window bounds), rate≡irate routing, windowed producers
retired, trait `_opts` surface, integration tests (181 green, clippy clean).
`RezReader` threaded so the rezolus workspace still compiles. Remaining:
viewer backends (routes.rs + WASM) + frontend Grid/Raw dropdown + delete
`rewriteCounterQuery`; then publish + version bump.**
This doc is the handoff for the engine work, which lands in the **metriken
fork** (`~/workspace/brayniac/metriken`, branch `feat/rate-time-mode`), not in
this repo. The rezolus-side wiring stays here and picks up once the engine
commit is on the fork.

> **Design change (2026-07-22):** this started as a three-mode design
> (`Raw`/`Snapped`/`Interpolate`). It collapsed to **two modes** once we found
> the frontend already does the thing the third mode was for — see "The
> realization" below. History is preserved at the bottom under "Superseded:
> the three-mode design."

---

## The problem

The viewer's `rate()` / `irate()` charts plot on a synthetic evaluation grid
`start + k·step`. The viewer sends `start = meta.minTime` — the recording's
first-sample wall-clock time (`src/viewer/assets/lib/data.js`, `defaultRangeFor`).
That's an **arbitrary** timestamp, not aligned to the step boundary, so:

1. **Phase offset.** With 1s samples landing at `.374s` past each second and
   `step=1s`, grid points sit at `minTime + k·1s` = `…, .374, .374+1, …` — never
   on round seconds. A value labeled `10:00:00` actually reflects the window
   ending at `10:00:00.374`.
2. **A/B compare breakage.** Two recordings with different `minTime` phases
   (almost always — different hosts/start times) on the same 1s step are offset
   by `(minTime_b − minTime_e) mod step`. Their grids never coincide, so
   `src/viewer/assets/lib/charts/util/compare_math.js`'s `1e-6` coincidence
   check (`Math.abs(ta[i] - tb[i]) > 1e-6`) bails to `null`.

The engine (metriken-query) generates the grid in
`promql/streaming/{rate,irate,gauge}.rs` — each producer walks a cursor
`cursor_ns = start_ns`, `cursor_ns += step_ns`, and emits `(cursor_ns, value)`.
The phase is set by whatever `start_ns` the caller passes.

## The realization: we already ignore the range window

The frontend already collapses `irate → rate` and forces the range window to
equal the step, in `rewriteCounterQuery` (`src/viewer/assets/lib/data.js:60`):

```js
//   Counter:   irate(m[5m]) → rate(m[Ns])   (true average rate over window)
const rewriteCounterQuery = (query, stepSecs) => {
    const window = stepSecs + 's';
    return query.replace(/\birate\s*\(([^)]*?)\[\d+[smhd]\]/g, `rate($1[${window}]`);
};
```

So the `[range]` token in a rate/irate query is effectively vestigial for us —
the value we actually want is "rate over the step interval," and the
uncertainty is expressed through the acquisition-window confidence band
(`metriken-query` 0.15 measurement-uncertainty work), **not** through
range-based smoothing.

Two problems with the status quo: (a) it's a fragile regex string-rewrite, and
(b) it only fires on the coarse-step path (`stepActive`, `data.js:779`) — at
default granularity `irate` stays true-irate, so the behavior is inconsistent
across granularities.

The fix is to move this into the engine as a first-class rate implementation
that is neither PromQL `rate` nor PromQL `irate` — **it's our own thing** — and
apply it uniformly at every granularity.

## The decision: two modes as a query-engine parameter

Rejected: new PromQL functions (forces query-explorer users to rewrite queries
to switch views). Chosen: a **mode parameter** so the same query text renders
under either mode via a viewer dropdown — the query explorer inherits it free.

| Mode | Timestamp | Value | rate vs irate | Bounds? | Alignable across recordings? |
|---|---|---|---|---|---|
| **`Grid`** (DEFAULT) | `floor(start/step)·step + k·step` | interpolated cumulative-counter Δ across `[t−step, t]` ÷ step | **identical** — both alias one producer | **yes** (from interpolated window edges) | **yes** (shared grid phase + interval-attributable value) |
| **`Raw`** | actual sample `ts` | pairwise Δ between consecutive real samples | identical (already collapse) | from per-sample windows | no (inherently) |

- **`Grid`** is the corrected, formalized version of what the frontend already
  fakes: a per-step rate with edges interpolated from the cumulative counter, a
  fixed grid phase, and a confidence band. It's the only representation whose
  **value** (not just label) is attributable to the grid interval, and the one
  that makes two recordings on a shared grid directly comparable. `rate()` and
  `irate()` both route to it — the `[range]` token is **inert** (documented
  divergence from PromQL; `Raw` is the escape hatch for sample-faithful views).
  It **must live in the engine** because it needs the raw cumulative counter
  samples (once `rate()` has run, they're gone — the viewer can't fake it).
- **`Raw`** is honest truth: points land where samples were taken, values are
  pairwise deltas between real samples. Best for jitter/cadence and
  single-recording analysis. Un-alignable across recordings by construction.

Why no `Snapped`: the old three-mode design kept a "rate over `[t−range]`,
phase-fixed" middle mode as the "smallest change from today." But *today* is
already `rate(m[step])` via the regex, so there is no distinct familiar
behavior to preserve — `Grid` **is** the corrected version of today. Two modes,
not three.

## API shape (additive, zero breakage)

In `metriken-query/src/lib.rs`:

```rust
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum RateMode {
    #[default]
    Grid,
    Raw,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct QueryOptions {
    pub rate_mode: RateMode,
}
impl QueryOptions {
    pub fn with_rate_mode(rate_mode: RateMode) -> Self { Self { rate_mode } }
}
```

**Trait (`MetricsSource`): add NEW default-impl methods; keep existing ones.**
Existing `query_range` / `query_range_display` keep their signatures and
delegate with `QueryOptions::default()`. Concrete impls override the `_opts`
variants. Every existing caller compiles untouched.

```rust
// existing (unchanged signature) — delegates:
fn query_range(&self, expr, start, end, step) -> Result<QueryResult, QueryError> {
    self.query_range_opts(expr, start, end, step, &QueryOptions::default())
}
fn query_range_display(&self, expr, start, end, step, opts: &DisplayOptions)
    -> Result<DisplayResult, QueryError>
{
    self.query_range_display_opts(expr, start, end, step, opts, &QueryOptions::default())
}

// NEW — concrete impls override to thread the mode into the engine; the
// display default post-processes query_range_opts (mirroring the existing
// query_range_display over query_range).
fn query_range_opts(&self, expr, start, end, step, qopts: &QueryOptions)
    -> Result<QueryResult, QueryError>;
fn query_range_display_opts(&self, expr, start, end, step, opts: &DisplayOptions, qopts: &QueryOptions)
    -> Result<DisplayResult, QueryError>;
```

> Trait-design note: verify the existing default `query_range_display` body and
> mirror it exactly for `query_range_display_opts`, just swapping the inner call
> to `query_range_opts`. Concrete `ParquetReader`/`MemoryStore` override
> `query_range_opts` to reach the engine with the mode; the display default then
> Just Works.

## Engine threading (the call chain)

`trait query_range_opts` → concrete impl (`ParquetReader`/`MemoryStore`) →
`QueryEngine::query_range` → `evaluate_expr` →
`streaming::dispatch::try_streaming` → `Ctx` → producers.

Key files (all paths relative to `metriken-query/`):
- `src/lib.rs` — trait + the new types above.
- `src/promql/mod.rs` — `QueryEngine::query_range` (~line 666),
  `evaluate_expr` (~line 492, calls `try_streaming`), `query` (~line 430,
  calls `self.query_range`).
- `src/promql/streaming/dispatch.rs` — `try_streaming` (~line 42, builds `Ctx`),
  `Ctx` struct (~line 84), `build_call` rate/irate arms (~line 335+).
- `src/promql/streaming/rate.rs` — `CounterRate`, `CounterPairwiseRate`, and the
  new Grid producer.
- `src/promql/streaming/irate.rs` — `CounterIrate` (no longer on the default
  path; `irate` now aliases the Grid producer).
- `src/promql/streaming/gauge.rs` — `GaugeStepGrid` (only affected for the
  snapped grid phase under `Grid`; `Raw` is a no-op for gauges).
- `src/parquet.rs`, `src/memory_store.rs` — concrete `MetricsSource` impls.

### What to change in the engine

1. **`Ctx` gains `rate_mode: RateMode`.** `try_streaming` receives it (new
   param) and stores it.

2. **Grid phase snapping (`Grid` only).** In `try_streaming`, when
   `rate_mode == Grid` and `step_ns > 0`, snap the cursor origin:
   ```rust
   let start_ns = match ctx.rate_mode {
       RateMode::Raw  => ctx.start_ns,
       RateMode::Grid => (ctx.start_ns / ctx.step_ns) * ctx.step_ns, // floor to step boundary
   };
   ```
   Pass this snapped `start_ns` into the producers. `end_ns` stays as-is (the
   `while cursor <= end_ns` loop bounds the tail). **Do the snap once in
   `Ctx`/`try_streaming`, not in every producer.**

3. **Grid producer (new — the core of increment 1).** For both `rate` and
   `irate` under `Grid`, build **one** producer that, at each grid point `t`,
   computes the interpolated cumulative counter value at `t` and at `t−step`,
   then `rate = (V(t) − V(t−step)) / step`. Concretely:
   - Find the bracketing samples around each grid edge by `partition_point`
     (same helper the existing producers use).
   - Linearly interpolate the cumulative value at the edge timestamp.
   - Handle counter resets: if `v[i+1] < v[i]`, treat `v[i+1]` as a fresh start
     (match the existing `total_increase += cur_v` reset convention).
   - The `[range]` window in the query text is **ignored** — the grid interval
     `[t−step, t]` is the value's basis. `rate` and `irate` produce identical
     output.
   - **Boundary rule (no extrapolation).** A grid point `t` is emitted only
     when *both* edges `t−step` and `t` fall within the observed sample range
     `[first_ts, last_ts]`. Leading/trailing partial intervals (e.g. the first
     grid point whose left edge precedes the first sample) are dropped rather
     than extrapolated — we never invent counter values outside observed data.
     Consequence: with samples phase-offset from the grid, the earliest/latest
     grid points may be absent; the overlapping interior still aligns across
     recordings, which is what A/B needs. (This is consistent with today's
     `CounterRate`, which skips windows with < 2 samples.)

4. **Bounds under `Grid` (required — do NOT defer).** 0.15's `Point` carries
   `bounds: Option<Band>` and `CounterRate`/`CounterIrate` already consume
   `windows: Option<&[(u64,u64)]>` (per-observation acquisition windows) to
   derive a band. The Grid producer must derive bounds too: interpolate the
   window edges the same way as the values, and compute the band from the
   interpolated elapsed-time span at the grid edges. This is a hard requirement
   — bounds are a headline feature, and `Grid` is the one mode where the value
   is interval-attributable, so dropping its band would be the worst place to.

5. **`Raw` routes to `CounterPairwiseRate`.** In `build_call`, the `rate` and
   `irate` arms: when `ctx.rate_mode == Raw`, build `CounterPairwiseRate`
   (already exists; emits real `ts_cur`, no grid) instead of the Grid producer.
   `CounterPairwiseRate::new(ts, vals, end_ns)` takes no `start_ns`/`step_ns`/
   `range_ns` — it iterates samples. Its output is already
   `Point::at(ts_cur, delta/dur)`, bounds derived from per-sample windows.

6. **Gauges / histograms / scalars:** mode is a no-op except the snapped grid
   phase. `GaugeStepGrid` and the `avg_over_time`/`idelta`/`deriv` producers
   take `start_ns` — feeding them the snapped `start_ns` from step 2 gives them
   the fixed phase for free under `Grid`. No per-producer mode logic needed.

## Tests (metriken-query, `cargo test -p metriken-query`)

Add tests in `src/promql/streaming/rate.rs` (and a trait-level one if practical):

1. **Phase alignment (`Grid`):** samples at `t = 1.0e9 + 0.374e9`, step 1s,
   `start = 1.374e9`. Assert emitted timestamps are `1e9, 2e9, 3e9, …` (round),
   not `1.374e9, 2.374e9, …`.
2. **Grid interpolation correctness:** counter `0, 100, 200, 300` at
   `t = 0,1,2,3s`, step 1s, grid snapped to 0 → rate `100/s` at grid points
   `t=1,2,3`. Then a phase-offset fixture (`t = 0.5, 1.5, 2.5s`, counter
   `0,100,200`) with grid at `0,1,2,3` — under the no-extrapolation boundary
   rule only the interior grid point `t=2` has both edges inside `[0.5, 2.5]`,
   and interpolation must recover `100/s` there (V(2s)=150, V(1s)=50). This is
   the case that distinguishes Grid from a naive window sum. (Done: producer
   tests `grid_rate_constant_counter_yields_constant_rate`,
   `grid_rate_interpolates_between_offset_samples`.)
3. **rate ≡ irate under `Grid`:** the same fixture through `rate(m[..])` and
   `irate(m[..])` yields identical points (timestamps and values), and the
   `[range]` token value doesn't change the result.
4. **`Raw` = real timestamps:** same fixture; `RateMode::Raw` emits points at
   the actual sample `ts` values, not grid points. Values are pairwise deltas.
5. **Grid bounds:** a fixture with acquisition windows produces a non-`None`
   `bounds` band on Grid points; interpolated-edge band matches the expected
   elapsed-time span. (The existing
   `rate_computes_interval_bounds_from_windows` test should still pass or be
   updated to the Grid convention — note why.)
6. **Default is `Grid`:** `QueryOptions::default().rate_mode == Grid`, and
   calling the existing (non-`_opts`) `query_range` produces snapped-grid
   timestamps. This is a behavior change from today's offset grid — that's the
   fix; assert round timestamps. Update any existing test that asserts the old
   offset phase and note why. Counter resets covered in a dedicated case.

Bump `metriken-query` version (0.15.0 → 0.16.0): additive API plus a
Grid-default behavior change → minor bump, not patch. **Do this bump at
publish/PR time, not during local dev:** while the `[patch.crates-io]` block is
active, rezolus requires `metriken-query = "0.15"`, so bumping the path crate to
0.16.0 makes the patch stop matching and cargo silently falls back to the
crates.io 0.15 (old trait) — rezolus then fails to compile against the new API.
Keep the fork at 0.15.0 locally; the 0.16.0 bump + rezolus `= "0.16"` dep bump
land together when the patch block is removed.

## Repo / git facts

- Work in **`~/workspace/brayniac/metriken`** (the fork; `origin =
  git@github.com:brayniac/metriken`, `upstream = iopsystems/metriken`).
- Branch **`feat/rate-time-mode`** exists (from a fast-forwarded `main` at
  `bc960a8`, metriken-query 0.15.0). Local-only. `main` was pushed to origin.
- The fork is identical to upstream/iopsystems at 0.15.0. All file/line
  references above are against 0.15.0 content.
- Verify with `cargo test -p metriken-query` before and after.
- The PR goes from this fork's `feat/rate-time-mode` to `iopsystems/metriken`
  `main`. The upstream PR is the **complete** engine change (both modes), even
  though developed incrementally.

## rezolus-side (done HERE, after the engine commit)

Already done (Phase 0): `Cargo.toml` has a `[patch.crates-io]` block pointing
all five metriken sibling crates at `../metriken/*`. It lets rezolus build and
run end-to-end against the local fork **before publish** — so the whole stack
can be exercised before the upstream PR is frozen. Remove that block once the
new metriken-query is published and bump `metriken-query = "0.16"`.

Pending here once the engine lands on the fork:
- **`src/rez_reader.rs`** — `RezReader` implements `MetricsSource`; override
  `query_range_opts` (and `query_range_display_opts` if not defaulting) to
  forward the mode to the underlying `ParquetReader`s.
- **Viewer backends:** `src/viewer/routes.rs` (HTTP) and
  `crates/viewer/src/lib.rs` (WASM) — accept a `mode`/`rate_mode` query param
  and thread it into `query_range_opts`. Both backends must stay in parity
  (see the `viewer-parity` skill).
- **Frontend:** global "Time mode" dropdown next to Granularity
  (`src/viewer/assets/lib/ui/controls.js` + `data.js`), values Grid / Raw,
  plumbed through `queryRange`/`queryRangeDisplay`
  (`src/viewer/assets/lib/viewer_api.js`). Store it alongside `_stepOverride` in
  `data.js`. The query explorer inherits it. **Compare mode must use Grid**
  (Raw is un-alignable — force it in compare and surface a note).
- **Delete the regex hack:** once the engine does per-step rate uniformly,
  remove `rewriteCounterQuery` and the `delta_counter` rewrite branch
  (`data.js:779-781`). Keep it until the engine lands, then rip it out.
- **Docs:** note that the `[range]` token is inert under `Grid` (rate ≡ irate),
  and `Raw` is the sample-faithful view. Update CLI/README/tooltip.
- **Skills:** run `viewer-parity` and `viewer-smoke` (`tests/viewer_smoke.sh`)
  before opening the rezolus PR.

## Open questions resolved during design

- **Number of modes:** two (`Grid` default, `Raw`), not three — the frontend
  already collapses irate→rate and forces range=step, so `Snapped` had no
  distinct behavior to preserve.
- **rate vs irate under `Grid`:** identical; both alias one producer; `[range]`
  is inert. `Raw` is the escape hatch for sample-cadence responsiveness.
- **Bounds under `Grid`:** required, derived from interpolated window edges — not
  deferred.
- **Default mode:** `Grid` (the corrected, uniform version of what the frontend
  already fakes).
- **Mode toggle scope:** global (next to Granularity), not per-chart.
- **API style:** additive `_opts` methods + `QueryOptions` struct — zero
  breakage for existing callers.
- **Where grid phase is fixed:** engine (`Ctx` snaps `start_ns`), not the viewer.

## Superseded: the three-mode design

The original design had `Raw` / `Snapped` / `Interpolate`. `Snapped` fixed only
the grid phase (value still reflected a PromQL `rate` over `[t−range]`);
`Interpolate` added interval-attributable values. We dropped `Snapped` on
finding that the frontend's `rewriteCounterQuery` already makes "today" equal
`rate(m[step])`, so `Interpolate` (renamed `Grid`) **is** the corrected version
of today and there's no familiar middle behavior worth a separate mode. The
`Interpolate` math and bounds requirements carried over verbatim into `Grid`.
