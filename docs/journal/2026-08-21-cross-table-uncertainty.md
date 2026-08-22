# Cross-table uncertainty: widen the band, drop the refusal

- **Opened:** 2026-08-21
- **Status:** **IMPLEMENTED** (metriken PR #135; rezolus drops the refusal).
  Two corrections since the entry was merged — see below.
- **Arc:** closes the loop the acquisition-groups work opened
  ([stage 4](2026-08-20-stage4-native-v3-ingest.md)). Groups made each
  reading's window honest; this makes *combining* two readings honest.
- **Owner:** Brian Martin
- **Repos:** rezolus

## Why

`.rez` is now the better recording in every measurable way — 0.386× the
size, 0.52–0.60× the query time, real acquisition windows — and the agent
defaults to producing the format that fills it (#1076). But it cannot answer
questions parquet answers, including questions our own dashboards ask.

Recorded from one agent, same 25 s window, one file each:

| query | `.rez` | parquet |
|---|---|---|
| `sum(irate(cpu_usage[5m]))` | 25 points | 25 points |
| `sum(irate(cpu_usage[5m])) / cpu_cores / 1e9` | **refused** | 25 points |
| `…cpu_tsc… / cpu_mperf… / cpu_cores` (Frequency chart) | **refused** | 25 points |
| `sum(irate(cpu_dtlb_miss[5m])) / sum(irate(cpu_instructions[5m]))` | **refused** | 25 points |

The refusal reads:

```
cross-timeline query spans samplers cpu_cores, cpu_usage —
per-sampler alignment (interpolate/decimate) is not yet supported
```

So the format with the best data is the one that fails a stock chart.

## How big the gap actually is

Auditing all 313 dashboard queries against a live agent's metric→sampler
map, 36 (11%) span samplers. That number is misleading in both directions,
and taking it apart is what makes this tractable:

| category | count | what it needs |
|---|---|---|
| divide by `cpu_cores` | 19 | it is a scalar |
| `gpu_amd_smi` + `gpu_nvidia` | 16 | nothing — mutually exclusive vendors |
| `cpu_dtlb ÷ cpu_perf` | 1 | a genuine join |

`cpu_cores` is a **single-member group** holding a CPU count. The GPU pairs
are an artifact: the metric name exists in both vendors' schemas, but only
one populates on a given host (on the test box both are 0/448 and 0/672
non-null). So exactly **one** query in the whole dashboard set is a true
two-sampler join — and both of its operands are PMU sweeps on the same tick.

This is not a missing subsystem. It is one refusal drawn too wide.

## The measurement that decides the design

Across one snapshot, 44 of 48 groups carry windows, and the **entire scrape
spans 6.139 ms**. Individual windows are microseconds: the widest is 911 µs,
`cpu_migrations` is 3.0 µs.

For the pairs that matter:

- `cpu_dtlb` vs `cpu_perf` — begins **1.555 ms** apart
- `cpu_usage` vs `cpu_cores` — begins **3.014 ms** apart

At a 200 ms interval that is 0.8%; at the 1 s default, 0.16%. Parquet
already performs this join. It simply does not tell you the 1.5 ms is there.

## The rule

**A group is one read.** Its members were sampled together by construction,
so two metrics from the same table have *no* alignment error between them —
not a small one, zero. The error is created by crossing a table boundary,
and nowhere else. That is the whole rule:

1. **Propagate bands through series-op-series.** *Already done* — see the
   correction below. `ZipMergeBinary` combines both operands' bands through
   `combine_bounds`/`interval_binop`, so a division of two rates already
   carries one.
2. **Same table → no alignment term.** Zero, by construction.
3. **Cross table → widen by the union span** of the two windows.
4. **Cross cadence needs no special case** — see the second correction
   below. It neither stays refused nor blows the band up; it decimates to the
   slow side on its own.

### Correction (2026-08-21, after the entry was merged)

Point (1) as originally written was **wrong**: it claimed series-op-series
bands were dropped. They are not. Verified against a real `.rez`:

```
sum(irate(cpu_usage[1m])) / sum(irate(softirq[1m]))
  First: … = 181140.15364092906  [171273.716427, 191590.779931]
```

The claim came from reading truncated command output (`head -4` cut the
`First:` line) plus a stale line in `CLAUDE.md` saying "series-op-series and
non-rate queries show none". Both are corrected.

This makes the remaining work **smaller and better founded**, not larger.
The machinery to carry a band through a binary op exists and is tested
(`interval_binop`, `combine_bounds`). What is missing is precisely the
alignment term: the band above combines the two rate bands and adds nothing
for the fact that `cpu_usage/cpu_usage_cpu` and `cpu_usage/cpu_usage_softirq`
are *different tables*, read at different instants. So today's cross-table
bands are **too narrow**, which is the one direction they must not be — and
that is exactly what rule (3) fixes.

### Correction 2 (2026-08-22): cross-cadence was wrong twice over

The entry said cross-cadence combinations "stay refused" and, in discussion,
that their band would explode and thereby signal meaninglessness. Both are
wrong. Measured on a real archive, `cpu_usage` at ~0.199 s against
`drivehealth` at ~19.6 s:

```
sum(irate(cpu_usage[5m])) / avg(drive_temperature)
  → 3 points,  band [28558194, 28801420]   (~0.85% wide)
sum(irate(cpu_usage[5m]))
  → 29 points
```

The result comes out at the **slow side's cadence** — 3 points, not 29 —
because `ZipMergeBinary` emits only where both sides have a point. Decimation
is already the behaviour, for free, from timestamp intersection.

And the band stays **tight**, because it reflects the two *acquisition
windows* (both short — drivehealth's is its ~170 ms sweep), not the 19.6 s
*interval* between readings. Cadence difference shows up as fewer output
points; window width shows up as band width. Those are separate axes, and
conflating them produced both errors.

### The real cross-cadence question is sampling, not alignment

What decimation would still buy is *meaning*, not safety. Rates are computed
over the evaluation **step**, not the declared range — `rate(x[1s])`,
`rate(x[30s])` and `irate(x[30s])` return byte-identical values, because
`GridRate` computes `increase / step_s` and the range only gates whether
enough samples exist. So the 3 surviving points above are fast-step rates
sampled at 3 arbitrary instants, not averages over the period the slow reading
spans.

No band widening addresses that: a band measures acquisition uncertainty, not
sampling representativeness. Such a result looks precise and is
unrepresentative. The lever is the step, which is a caller parameter
(`query_range(query, start, end, step)`) — so choosing it from the slowest
referenced sampler's cadence is a rezolus-side change, tracked separately
because it changes VALUES where this entry's rule changes only bands.

### Why the union span, not begin-to-begin

The value carried by a group is an observation whose true instant is unknown
within its window; that is what the window *means*. So when two groups are
combined as if simultaneous, the real offset between the observations could
be anywhere from zero (overlapping windows) up to

```
max(a.end, b.end) − min(a.begin, b.begin)
```

— the case where one was read at the start of its window and the other at
the end of its. Begin-to-begin understates that by the second window's
width. The difference is microseconds at the sizes measured above, so this
is not chosen for its magnitude; it is chosen because it is the bound that
is actually true, and because a band that can be wrong in the narrow
direction is worse than no band. Widen to contain the nominal, as the
rate() bands already do.

## What it dissolves

`route()` refuses cross-sampler queries because there was no way to say what
the join costs. Once the band can carry an alignment term there is nothing
left to refuse — the query answers, and the 1.5 ms is stated rather than
hidden. The refusal was a placeholder for arithmetic we had not written.

Downstream, that removes the last thing standing between `.rez` and being
the default recording format: it is smaller, faster, and honest about
windows, and it would no longer be the format that cannot answer a stock
chart. It also makes `parquet` demote cleanly to what it is genuinely good
at — interchange, and multi-source recordings that mix rezolus with
Prometheus sources.

## Open

- **Does a widened band change any existing dashboard's rendering?** The
  bands are additive information, but a chart that draws them will draw
  wider ones on cross-table plots. Worth looking at before landing.
- **Cross-cadence semantics** — interpolate, decimate, or nearest-with-a-
  large-band — remains unanswered and deliberately out of scope here.
- **Prometheus → `.rez`** becomes worth revisiting once this lands. A scrape
  is one HTTP GET at one instant: exactly one group with one real window,
  which would give service metrics acquisition windows they have never had.
  The converter already produces the `Snapshot` type `.rez` ingests; what it
  lacks is a `sampler` identity for its metrics.

## Method notes

- The 313-query audit came from `cargo run -p dashboard` cross-referenced
  against a live agent's `/metrics/json` group schemas, so metric→sampler
  membership is the shipped mapping rather than a guess from source.
- Window spreads are from a single `/metrics/json` snapshot on a 32-core
  x86_64 host under a steady workload; the two recordings compared above came
  from one agent minutes apart, so the query results differ only by format.
