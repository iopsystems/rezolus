# Recorder resource footprint — seal cost and peak RSS

- **Opened:** 2026-08-13
- **Status:** **IMPLEMENTED & MEASURED** for the memory work — peak RSS
  **843 → 189 MB (4.5×)** with total process CPU *reduced*, from two constants.
  Seal-policy retune landed alongside. Two follow-ons open (WAL-sourced seals,
  agent↔recorder transport); the structural root cause (per-metric window
  sidecars tripling column count) is untouched.
- **Arc:** follows [`.rez` v3 — SQLite container](2026-08-12-rez-sqlite-container.md)
  and [streaming segmented writer](2026-08-11-rez-streaming-writer.md). Amends
  two decisions in the former — see "Corrections" at the end.
- **Owner:** Brian Martin
- **Repos:** rezolus only. Base commit `cf86d0c3`; the work is a working-tree
  change at time of writing, not yet committed.

## Why

Two questions, which turned out to be nearly independent.

1. **What should the seal policy actually be?** The v3 entry settled `max_bytes`
   at 8 MiB on a size argument. That reasoning was never tested against CPU.
2. **Why does a telemetry recorder need 843 MB of resident memory?** Against a
   50–100 MB target for an always-on fleet agent, this is the number that makes
   hindsight expensive to run everywhere.

The second question dominated the effort and produced the more useful result.

## Method

Three harnesses, escalating as each failed to answer the question:

1. **A 28-cell seal-cost sweep** (one recorder, one query per cell; 300 s at a
   50 ms interval) across two hosts — a 32-core SSD host under a live
   production workload with 30 samplers, and an idle 64-core NVMe host with 26.
   Policy and codec driven from the environment by temporary scaffolding so a
   single binary sweeps every cell.
2. **RSS sampled over time** from `/proc/<pid>/statm` at 100 ms, kept alongside
   `ru_maxrss`. This distinction is load-bearing and cost us time to notice:
   `getrusage` has **no current-RSS field** on Linux (`ru_maxrss` is a monotonic
   high-water mark; `ru_idrss`/`ru_isrss`/`ru_ixrss` are unmaintained and always
   0), so peak and steady-state cannot be separated without `/proc`.
3. **Page-fault profiling** — `perf record -e page-faults -g --call-graph dwarf`
   at period 1, so one sample is one fault is one resident page. RSS grows
   exactly when a page is first touched, so this attributes resident memory by
   call stack with no rebuild. This is what finally cracked it, and it is the
   technique to reach for first next time.

All cells hold the seal policy fixed and are bracketed by repeated baselines,
because the busy host drifts: host CPU varied 133k–238k jiffies between cells
while baseline RSS held to 0.5%.

## Part 1 — seal policy is not a CPU knob

**The headline negative result.** Across the sweep, *seal* CPU moved over a 3.3×
range (6.0–19.9 s per 300 s cell) while **total process CPU moved almost not at
all**: 80.0–89.5 s overall, and 80.55–84.06 s across eight repeats of the same
cell — a 4.4% noise band. Sealing is only 7–24% of what the recorder burns; the
per-tick scrape/decode/ingest path is the rest.

So the caps are free variables on CPU and must be chosen for finalize wall,
peak memory, and the kill-loss window. Selected values (`SealPolicy::default`):

| | old | new | why |
|---|---|---|---|
| `max_bytes` | 8 MiB | 8 MiB | held |
| `max_rows` | 4096 | **900** | finalize 1147.6 → 549.8 ms at the same byte cap |
| `max_age` | 300 s | 300 s | bounds kill-loss, not seal cost |

`max_bytes = 32 MiB` was tried and **rejected**: it is better on every offline
axis (archive 218.8 vs 302.0 MiB, `mcp query` 0.85 vs 2.65 s) and worse on the
one the agent pays — finalize 827.3 vs 549.8 ms, peak RSS 844 vs 807 MB — for
CPU inside the noise band. Read cost tracks segment *count* (~12 ms/segment) and
belongs to the offline compactor, which can have it without charging the agent.
Below 8 MiB is worse still: a 2 MiB cap inflated the archive to 750.3 MiB
against 302.0 and took `mcp query` to 9.9 s, because segments are the encoder's
unit of compression.

**Compression: LZ4_RAW adopted.** The call site passed `None` for
`WriterProperties`, which selects `UNCOMPRESSED` rather than any default. LZ4 is
free on CPU (80.55/82.52 s total against 81.72/83.30 uncompressed — inside the
band) and halves the archive (324.4 vs 405.1 MiB). zstd is rejected on
**memory**: +500 MB peak RSS (1330/1356 vs 832 MB) and 24% more seal CPU, with
level 3 buying no ratio over level 1.

**Correction banked:** an earlier write-up credited LZ4 with a read speedup
(2.62 → 0.85 s). That comparison moved the codec *and* both caps. Isolating the
codec: 1.45 → 1.39 s and 2.62/2.66 → 2.59/2.65 s — i.e. nothing. The read win
came from `max_bytes` via segment count.

