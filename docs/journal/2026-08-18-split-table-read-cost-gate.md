# Split-table read-cost gate — measured, and the split layout wins

- **Opened:** 2026-08-18
- **Status:** **MEASURED** — results below; go/no-go decision pending.
- **Arc:** executes the measurement gate defined in the addendum to
  [window sidecar cost](2026-08-17-window-sidecar-cost.md), which reopened
  the split-tables option behind exactly this measurement.
- **Owner:** Brian Martin
- **Repos:** rezolus (branch `split-rez-measurement-gate`, PR #1065).

## Question

Does splitting each sampler's wide `.rez` table into per-acquisition-group
tables make read cost unacceptable? The prior entry's rejection rested on
segment count driving read cost (~12 ms/segment, measured on 6,206-column
tables) — but footer/schema parse scales with table width, so narrower
segments should open faster. Which effect wins had to be measured, not
argued.

## Method

`rezolus parquet split-groups` (hidden dev subcommand on the branch)
rewrites a finalized v3 recording into inferred per-acquisition-group
tables; `scripts/split-rez-measure.sh` compares three arms in one tmpfs
directory: the original file (`orig`), a provenance-matched identity
re-encode (`wide-rt`, same writer path/codec as the split arm so the
comparison isolates layout), and the split rewrite (`split`). One untimed
warm-up pass per arm, then 7 timed reps round-robined across arms.

Input: a 300 s, 50 ms-interval recording from a live agent
(5.17.1-alpha.11, 30 samplers) on a 32-core x86_64 host, Linux 6.12, under
a steady workload. 314.6 MB, 309 segments, 26 tables.

Query answers were verified identical across arms (sorted series output is
byte-identical; the split arm lacks the `[lo, hi]` bounds annotations — see
caveats — and prints series in a different order because column order
changes).

## Results

Medians of 7 reps; ratio is split / wide-rt.

| | orig | wide-rt | split | ratio |
|---|---|---|---|---|
| archive bytes | 314,638,336 | 314,617,856 | **197,722,112** | **0.63×** |
| segments | 309 | 309 | 1,723 | 5.6× |
| tables | 26 | 26 | 102 | 3.9× |
| `parquet metadata` (ms) | 4 | 4 | 9 | 2.25× |
| query: per-CPU sampler sum-rate (ms) | 3,299 | 3,332 | **1,816** | **0.55×** |
| query: cgroup family by-name rate (ms) | 2,642 | 2,616 | **1,780** | **0.68×** |
| query: scheduler sum-rate (ms) | 2,645 | 2,625 | **1,773** | **0.68×** |

Rep spreads are tight (widest: wide-rt per-CPU query 3,250–3,559 ms; split
arms all within ~5%). `orig` ≈ `wide-rt` everywhere — the re-encode is a
genuine identity, so nothing below is an artifact of rewriting.

**The segment-count fear inverted.** 5.6× the segments and the queries got
1.5–1.8× *faster*. Per-segment open cost tracks schema width, and the split
arm's tables are ~4–70× narrower; the width term dominates the count term
at this geometry. The old ~12 ms/segment number was a property of
6,206-column footers, not of segments.

**The archive shrank 37%** with the same codec and data. This is the
per-metric sidecar overhead leaving: the prior entry measured 53% of
`cpu_usage`'s columns as all-null sidecar columns, and every windowed
metric carried its own copy of a shared window. One window pair per table
removes both.

## Caveats, both directions

- **Toward split:** today's reader does not pair the split arm's
  table-level `:window_begin`/`:window_width` sidecars, so split-arm query
  times exclude the rate-bounds arithmetic the wide arm performs. In the
  real design the reader restores bounds from one pair per table, so the
  true cost sits between the arms — but the wide arm's per-metric sidecar
  *column reads* are exactly what the design removes, so most of the gap is
  real.
- **Against split:** windowless metrics were grouped per base-metric
  family, over-splitting relative to real acquisitions (102 tables is an
  upper bound on the design's table count). The split arm carried that
  handicap and still won.
- All arms sat in tmpfs with a warm-up pass, so this measures decode/open
  CPU cost, not disk I/O — which matches how the viewer/MCP read recordings
  in practice (and matches the prior entry's methodology).
- `parquet metadata` is the one regression: 4 → 9 ms, tracking table count.
  It is a once-per-open manifest description, three orders of magnitude
  below a single query, and 9 ms absolute.

## Verdict framing

The plan's gate was: split within ~1.25× on query and metadata cost, within
~1.10× on bytes. Queries came in at 0.55–0.68× and bytes at 0.63× — passes
with wide margin. Metadata cost exceeds its ratio threshold (2.25×) at a
trivial absolute value (9 ms); recorded as a known, accepted cost unless
table counts grow far beyond ~100.

**Recommendation: gate passes.** The split layout is smaller and faster to
query than the wide layout on the same data, before the design's own
column-count wins (explicit groups, windows for currently-windowless
acquisitions) are even realized. Next: Stage 2+ of the acquisition-groups
design (SnapshotV3 in metriken-exposition, agent-side group registry,
recorder ingest, reader table-level window pairing), each planned
separately.
