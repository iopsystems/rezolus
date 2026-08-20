# Stage 4 — native V3 ingest measured: the split layout pays off on real recordings

- **Opened:** 2026-08-20
- **Status:** **MEASURED.** The synthetic gate's prediction reproduces on
  real recordings at production cadence — and beats it on archive size.
- **Arc:** closes the loop opened by
  [split-table read-cost gate](2026-08-18-split-table-read-cost-gate.md)
  (which measured a mechanically *rewritten* archive) and continued through
  [v3 format acceptance](2026-08-19-v3-format-acceptance.md). This is the
  first measurement of the shipped write path.
- **Owner:** Brian Martin
- **Repos:** rezolus (branch `feat/agent-acquisition-groups`),
  metriken (fork branch `acq-groups`, rev-pinned).

## Why

The 2026-08-18 gate proved a *layout* was worth building: it took a wide
`.rez`, rewrote it into per-acquisition-group tables offline, and measured
0.55–0.68× query time and 0.63× archive bytes. But that was a rewrite tool,
and it lost `rate()` uncertainty bands entirely — the split arm carried
table-level window columns the query engine did not yet understand.

Stage 4 built the real thing: the recorder ingests SnapshotV3 natively into
per-group tables with one window pair per table (no per-metric sidecars),
the query engine resolves table-level windows, and a same-timeline union
answers queries spanning a sampler's groups. This entry measures that path
end to end.

## Method

One agent build, recorded twice on a 32-core x86_64 host (Linux 6.12) under
a steady workload, differing only in the agent's `snapshot_format`:

- **v2 arm** — default format → `record --interval 50ms --duration 300s`
- **v3 arm** — `snapshot_format = "v3"` → same command

Arms ran back-to-back, not interleaved — the same caveat as prior rounds,
and the reason values differ between arms (a live workload moves; the two
arms observe different 300 s windows). Query timings are 7 reps, median.

An earlier pair at **1 s** interval is reported alongside, because the
difference between the two is the most useful result here.

## Results

### Archive size

| interval | v2 | v3 | ratio |
|---|---|---|---|
| 50 ms (gate conditions) | 259.9 MB | **100.4 MB** | **0.386×** |
| 1 s | 20.3 MB | 14.2 MB | 0.70× |

The 50 ms figure beats the synthetic gate's 0.63× by a wide margin. Both
arms hold the same data; the difference is the per-metric window sidecars
the v3 layout does not write.

### Tables and segments (1 s pair)

| | v2 | v3 |
|---|---|---|
| tables | 25 | 54 |
| segments | 51 | 105 |

V3 carries roughly twice the tables and segments and is still smaller — the
sidecar elimination outweighs the per-table overhead the gate worried about.

### Query latency (50 ms archives, median of 7)

| query | v2 | v3 | ratio |
|---|---|---|---|
| `sum(irate(cpu_usage[1m]))` | 1152 ms | 596 ms | **0.52×** |
| `sum by (name) (irate(cgroup_cpu_cycles[5m]))` | 1029 ms | 603 ms | **0.59×** |
| `sum(irate(scheduler_runqueue_wait[5m]))` | 977 ms | 583 ms | **0.60×** |

Squarely inside the gate's predicted 0.55–0.68× band.

### The scale dependence — the finding worth keeping

At **1 s** the same three queries went the *other* way: 120→136 ms,
110→156 ms, 122→139 ms, i.e. **1.13–1.42× slower**. The split layout's win
is not unconditional. It comes from reading narrow tables instead of wide
ones, and that saving scales with table width and row count; the cost —
opening roughly twice as many tables and segments — is close to fixed. At
6,000 ticks over thousands of columns the saving dominates by ~2×; at 300
ticks over a fifth of the bytes the fixed cost wins.

Production cadence is where the design is aimed, so this is the right
trade. But a coarse-interval recording is measurably slower to query under
v3, and that belongs on the record rather than in a footnote.

### `rate()` bands are back

The gate's split arm produced no uncertainty bands at all. Both arms now do:

```
v3: First: … = 1908998389  [1908998389.000000, 13489894745.943729]
v2: First: …  =  29216170  [29216170.000000,     887508955.234037]
```

That is Part A (table-level window columns in the query engine), Part B
(one window pair per group table in the writer) and Part C agreeing on real
data — the capability the whole arc exists to deliver, surviving from the
agent's acquisition bracket through the recorder to the query.

### The cross-group union path

A query spanning two groups of one sampler — `sum(irate(cpu_usage[1m])) /
sum(irate(softirq[1m]))`, whose metrics live in `cpu_usage/cpu_usage_cpu`
and `cpu_usage/cpu_usage_softirq` — is the path Part C added and nothing
had measured:

| | v2 (one wide table) | v3 (union of two group tables) |
|---|---|---|
| result | 300 points | 300 points |
| bands | present | present, per metric |
| latency (median of 7) | 1503 ms | **743 ms** |

0.49×, with each metric's band still resolved from its own group's window.
The union does no timestamp join, no column concatenation and no null
filling — it dispatches by metric name and lets the engine align series on
its evaluation grid, which is why the per-group windows survive to the
query instead of being fanned back into per-metric sidecars.

## Verdict

The shipped write path reproduces the synthetic gate's query result
(0.52–0.60× against a predicted 0.55–0.68×) and substantially beats its
archive prediction (0.386× against 0.63×), while restoring the uncertainty
bands the gate's rewrite arm could not carry. The cross-group union — the
one path with no prior measurement — is the fastest relative result of the
set.

The honest qualifier: this holds at production cadence. At 1 s the same
queries are 1.13–1.42× slower under v3, because the fixed cost of opening
about twice as many tables stops being repaid by narrower reads. Anyone
recording at coarse intervals should know that before flipping the format.

## Notes

- Values differ between arms by construction (sequential runs, live
  workload). Shape, series counts and band structure were compared; value
  equality was never available and was not asserted.
- Both arms used one binary at branch head `0e447844`, verified fresh
  before measuring — an earlier attempt in this session came within one
  step of measuring a day-old binary that predated Parts B and C.