**Free-list reclaim at finalize** (`reclaim_all`, `rez_v3_writer.rs`). WAL
pruning frees pages continuously but only the retention path called
`reclaim_if_fragmented`, so a `record` run never returned a page. At the shipped
policy a 302.0 MiB archive carried 30.8 MiB of free list (+10%); a sparse 1 Hz
probe was 21.5 MiB of file over 8.65 MiB of segments (+148%).

## Part 2 — peak RSS: 843 → 189 MB

### The flatness result, and what it eliminated

RSS against a 16× change in builder headroom:

| byte cap | 2 MiB | 4 MiB | 8 MiB | 16 MiB | 32 MiB |
|---|---|---|---|---|---|
| peak RSS (MB) | 742–747 | 750–752 | 807–835 | 856–947 | 844–1030 |

Sixteen-fold more builder headroom moves RSS ~100 MB. **Any term that scales
with the seal cap is therefore a minority of the footprint** — which
disqualifies the builders, the encode working set, and batch residency as
dominant causes. This single table saved us from a seal-path refactor that would
have bought ~8%.

### What it was

| change | RSS | note |
|---|---|---|
| baseline | 843 MB | |
| writer page-cache split | 599 MB | **−244** |
| parquet dictionary off | **189 MB** | **−403** |
| `shrink_to_fit` at seal | −6 MB | kept for accounting honesty, not impact |

**Writer page cache** (`rez_sqlite.rs`). `cache_size` was 256 MiB on *every*
connection, justified by a +78% segment-**read** benchmark. A recording writer
inserts opaque BLOBs and never reads one back; it only benefits from caching
catalog b-trees, which are kilobytes. It is not idle headroom — a seal batch
dirties every overflow page of every segment inside one transaction, so a
co-seal walks the cache to whatever cap it is given and SQLite does not return
it. Split to 16 MiB for `create` (writers), 256 MiB retained for `open`, which
is where every bulk read in the tree arrives.

**Parquet dictionary encoding** (`segment_writer_props`, `rez.rs`) — the single
largest memory decision in the recorder. `ArrowWriter` instantiates a column
writer for **every column of a row group simultaneously**, each carrying its own
`DictEncoder` buffer and interner. Measured column counts on a 32-core host:

| table | columns |
|---|---|
| `cpu_usage` | **5,846** |
| `syscall_counts` | 1,397 |
| `scheduler_runqueue` | 1,118 |
| `cpu_tlb_flush` | 869 |

Disabling it: **592 → 189 MB (−68%)**, with archive size *identical* (293.9 vs
293.6 MiB) and CPU **cheaper** — seal CPU 9.268 → 6.523 s (−30%), total process
CPU 97.1 → 86.7 s. It costs nothing because the data is the worst possible
dictionary input: u64 counters and gauges, where a monotonic counter makes every
value distinct. There are no string columns; names and labels live in the schema.

### Dead ends, all measured

- **glibc allocator retention — refuted.** `MALLOC_ARENA_MAX=2` moved RSS 843 →
  840 MB, and nothing on top of the cache split (593 → 590). The footprint is
  live in-use memory, not fragmentation.
- **Histograms — refuted on arithmetic.** Hypothesised as the bulk at ~58 KB per
  histogram; `docs/principles.md` standardizes on `grouping_power = 3`, i.e. 496
  buckets ≈ 4 KB (`src/common/mod.rs:4`). Reaching 590 MB would need ~150,000
  histogram series. A comment in `rez.rs` claiming 7,424 buckets at gp=7/mvp=64
  was the source of the error and has been corrected — no sampler uses gp=7.
- **The three plausible `WriterProperties` companions — all null.**
  `write_batch_size = 256` (the 1024 default is larger than a whole 900-row
  segment) moved RSS 1 MB; `EnabledStatistics::Chunk` moved 1 MB and cost
  finalize 407 → 742 ms and query 3.04 → 3.93 s; 64 KiB page limits moved 2 MB.
  All four together scored 194 MB — no better than the dictionary alone, for
  10 s more CPU.

### Shape of the remaining footprint

RSS sampled at 100 ms over a 300 s run: 12.6 MB at t=0, **473.5 MB by t=20 s**,
590.6 MB by t=100 s, then flat to 0.1 MB for the last 200 s. `sampled_max`
604.8 MB against `ru_maxrss` 602.2 MB, so no spike hides between samples. It is
a working set established almost immediately, not a leak and not a transient.

Page-fault attribution accounted for **599.3 MiB against a 590 MB plateau**:

