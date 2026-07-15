# Measurement uncertainty — rate() error bars (query-engine leaf)

- **Opened:** 2026-07-15
- **Status:** Open — design landed, pre-build. Sub-project (4) of the arc: the
  culmination — turn the per-observation acquisition windows into **honest
  uncertainty on `rate()`/`increase()`** via interval arithmetic, at the query
  engine. Consumes the `:window_*` sidecar columns whose skip-seam was left by
  [`.rez` reader ecosystem Phase A](2026-07-15-rez-reader-ecosystem.md) (metriken
  `next` `2e98270`).
- **Arc:** [measurement uncertainty](2026-07-08-measurement-uncertainty.md).
- **Owner:** Brian Martin
- **Repos:** metriken (`~/workspace/metriken`, `next`) — the query engine
  (`metriken-query`) carries windows and computes rate bounds; rezolus — MCP
  `query` surfaces the bounds.

This entry is the design spec (absorbs the brainstorm).

## Why

Every metric carries an honest per-observation acquisition window `[begin,end]`
all the way into the parquet as `<m>:window_begin`/`<m>:window_width` sidecar
columns — but the query engine **skips them** (`parse_schema`,
`metriken-query/src/parquet.rs:1227`, from `.rez` Phase A). So `rate()` still
divides `Δv` by a *point* elapsed time `(last_ts − first_ts)` and returns a
scalar with no uncertainty, even though the elapsed time is only known to within
the two observations' windows. This sub-project closes that: the elapsed time
becomes an interval, and `rate()` returns a **bound**.

## The math (settled during the arc brainstorm)

**Interval arithmetic, no distributional assumptions.** `rate` over a range is
`Δv / elapsed`, where the nominal `elapsed = last_ts − first_ts`. But the first
sample was acquired in window `[b_first, e_first]` and the last in
`[b_last, e_last]`, so the true elapsed lies in `[b_last − e_first, e_last −
b_first]`. Therefore:

```
rate ∈ [ Δv / (e_last − b_first) ,  Δv / (b_last − e_first) ]
        └── lower (widest elapsed) ──┘  └── upper (narrowest elapsed) ──┘
```

The **nominal** value is unchanged (`Δv / (last_ts − first_ts)`); the bound
brackets it. `irate` is the same over the last two samples. Windowless samples
(level-4 packed metrics) → no bound (the interval is `None`, honest: their
acquisition time is the snapshot, already a point).

## Decision: leaf-only propagation

Intervals originate at `rate()`/`irate()` and are **not propagated** through
downstream operators (scope choice, 2026-07-15). A bare `rate(x[5m])` carries a
bound; `increase(x[5m])` (`= rate * seconds`), `rate(x)+rate(y)`, and any
`sum()`/`avg()` **drop** the bound (return `None`). This is the minimal honest
foundation: it lands the window→bound pipeline end-to-end without interval
arithmetic in `BinOp`/aggregators. Tier-1 propagation (interval arithmetic
through binary/scalar ops) and full aggregation semantics are a **later round**;
so are the **viewer error bands** and the **correlation ceiling**.

Consequence made explicit: because leaf-only drops bounds under any operator,
the immediately useful query is a bare `rate(metric[range])`. That is exactly
what a validation surface (MCP `query`) and, later, per-metric viewer rate panels
issue — so the foundation is useful on its own while the propagation/rendering
rounds follow.

## Scope (sub-project 4 of the arc)

**In:**
1. **Windows into samples** — `parse_schema` associates the `:window_begin`/
   `:window_width` sidecars with their base metric; `read_counters`/`read_gauges`
   read them so `Counter`/`Gauge` carry per-sample windows.
2. **`rate()`/`irate()` bounds** — the two producers compute `[lo,hi]` from the
   first/last sample windows.
3. **`Point` carries a bound** — mechanical field addition across the ~11
   producer/consumer sites; only the 2 rate producers set `Some`, everything else
   (binary ops, aggregators, other range fns) sets `None`.
4. **`QueryResult` intervals** — `Sample`/`MatrixSample` gain an optional,
   backward-compatible `interval`/`intervals` field; `collect_to_matrix`
   preserves it.
5. **MCP `query`** surfaces the bounds (JSON already serializes; add a formatted
   CLI display).

**Out (later rounds):** Tier-1 interval propagation through operators; aggregation
interval semantics; **viewer error-band rendering**; **correlation ceiling** in
MCP `analyze-correlation`.

## Section 1 — Windows into the sample series

`metriken-query/src/parquet.rs`:
- `parse_schema` (`:1210`) currently returns a `ColDesc` per metric and skips
  `:window_begin`/`:window_width` (`:1227`). Change: for a base metric `<m>`,
  record the column indices of `<m>:window_begin` (Int64, offset from the row
  `timestamp`) and `<m>:window_width` (UInt64) on that metric's `ColDesc` (two
  `Option<usize>` fields), rather than dropping them. They remain non-metrics
  (never listed).
