# Measurement uncertainty — correlation uncertainty range (interval r-band)

- **Opened:** 2026-07-17
- **Status:** LANDED (rezolus `measurement-uncertainty-phase-1`). The MCP
  `analyze-correlation` tool now reports a **measurement-uncertainty range**
  `[r_lo, r_hi]` beside each Pearson correlation: the span of correlation still
  achievable once every input point is allowed to vary within its
  rate/histogram acquisition band. Pure interval arithmetic — no distributional
  assumption. Live-validated on a 20 s `.rez`:
  `sum(rate(network_bytes[1m]))` vs `sum(rate(network_packets[1m]))` →
  `0.8219 [0.8206, 0.8231]` (nominal contained, uncertainty narrow); tight-window
  metrics (cpu_cycles/instructions) collapse to a degenerate band
  `0.9994 [0.9994, 0.9994]`, the honest "measurement barely affects this r".
- **Arc:** [measurement uncertainty](2026-07-08-measurement-uncertainty.md).
  Round 2 of the post-rate follow-on sequence (histogram coverage → **correlation**
  → compare/multi-series bands).
- **Owner:** Brian Martin
- **Repos:** rezolus (`~/workspace/rezolus`) — `src/mcp/correlation.rs`. MCP-only
  (the viewer has no correlation surface).

This entry is both the design spec and the outcome record.

## Why

The rate/histogram rounds put a `[lo, hi]` band on every *value* the dashboards
plot. The MCP `analyze-correlation` tool consumes those same series but reduced
them to bare `.values` (`prepare_series_data`), discarding the bands. Yet a
correlation computed from uncertain points is itself uncertain: if each `xᵢ`/`yᵢ`
is known only to bucket/window resolution, the Pearson `r` is only known to some
range. Reporting a bare `r = 0.88` overstates confidence when the inputs are
measurement-limited.

## The model (chosen: interval range of r)

Three models were on the table (see the arc's decision log):

1. **Interval range of r** *(chosen)* — recompute Pearson `r` with points pushed
   to their band edges; report the achievable `[r_min, r_max]`. Pure interval
   arithmetic, matching the arc's standing "no distributional assumptions" rule.
2. **Attenuation/disattenuation ceiling** — treat band half-width as
   measurement-error magnitude, compute reliability, disattenuate `r`. Tight and
   standard (psychometrics) but *requires* an error-variance model — breaks the
   interval-only stance. Rejected.
3. **Reliability caveat only** — a "measurement-limited" flag, no numeric range.
   Cheaper but less useful. Rejected in favour of (1).

The exact range of Pearson `r` over box-constrained points is non-convex
(NP-hard in general), so (1) is computed by an **approximation that stays on the
honest side**: a greedy corner coordinate-search (below) that only ever places
points at valid in-box positions. Every `r` it reports is therefore *achievable*
by real points, and because the box is connected the whole reported
`[r_min, r_max]` is achievable — a **subset** of the true range. The tool never
*under*-states which `r` values are possible; it may under-state the width (the
true range could be wider), which is the safe direction for an honesty tool. The
nominal `r` is folded in explicitly, so `r_min ≤ r_nominal ≤ r_max` always holds.

## What is bounded

The existing pipeline is `r = Pearson(detrend(x), detrend(y))` (normalization is
scale-invariant, so it drops out). The trend line is estimated from the
**nominal** values and subtracted from all three of `(nominal, lo, hi)` — a fixed
per-index shift that preserves each band's width and keeps the box's nominal `r`
equal to the reported `r`. `r` is then bounded over the band-perturbed detrended
points at the **optimal lag** already selected by the nominal cross-correlation.

## The algorithm

`correlation_band(x: &[(nom, lo, hi)], y: &[(nom, lo, hi)]) -> Option<(f64, f64)>`:

- Maintain the five Pearson running sums (`Σx, Σy, Σxx, Σyy, Σxy`) so moving one
  point updates them and recomputes `r` in O(1).
- **Max pass:** for each point `i`, holding others fixed, evaluate the 4 box
  corners `{loᵢ,hiᵢ}×{loᵢ,hiᵢ}` and keep the one that most increases `r`; repeat
  passes until a full pass makes no change. **Min pass:** same, minimizing.
- Run from 5 seeds (nominal, lo/lo, hi/hi, lo/hi, hi/lo) to escape local optima;
  take the widest `[min, max]` found and fold in `r_nominal`.
- Clamp to `[-1, 1]`; return `None` for `< 3` points or a constant series
  (undefined correlation).

Corner-only search is a heuristic (the single-point optimum of the `r` *ratio*
need not sit at a corner), which is why the result is documented as an achievable
subset rather than the exact range. Cost is `O(passes · n · 4)` per seed per
pair — negligible next to the existing lag sweep.

## Plumbing

- `prepare_series_boxes(series) -> Option<Vec<(f64,f64,f64)>>` — detrended
  `(nominal, lo, hi)` per point, or `None` when the series carries no
  `intervals`. `correlation_band_at_lag` slices both series to the optimal-lag
  overlap (mirroring `calculate_correlation_at_lag`) and calls `correlation_band`.
- `CrossCorrelationResult`, `SeriesCorrelation`, and `CorrelationResult` gain an
  `r_band: Option<(f64,f64)>`; the overall-summary tuple (`CorrSummary`) carries
  it so the strongest-|r| pair's band becomes the top-line band.
- `format_correlation_result` prints `r=0.8219 [0.8206, 0.8231]` on the top line
  and each series-pair line, with a footer explaining the range. When no input
  band is present (plain parquet, or gauge inputs without windows) nothing
  changes — no band is emitted.

## Where bands come from

Bands ride `MatrixSample.intervals`, populated only when the source carries
per-observation acquisition windows: a **`.rez`** archive or a live agent. A plain
`.parquet` has no window sidecar columns, so rate() over it has no band and the
correlation shows none — verified: the same query bands on `/tmp/corr.rez` and is
bare on `/tmp/corr.parquet`.

## Testing

Unit (`src/mcp/correlation.rs` tests):
- `zero_width_bands_collapse_to_nominal_r` — degenerate boxes → `[r, r]`.
- `wide_bands_widen_and_contain_nominal` — perfectly-correlated nominal with wide
  `y` bands drops `r` below 1, stays ≤ 1, contains nominal.
- `bands_can_lift_negative_correlation` — anti-correlated nominal (`r=-1`) lifts
  toward 0, stays ≥ -1, contains nominal.
- `series_correlation_carries_r_band_when_inputs_have_bands` /
  `…has_no_r_band_without_input_bands` — pipeline threading both ways.

Live: 20 s `.rez` from a local agent (above).

## Fit with the arc

- Same *surface* as the rate/histogram rounds (`intervals`/`r_band` display),
  a *third uncertainty model*: not a value band but the **range of a derived
  statistic** over value bands. Interval arithmetic throughout — consistent with
  the arc's no-distribution stance (which is exactly why model (2) was rejected).
- MCP-only; the viewer has no correlation panel. Round 3 (compare/multi-series
  value bands in the viewer) is the remaining follow-on.

## Open questions / future

- **Tighter range** — corner coordinate-search gives an achievable subset. A
  future exact/outer bound (e.g. branch-and-bound, or a projected-gradient inner
  search plus a convex outer relaxation) could report the true range, at real
  cost. Not worth it until a real recording shows a correlation whose band is wide
  enough to change a conclusion.
- **`discover_correlations`** reuses `calculate_correlation`, so it inherits the
  band for free; it does not yet rank or filter by band width.
