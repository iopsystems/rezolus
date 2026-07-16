# Measurement uncertainty — histogram value uncertainty (bucket resolution)

- **Opened:** 2026-07-16
- **Status:** LANDED (metriken `next` `034cde2`). `histogram_quantile`/
  `histogram_quantiles` results carry an honest **value** band = the containing
  bucket `[start, end]` (from the histogram's `grouping_power`/`max_value_power` —
  no windows), and `build_binary` gained a `(Materialized, Scalar)` arm so the
  ns→s unit conversion (`histogram_quantiles(...)/1e9`) keeps a scaled band (also
  closing the prior outright-`Unsupported` gap). Reuses the `QueryResult.intervals`
  + MCP-display + viewer-band infrastructure from
  [rate() error bars](2026-07-15-rate-error-bars.md). Live:
  `histogram_quantile(0.99, scheduler_runqueue_latency)/1e9` returns a band with
  real bucket-quantization width; linear-region buckets are exact (zero width).
  A follow-on round of sub-project (4).
  - **Deferred:** `histogram_sum`/`histogram_mean` (per-bucket band accumulation),
    `histogram_count` (exact), and series-op-series against a histogram result.
- **Arc:** [measurement uncertainty](2026-07-08-measurement-uncertainty.md).
- **Owner:** Brian Martin
- **Repos:** metriken (`~/workspace/metriken`, `next`) — the query engine
  (`metriken-query`) histogram path.

This entry is the design spec.

## Why

The rate-error-bar rounds now band every operator the dashboards issue over
**rates**. The largest untouched dashboard family is **histogram/latency queries**
— `histogram_quantile(q, latency)` / `histogram_quantiles([qs], latency)` for
p50/p99 latencies. These currently carry **no band**.

For histograms the valuable uncertainty is **not temporal** (the acquisition
window) — it is the **value quantization** inherent to the H2 histogram. A
quantile result lands *in a bucket*, and that bucket has a known value range
`[start, end]`. The true quantile lies somewhere in that range; the reported point
is only known to bucket resolution. That range — derived directly from the
histogram's `grouping_power`/`max_value_power` (already stored in the parquet, and
required to reconstruct the histogram from its bucket columns) — is a hard,
computable band on the value, needing no windows.

## The model (and why it's trivial)

The engine already computes the bucket. In `apply_quantiles`
(`metriken-query/src/promql/streaming/histogram.rs`):

```rust
let r = CumulativeROHistogram32Ref::from_parts_unchecked(config, idx, cnt);
let qr = r.quantiles(quantile_floats)?;
if let Some(bucket) = qr.get(q_key) {
    out[out_idx].push((t_sec, bucket.end() as f64));   // <- value = bucket.end()
}
```

The nominal value **is** `bucket.end()`, and the band is exactly
`[bucket.start(), bucket.end()]` — the bucket the quantile falls in. The nominal
sits at the band's upper edge, so `start ≤ nominal ≤ end` holds by construction.
No new computation: the bucket is already in hand.

## The one wrinkle: histogram output is `Built::Materialized`

`histogram_quantile` output is a terminal `Built::Materialized { MatrixSample }`,
not a streaming `Point` iterator. `build_binary`
(`metriken-query/src/promql/streaming/dispatch.rs:236`) currently **rejects** any
binary op against it: `"binary op against a histogram_quantile result not
supported"` — so `histogram_quantile(...) / 1e9` (the ns→s unit conversion the
dashboards use) falls back to the **eager** engine, which has no bands.

So banding the *bare* quantile is easy, but banding the *actual dashboard latency
panels* also requires teaching the streaming engine to apply a scalar op to a
materialized histogram result (scaling its intervals) — which also closes that
pre-existing "not supported" gap.

## Scope (this round)

**In:** `histogram_quantile` / `histogram_quantiles` value bands, and the
`(Materialized, Scalar)` binary support so unit-converted percentiles keep the
band. **Out (later):** `histogram_sum`/`histogram_mean` (per-bucket band
accumulation — a different formula), `histogram_count` (exact), and any
series-op-series against a histogram result.

## Section 1 — Bucket band on the quantile

`histogram.rs`:
- `apply_quantiles` pushes `(t_sec, bucket.end(), Some((bucket.start(),
  bucket.end())))` — the output rows carry the band. Change `out` from
  `Vec<Vec<(f64, f64)>>` to carry the optional band per point (a third tuple
  element, or a small row struct).
- The `MatrixSample` construction sites in `histogram.rs` (currently
  `intervals: None`) set `intervals` from the collected bands.

Confirm the `histogram` crate's `Bucket` exposes `start()`/`end()` (it does —
`apply_quantiles` already calls `bucket.end()`); `start()` is the bucket's lower
edge (the previous bucket boundary), giving the full `[start, end]` value range.

## Section 2 — `(Materialized, Scalar)` binary in the streaming engine

`dispatch.rs` `build_binary`:
- Add match arms for `(Built::Materialized, Built::Scalar)` and
  `(Built::Scalar, Built::Materialized)`: apply the scalar op to each
  `MatrixSample`'s `values` **and** scale/normalize its `intervals` by the same
  op (reuse the scalar-interval logic from `ScalarBroadcast` — apply to both band
  endpoints, `min`/`max` to normalize sign; drop on div-by-zero). Return a
  `Built::Materialized`.
- This keeps `histogram_quantile(...) / 1e9` (and `* 8`, etc.) in the banded path
  and fixes the current outright-`Unsupported` gap.

## Section 3 — reuse downstream

No new plumbing: the bands ride `MatrixSample.intervals` (already serialized in
`QueryResult`, displayed by MCP `query`, and rendered as viewer error bands). A
latency panel's `histogram_quantiles(...)/1e9` band renders exactly like a rate
band.

## Testing

- **Bucket band:** a fixture histogram with a known bucket layout →
  `histogram_quantile(0.99, m)` returns a `MatrixSample` whose `intervals[i]` ==
  the containing bucket's `[start, end]`, and the value `== end` sits inside.
- **Scalar-scaled band:** `histogram_quantile(0.99, m) / 1e9` →
  intervals scaled by `1/1e9`; the value stays inside.
- **No regression:** a non-histogram query is unaffected; a histogram query
  without the new binary arm (bare quantile) still works.

## Fit with the arc

- A **distinct uncertainty model** from the rate rounds: value quantization
  (bucket resolution) vs. temporal (acquisition window). Same *surface*
  (`QueryResult.intervals`), different *source*.
- Closes a real engine gap (`(Materialized, Scalar)` was outright unsupported in
  streaming), independent of the bands.
- Leaves `histogram_sum`/`mean` (per-bucket accumulation) and the correlation
  ceiling as later rounds.

## Open questions / spec-time details

- **Nominal at the edge vs. midpoint** — the value is `bucket.end()` today (band
  upper edge). Keeping it avoids changing existing query values; the band
  `[start, end]` still contains it. A future refinement could report the bucket
  midpoint as the nominal (band-centered), but that changes displayed latencies —
  out of scope here.
- **`start()` for the first bucket** — confirm `bucket.start()` is well-defined
  for the lowest bucket (0 or the histogram's min); if it returns something odd,
  clamp to a sane lower edge.
- **Series-op-series against a histogram result** (`histogram_quantile(a) /
  histogram_quantile(b)`) — rare; stays `Unsupported`/eager for now.