- `read_counters` (`:1562`) / `read_gauges` (`:1638`) read the window columns
  when present and reconstruct per-sample `[begin_ns, end_ns]`
  (`begin = timestamp + begin_offset`, `end = begin + width`). `Counter`/`Gauge`
  (`src/types.rs:3-13`) gain `windows: Option<Vec<(u64, u64)>>`, aligned with
  `timestamps`/`values`. `None` when the sidecars are absent (windowless).

## Section 2 — `Point` and the rate producers

- `Point` (`src/promql/streaming/mod.rs:64`, today `type Point = (u64, f64)`)
  becomes a small struct: `struct Point { t: u64, v: f64, bounds: Option<(f64,
  f64)> }` (or a 3-tuple — struct preferred for readability). Every `Some((t,
  v))` → `Point { t, v, bounds: None }`; every destructure updates. Mechanical;
  ~11 sites (the map enumerates them: 7 producers, `MergeReduce`, 3 binary ops).
- `CounterRate::next` (`src/promql/streaming/rate.rs:78`) and `CounterIrate::next`
  (`irate.rs:72`) gain access to the series' `windows` and set `bounds = Some((lo,
  hi))` per the math above, when the first/last samples in the range window have
  windows; else `None`. The nominal `v` is unchanged.
- All other producers/consumers set `bounds: None` (leaf-only). `BinOp::apply`
  (`binary.rs`) is untouched (operates on `v` only; the binary iterators emit
  `Point { bounds: None }`).

## Section 3 — `QueryResult` and MCP surface

- `Sample` (`src/promql/mod.rs:35`) gains `interval: Option<(f64, f64)>`;
  `MatrixSample` (`:42`) gains `intervals: Option<Vec<(f64, f64)>>`. Both
  `#[serde(default, skip_serializing_if = "Option::is_none")]` so existing
  consumers and stored JSON are unaffected.
- `collect_to_matrix` (`streaming/mod.rs:119`) maps each `Point.bounds` into the
  parallel `intervals` vec (all-`None` → field stays `None`).
- rezolus MCP `query` (`src/mcp/mod.rs`) already serializes `QueryResult`; add a
  formatted display so `rezolus mcp query file "rate(cpu_cycles[1m])"` prints the
  bound alongside the value.

## Section 4 — Testing

- **Sidecar read:** a fixture parquet with a counter + its `:window_*` sidecars →
  `Counter.windows` is `Some` with the reconstructed `[begin,end]`; a fixture
  without sidecars → `None`. (Extends the Phase-A fixture in
  `metriken-query/tests/integration.rs`.)
- **rate bound (hand-checkable):** two samples `Δv` apart with known windows →
  `rate(x[range])` returns the nominal value **and** `bounds` equal to the
  hand-computed `[Δv/(e_last−b_first), Δv/(b_last−e_first)]`. A windowless fixture
  → `bounds: None`.
- **Leaf-only:** `rate(x)*60` and `rate(x)+rate(y)` return `intervals: None`
  (bound dropped under an operator) — locks the scope decision.
- **Back-compat:** a query with no rate and old stored JSON round-trips unchanged
  (the new fields default to `None`/absent).
- **Nominal unchanged:** the point value of `rate()` is byte-identical to before
  (bounds are additive, never alter the nominal).

## Fit with the arc / principles

- This is the **payoff** of the whole arc: the windows recorded since Phase 1 (the
  drivehealth pilot) finally become uncertainty in analysis. Phase A of
  sub-project 3 deliberately left this exact seam (`parse_schema` skip → read).
- **No agent-side clock semantics** (Principle 10) — the engine keys entirely on
  windows the samplers already recorded; no new time authority.
- Sets up the deferred rounds cleanly: Tier-1 propagation extends `BinOp` to
  interval arithmetic over the same `Point.bounds`; viewer bands render the same
  `MatrixSample.intervals`; the correlation ceiling consumes them from the final
  `QueryResult`.

## Open questions / spec-time details

- **`Point` struct vs 3-tuple** — struct is clearer and makes the `bounds: None`
  default obvious at the ~11 sites; confirm no hot-loop perf regression (it's the
  same data, stack-allocated).
- **Bound orientation when `Δv < 0`** (counter reset mid-range) — `rate` already
  handles resets by summing per-step increments; confirm the bound is computed
  from the same `Δv` the nominal uses (reset-adjusted), so `lo ≤ nominal ≤ hi`
  always holds. Add a reset fixture.
- **`increase()` desugaring** — `increase(x[r]) ≡ rate(x[r]) * r_seconds` is a
  scalar op, so leaf-only drops its bound. If `increase` bounds are wanted sooner
  than the full propagation round, a targeted exception (scale the rate bound by
  the constant) is a small follow-up — noted, not in scope here.
- **Windowless-within-range** — if some samples in a rate range have windows and
  others don't (mixed sampler), define the bound from the first/last samples that
  *do* carry windows, or fall back to `None`; settle in the plan (default: bound
  only when both the first and last in-range samples have windows, else `None`).