| frame | MiB | share |
|---|---|---|
| `rez::write_table_parquet` | 213.1 | 36% |
| ↳ `get_column_writer` (constructing writers) | 166–173 | 28% |
| `recorder::run` closure (scrape + msgpack decode) | 108.9 | 18% |
| `TableBuilder::push_row` + `RawVec::grow_one` | 49.5–55.6 | 9% |
| LZ4 compress | 28.6 | 5% |
| `sqlite3_step` / segment insert | 26.3 | 4% |

## The structural root cause, untouched

**Per-metric `:window_begin`/`:window_width` sidecars triple every table's
column count.** `cpu_usage` is 5,846 columns where ~1,950 would do. For a BPF
sampler every metric in a tick shares one acquisition window — the maps are read
once — so a window pair per metric is 3× the columns for no additional
information. The format already has the cheaper shape available: `:wall_offset`
is stored once per row at table level.

The dictionary fix removed this redundancy's *cost*, not the redundancy. Whoever
picks this up gets a 3× reduction in column count, which is upstream of parquet
memory, archive size, and read cost simultaneously.

## WAL-sourced seals — DONE, and it bought more than memory

Sealing a v3 segment now replays that sampler's live WAL instead of encoding a
parallel `TableBuilder`, so a tick's values are written once rather than twice.
Measured against merged main (`c07767fb`) on a 32-core host under a live
workload, three interleaved rounds per arm at a 50 ms interval for 300 s:

| | buffered | WAL-sourced | |
|---|---|---|---|
| peak RSS | 192 MB | **115 MB** | −40% |
| process CPU | 89.5 s | **69.3 s** | −23% |
| archive | 261.3 MB | 278.1 MB | +6.4% |
| query | 3.07 s | 3.25 s | +6% |

**The archive grew because the recording holds more data, not more overhead —
and that is the headline result.** At 2,400 ticks due in 120 s, the buffered
arm captured 2,187 and the WAL-sourced one 2,391: **dropped ticks fall from
8.9% to 0.4%**. Per row the archive is slightly *smaller* (1,992 vs 2,024 B),
which is what the byte-identical segment property predicts. Query time tracks
the extra segments the extra rows produce.

So the per-tick saving was not headroom. At a 50 ms cadence the scrape loop
could not keep up with its own bookkeeping and was losing roughly one tick in
eleven; removing the second copy bought that back.

Three predictions, all wrong in the same direction: ~50 MiB estimated against
77 measured, CPU direction called as "within 2×, sign unknown" against −23%,
and the fidelity effect not anticipated at all. The shadow read-back that
sized the cost (2.7 s of decode per 300 s run) was accurate; what it could not
see was the `push_row` cost it displaced.

Structured as three commits — extract the seal accounting so both containers
keep one predicate, relocate `materialize_wal_tail` to the module owning WAL
rows, then the change. The claim everything rests on (a replayed segment is
the segment buffering would have produced) is asserted as encoded parquet
bytes, and caught a fixture defect on first run where the data pages matched
byte-for-byte and the embedded arrow schema did not.

## Open / next
- **Agent↔recorder transport.** The msgpack decode path is 108.9 MiB (18%).
  Shared memory would target it; scoped as a separate project. Note the payload
  itself is small — ~142 KiB per tick across all samplers — so the prize is
  decode residency and allocation churn, not bytes on the wire.
- **Recorder/hindsight self-metrics.** Neither process registers a single metric
  of its own, so hindsight's footprint is invisible on every fleet host. The
  agent's `rezolus_rusage` sampler is `RUSAGE_SELF` only, and its
  `rezolus_memory_usage_resident_set_size` gauge is fed from `ru_maxrss` — a
  monotonic peak published under a name and description that read as current
  usage, so it can never decrease. Both worth fixing.
- **`WRITER_CACHE_SIZE_KIB` is sized, not fitted.** 16 MiB is reasoned from
  `max_bytes`; where the knee sits between 2 and 256 MiB is un-swept. Now a
  small term.
- **Flaky test**, pre-existing and unrelated:
  `hindsight::buffer::tests::at_retention_bound_flips_once_the_recording_outlasts_the_lookback`
  fails ~1 run in 6. The front half is deterministic (fixed timestamps; the
  predicate is arithmetic on `first_ts`/`newest_ts`), so the race is likely in
  the later assertions, which depend on the writer thread having drained its
  channel. Not diagnosed.

## Corrections to the v3 entry

[`.rez` v3 — SQLite container](2026-08-12-rez-sqlite-container.md) should be read
with two amendments:

1. § "Decision: keep `max_bytes` at 8 MiB" reaches the right value by an
   argument that was never CPU-tested. The value holds; the reasoning is
   replaced by Part 1 above — the caps are CPU-neutral and are chosen for
   finalize wall and memory.
2. § "Per-sampler compression ratios" proposes a target-*encoded*-size cap to
   fix `syscall_latency` emitting many small segments while `cpu_usage` emits
   large ones. Still reasonable, but its urgency drops: segment count drives
   read cost, which is the compactor's problem, and it does not touch peak RSS,
   which is set by column count.
