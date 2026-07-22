# Viewer band system — view split, decimation budget, worst-case hull (design)

- **Opened:** 2026-07-21
- **Status:** OPEN — design discussion landed pre-build; resuming 2026-07-22.
  No implementation yet. Grew out of explaining the #1017 uncertainty bands
  (swatch row PR #1021, mean-vs-median entry
  [2026-07-21](2026-07-21-decimation-mean-vs-median.md)).
- **Design record:** this entry absorbs the discussion; no separate doc.

## Problem

A decimated chart stacks up to three same-hue translucent fills
(`buildBoxplotSeries`, `src/viewer/assets/lib/charts/boxplot.js`): inner IQR
at opacity 0.45, outer min–max at 0.28, and the measurement-uncertainty
ribbon at 0.22 with 0.65-opacity borders, drawn above the spread fills.
Because they stack, the on-screen result is composite alpha levels (outer
alone ≈ 0.28, inner+outer ≈ 0.60, all three ≈ 0.69) with soft boundaries.
The swatch row (PR #1021) labels what shades *mean* but cannot make the
regions *separable* — user-verified: even knowing the semantics, the fills
are hard to tell apart. Fill lightness carries almost no signal between the
ribbon (0.22) and the envelope (0.28).

## Decision 1 — split the two smears into distinct views

The fills conflate two smears with different epistemics:

- **Spread** (inner/outer bands): *what happened* — the signal varied; the
  canvas can't show every sample. Summarization forced by pixels.
- **Measurement** (ribbon, rate bounds): *what we can claim* — limits of the
  measurement process (acquisition-window timing, value quantum).

One is variance in the world; the other is confidence in the instrument.
**They will not be overlaid on the same chart at the same time.** Two views:

- **Summary view** (default): median line + IQR band + min/max envelope.
  Pure "what happened".
- **Measurement view**: median line + timing-bounds ribbon + worst-case
  hull (Decision 3), and the home for any rate-normalization treatment.
  No spread bands.

Key simplification: at native resolution the spread bands collapse by
construction, so **the native chart is already the pure measurement view**
— the toggle only means anything in decimated mode. Each view carries at
most one wash plus lines, which dissolves most of the legibility problem
without a deeper redesign (envelope-as-lines inside summary view — the
`buildEnvelopeLines` treatment compare mode already uses — remains a
nice-to-have, not a rescue). Swatches become per-view (2–3 entries).

**Open:** toggle scope. Leaning: per-chart control (the Mean/Total-toggle
idiom) with a sticky global default — "how sure are we" is usually asked of
one suspicious chart, but defensive-planning sessions flip everything.

## Decision 2 — decimation budget policy (replaces the current formula)

Current formula (`displayBudget`, `src/viewer/assets/lib/data.js`):
`min(pixelBudget, max(48, ceil(native/5)))`, raw passthrough only when the
budget ≥ native.

**Findings against it:**

- The 48 floor was documented as protecting detail ("the cap sits above the
  native count, so the server returns native resolution"; journal
  [2026-07-13](2026-07-13-viewer-display-decimation.md): "deep zooms
  (<~4 min) fall back to native"). Both claims are wrong for
  48 < native < 240: there `max(48, ceil(native/5))` = 48 < native, so the
  floor acts as a *cap* — e.g. a 2-min window (130 samples at 1 s) forces 48
  under-filled buckets (~2.7 samples each) whose quantile stats are mostly
  interpolation. True native passthrough happens only at ≤ 48 samples.
- The passthrough threshold is width-independent, which is dimensionally
  wrong: readability of a raw line is a pixels-per-sample property.

**Worst-case readability analysis** (the pathological case: the value
alternates between two extremes every sample):

- Budget pixels are **CSS px** by deliberate prior decision (`pixelBudget`
  ignores devicePixelRatio; ×DPR was measured as ~4× over-fetch with no
  visible gain). CSS px is the perceptual unit; retina panels sharpen
  strokes but don't make 2-CSS-px spacing resolvable.
- Stroke width is 1.5 px; 2 px/sample leaves sub-pixel air → antialiased
  smear. ~3 px/sample is bare separability; **5 px/sample** gives 10 px per
  worst-case excursion cycle and legible `step: 'start'` treads.

**Layout facts** (`charts.css`, `style.css`): two-up grid only at ≥1200 px
viewport; below that, single-column (cells are *wider*). Minimum realistic
two-up cell ≈ 470 CSS px; typical 590–830. ~300 px cells occur only on
phones (~393 CSS px viewport), not a design target; `BUDGET_MIN = 300` is
the defensive clamp.

**Alternatives compared** for the band between the raw gate and 240 samples
(the two formulas are identical above native = 240):

- **A — element gating:** keep the 48 floor; suppress the inner band when
  buckets hold < 5 samples (median + envelope only there). Softer zoom
  transition (2.5–3.5× at 590–830 px cells) and finer time resolution in
  the valley; but the visual grammar changes with zoom, and the dark band
  pops in at 240.
- **B — honest buckets (CHOSEN):** `buckets = min(px, ceil(native/5))`, no
  floor. Every bucket holds ≥ 5 samples; the full line + dark + light
  structure is preserved everywhere above the gate. Entry blockiness is
  scale-invariant (25 px-wide columns — effectively a candlestick chart
  that refines continuously); gate transition is a constant 5× (converges
  with A at 1200 px cells). At exactly 5 samples, type-7 quantiles make the
  five-number summary **exact order statistics** (rank `q·(n−1)` is integer
  for q ∈ {0,¼,½,¾,1} iff `4 | n−1`; n = 5 is the smallest — the fencepost
  argument; also the undocumented justification for `/5` over `/4`:
  4 quarters need 5 boundary samples).

**Final policy:**

```
native ≤ px/5   → raw passthrough        (≥5 CSS px per sample)
native > px/5   → buckets = min(px, ceil(native/5))   (≥5 samples per bucket)
```

Two constants, both 5, **separately derived** — `MIN_PX_PER_SAMPLE`
(perceptual: stroke + air + treads) and `MIN_SAMPLES_PER_BUCKET`
(statistical: exact five-number summaries). Keep two named constants; do
not merge symbols — either can legitimately move alone. Pleasant
coincidence worth locking in: at `BUDGET_MAX = 1200`, `px/5 = 240` — the
raw gate meets the full-smoothing crossover and the policy is perfectly
two-regime on max-width charts. **A is the recorded fallback** if the 5×
gate step or phone-width cells (~14 columns) bite in practice.

## Decision 3 — worst-case hull (measurement view)

Only the median currently carries interval treatment (`unc_lo`/`unc_hi` =
median of per-sample interval edges, `metriken-query` `display.rs`). That
exclusivity is presentation, not math: min/max/quantiles are **monotone**
statistics, and for monotone statistics the interval extension is exact —
true bucket-min ∈ `[min(lo_i), min(hi_i)]`, true max ∈
`[max(lo_i), max(hi_i)]`. The **interval hull** `[min(lo_i), max(hi_i)]` is
the tightest range guaranteed to contain every true instantaneous value in
the bucket — statistic-free, degenerates to the per-sample band at native
resolution, and answers the defensive-planning question ("worst the system
could have been") — the viewer-side shape of the arc's open
"alerting/thresholds on uncertain values" problem
([2026-07-08](2026-07-08-measurement-uncertainty.md), non-goals).

Constraint: the hull shows *possibility, not observation* — where spikes
**could** hide, including ones that didn't happen. It must not share the
observed envelope's visual voice (candidate: dashed hull lines outside the
observed extremes) and needs its own swatch. Wire cost: two per-bucket
columns + a monotone one-liner in the reducer.

## End-state framing — unsnapped timestamps

The arc's stated end-state (drop the unified-timestamp myth; multi-timeline
plotting; as-of joins on the anchored clock) plots points at true
acquisition instants and computes rate denominators over true elapsed time.
That dissolves the grid-induced component of uncertainty — for tight-window
samplers (BPF windows measured ~1000× tighter than the fleet interval)
residual bounds go sub-pixel and draw nothing (existing collapse
suppression). What remains is irreducible: **window width** (drivehealth's
0.18–2.3 s reads), **value quantum** (tick counters, histogram buckets),
**cross-host clock uncertainty**, **cross-series alignment**.

Viewer consequence: the measurement view is a **waypoint, not the
destination**. In the unsnapped world it contracts from "second mode" to
**exception surface** — engaging only where residual uncertainty is
visually material (mechanical rule: band ≥ ~1 px), ideally self-announcing
("this chart carries material measurement uncertainty") rather than a mode
the user must know to reach for. Design the toggle now so it can become
that later.

## Related decisions recorded elsewhere

- Jitter timeline renders via echarts LTTB (no envelope guarantee); parked
  behind the CDF/PDF distribution panels — see `docs/backlog.md`
  (simple-capture section, decision 2026-07-21).
- Mean-vs-median for the decimated line —
  [own entry](2026-07-21-decimation-mean-vs-median.md).
- Shade-meaning swatch row — PR #1021 (per-view swatches follow this
  design).

## Plan (resume 2026-07-22)

1. Sign off the view split + toggle scope (the one open question).
2. Implement the budget policy (TDD via an exported `displayBudget`;
   regimes, the 25 px-entry property, the 1200 px convergence as cases);
   fix the `data.js` comment (both derivations) and the 2026-07-13 entry's
   "<~4 min → native" line.
3. Implement the two views + per-view swatches; then the hull.
