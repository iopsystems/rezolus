# `.rez` v3 versus parquet on the read path — the comparison nobody had run

- **Opened:** 2026-08-27
- **Status:** **MEASURED, THEN FIXED.** The regression was real: a v3 `.rez` was
  **2.25× larger and 8.0× slower to query** than a single parquet holding the
  same window (1.76× / 4.4× at 1 s). The cause was never the layout — the reader
  materialized the entire archive at open, so 91% of a query's wall time went on
  tables it never read. With the reader made lazy, **`.rez` is
  faster than parquet on every query in both arms** — 0.61–0.72× at 50 ms and
  0.70–0.94× at 1 s. The size gap is unchanged and remains open. See
  *Resolution*.
- **Arc:** closes the gap left standing by
  [stage 4 native V3 ingest](2026-08-20-stage4-native-v3-ingest.md) and the
  [split-table read-cost gate](2026-08-18-split-table-read-cost-gate.md). Both
  measured *layout* — split versus wide, v3 versus v2 — with a `.rez` container
  on both arms, so container cost cancelled and was never priced. This prices it.
- **Owner:** Brian Martin
- **Repos:** rezolus (`bf52bdeb`), metriken-query 0.20.0 (read path).

## Why

`record` now defaults to `.rez` (#1097), and the case for that default rested on
figures that do not say what they appear to say. The widely-quoted **0.386×**
archive and **0.52–0.60×** query numbers compare **v3 against v2, both `.rez`**
(#1070/#1076). The gate's **0.63×** and **0.55–0.68×** compare **split against
wide tables, both `.rez`** (#1065). Neither has a parquet arm.

So the format we now write by default had never been measured against the format
it replaced, on the two axes a user notices first. Before pushing adoption, that
number needed to exist — whichever way it came out.

## Method

A 32-core Linux host, release build, otherwise-idle. One agent; **two recorders
running concurrently against it**, so both capture the same window and the same
counter values rather than two sequential runs that would differ in workload:

```
rezolus record --url … -o arm.rez     -i <interval> -d 300s
rezolus record --url … -o arm.parquet -i <interval> -d 300s
```

Two arms, 300 s each: **50 ms** (high-resolution, where the layout is expected to
win) and **1 s** (coarse, where #1070 recorded the layout advantage reversing).
Four queries, 7 reps each, medians reported: a per-CPU fan-in rate, an IPC ratio
(cross-group, same sampler), a cross-sampler ratio, and a second sampler's
counters. A fifth trivial query serves as a **fixed-cost floor**, since each
measurement includes process startup and file open.

Both archives were verified to hold the same window (6001 rows per table at
50 ms; matching `first_sample_ns`/`last_sample_ns`). No metric names, labels or
cgroup paths from the measurement host appear in this entry.

## Results

### Archive size

| interval | `.rez` | parquet | ratio |
|---|---|---|---|
| 50 ms | 93.2 MB | 41.5 MB | **2.25× larger** |
| 1 s | 13.4 MB | 7.6 MB | **1.76× larger** |

### Query latency, medians of 7

| interval | query | `.rez` | parquet | ratio |
|---|---|---|---|---|
| 50 ms | per-CPU fan-in rate | 629 ms | 79 ms | 8.0× |
| 50 ms | IPC (cross-group) | 608 ms | 70 ms | 8.7× |
| 50 ms | cross-sampler ratio | 620 ms | 88 ms | 7.0× |
| 50 ms | second sampler counters | 578 ms | 42 ms | 13.8× |
| 1 s | per-CPU fan-in rate | 149 ms | 34 ms | 4.4× |
| 1 s | IPC (cross-group) | 151 ms | 56 ms | 2.7× |
| 1 s | cross-sampler ratio | 155 ms | 51 ms | 3.0× |
| 1 s | second sampler counters | 150 ms | 34 ms | 4.4× |

### The fixed-cost floor is nearly the whole query

| interval | floor `.rez` | floor parquet | floor as % of `.rez` query |
|---|---|---|---|
| 50 ms | 572 ms | 44 ms | **91%** |
| 1 s | 141 ms | 36 ms | **95%** |

Process startup is 2–3 ms, so the floor is file open, not measurement overhead.

## Mechanism — it is the open, not the layout

Three observations isolate it.

**A query's cost does not depend on what it asks for.** Querying a single small
gauge table costs 664/631/642 ms; a query touching six `cpu_usage` tables costs
599/579/598 ms. The smaller query is *slower*. Cost tracks the archive, not the
question.

**The reader materializes everything at open.** `RecordingBytes.tables` is
`Vec<(String, Vec<Vec<u8>>)>` (`src/recorder/rez.rs:817`) — every segment's bytes
of every table, in memory, before a query is parsed. `RezReader::from_recordings`
(`src/rez_reader.rs:166`) then constructs a reader per table over those bytes.
This is the same whole-archive slurp the SQLite container was adopted to
eliminate: #1060 fixed it on the **write** side; the read path still does what
the tar reader did, and `read_v3_recordings` says so — it resolves a v3 archive
into "the same `RecordingBytes` the tar reader" consumed.

**Footer work dominates that open, and it is per segment.** The archive is
**50 tables across 418 segments** (218 KB average). `SegmentedParquetReader::open_bytes_with_pool`
(metriken-query 0.20.0, `src/segmented.rs:48`) opens each segment individually
and then builds four identity indexes, its own comment describing "a single
schema pass per segment per metric kind" — about 418 footer parses and ~1,670
schema passes for the whole archive. Forcing real blob reads out of SQLite
(`sum(length(hex(bytes)))`, which must materialize each payload) costs 157–180 ms
of the 572 ms; the remainder is footer and index construction.

For comparison the parquet arm is **one file, 5,127 columns × 6,001 rows, 4 row
groups** — one footer, and column pruning means a query reads only the columns it
names.

A query touching `cpu_usage` needs **6 tables / 47 segments — 11% of the
archive**. The other 89% is opened and discarded.

The size gap has the same root. 418 segments each carry a complete parquet
footer and schema, and compression cannot work across segment boundaries; the
parquet arm compresses each column across all 6,001 rows at once. Segments here
average 218 KB against the **1.4 MiB fleet average** the container design was
priced at ([SQLite container](2026-08-12-rez-sqlite-container.md)), so the
per-segment overhead is being paid ~6× more often than that design assumed.

## What this does and does not change

It does **not** invalidate the default. `.rez` still holds properties parquet
structurally cannot, and they are why it exists:

- **Bounded finalize** — 303.7 ms median regardless of length; a 30× longer
  recording costs 1.25× more (#1041). The parquet path replays its whole spool.
- **Survives a kill** — one interval lost, archive still opens and answers
  PromQL. A killed parquet recording loses everything, because the file is
  written at the end.
- **Readable while being written** — hindsight's rolling buffer is impossible
  without it.
- **Acquisition windows** — one per read, so `rate()` carries honest uncertainty
  instead of a per-metric guess.

What it changes is the *claim*. The adoption case is durability and measurement
honesty, bought at a present cost in archive size and query latency — not a
free win on all axes. Anyone told `.rez` is smaller and faster will measure it
and find otherwise.

It also corrects a reading of the earlier entries. Their ratios are sound for
what they compared; the error would be carrying them across to a comparison they
never made. Recorded here so the next person does not have to re-derive it.

## Plan

Three fixes, in value order. None requires changing the layout.

1. **Open only the tables a query touches** (rezolus). The blocker is name
   resolution: `counter_names()`/`columns()` (`src/rez_reader.rs:531–560`) ask
   every table's reader, which needs its footer. Fix by writing a metric→table
   index into the SQLite catalog — `segments` is already keyed
   `(recording_id, sampler, seq)` (`src/recorder/rez_sqlite.rs:1159`), so scoped
   fetch is free — and answering resolution from it. Archives without the index
   fall back to today's path. Expected: 572 → ~70 ms open on this archive.
2. **Share the parsed schema across a table's segments** (metriken-query). A
   table's segments have identical schemas, which `schema_hash` already asserts.
   One identity pass per *table* rather than per *segment*.
3. **Seal larger segments** (tuning). 218 KB average against a 1.4 MiB design
   assumption. Fewer footers, and compression regains locality — this is the
   lever on the size gap, which fixes 1 and 2 do not touch.

## Resolution — the reader, made lazy

Fix 1 landed, and went further than the plan: the v3 path no longer materializes
the archive **at all**.

Three changes, each measured separately because the first two were not enough on
their own:

1. **Route from a name catalog, not from open readers.** `owners` asked every
   table's reader for the query's columns, which required opening it.
   `metriken_query::referenced_metrics` (metriken#138) extracts a query's metric
   names by parse alone, so routing matches against a per-table catalog probed
   from ONE segment's footer. *629 → 497 ms — almost nothing.*
2. **Answer `time_range` and `interval` without a reader.** The first fix was
   defeated one call earlier: `mcp query` asks `time_range()` before evaluating
   anything, and it fanned out through `reader()`, opening every table regardless.
   Probing each table's span made the laziness actually hold. *497 → 132 ms.*
3. **Fetch a table's payload only when it is queried.** `read_v3_recordings`
   pulled every segment's bytes out of SQLite before the reader existed.
   `SegmentSource::Db` holds `(path, recording_id, sampler)` and resolves on
   first use; spans come from `segment_span`, which reads no BLOB. *132 → 56 ms.*
4. **Parse the remaining probe footers in parallel.** They are independent; the
   SQLite phase stays serial because a `Connection` is not `Sync`, and is cheap
   (2.1 ms of catalog queries + 5.4 ms of probe blobs for a 50-table archive).
   *56 → 49 ms, floor 33 → 23 ms.*

A refuted hypothesis, recorded because it looked obvious: parallelism gave 1.8×
rather than anything near the core count, which suggested contention on the
shared `BufferPool` mutex. Rewriting the probes chunked with a private pool per
worker measured **identical**. Contention was not the limiter. What remains is
~12 ms of schema parsing that does not scale with cores — plausibly
allocator-bound, since parsing thousands of columns allocates heavily, but that
is a hypothesis and this one has already been wrong once. The chunked form was
kept anyway: it bounds thread count instead of spawning one per table.

The tar path still materializes, and should: tar has no index. Using the index
is what SQLite was adopted for, and until now only the writer used it.

### Measured, same host and archives as the regression above

| interval | query | `.rez` before | `.rez` after | parquet | before | after |
|---|---|---|---|---|---|---|
| 50 ms | per-CPU fan-in rate | 629 ms | **49 ms** | 71 ms | 8.0× | **0.69×** |
| 50 ms | IPC (cross-group) | 608 ms | **46 ms** | 75 ms | 8.7× | **0.61×** |
| 50 ms | cross-sampler ratio | 620 ms | **63 ms** | 87 ms | 7.0× | **0.72×** |
| 50 ms | second sampler counters | 578 ms | **25 ms** | 41 ms | 13.8× | **0.61×** |
| 1 s | per-CPU fan-in rate | 149 ms | **31 ms** | 44 ms | 4.4× | **0.70×** |
| 1 s | IPC (cross-group) | 151 ms | **32 ms** | 39 ms | 2.7× | **0.82×** |
| 1 s | cross-sampler ratio | 155 ms | **34 ms** | 46 ms | 3.0× | **0.74×** |
| 1 s | second sampler counters | 150 ms | **29 ms** | 31 ms | 4.4× | **0.94×** |

The floor tells the story best: **23 ms on the 88 MB archive against 27 ms on the
12 MB one.** Open has stopped scaling with archive size — the larger archive is
now *faster* to open, because what remains is proportional to table count, not
bytes. Measured across archives filtered to different table counts, that cost is
**~0.25 ms per table on top of ~12 ms fixed** (down from 0.45 ms/table before the
probes were parallelized), so a 200-table archive would floor near 50 ms.

Floor overall: **572 → 23 ms, 24.9×.**

### What the tests caught

The first version skipped any sampler with no sealed segments. That is exactly a
quiet sampler in a live hindsight buffer — rows in the WAL, nothing sealed yet —
and four tests failed on it, including
`a_hindsight_buffer_opens_as_an_ordinary_rez`. The probe now falls back to the
materialized WAL tail, which is what `table_segments` did all along. Worth
recording because the regression this entry opens with also came from the read
path quietly doing the wrong thing for a whole class of table.

### Not fixed

**Size is unchanged at 2.25×.** All three changes target open latency; none moves
bytes. Fix 3 (larger segments) remains the only lever on it, and remains a trade
rather than a win — `max_rows: 900` was chosen to cut finalize from 1147.6 →
549.8 ms ([recorder resource footprint](2026-08-13-recorder-resource-footprint.md)),
so raising it buys size and open at finalize's expense. With query latency now
ahead of parquet, that trade is harder to justify than it looked.

## Deferred / reopen

- **The size gap is unaddressed and now the only open axis.** `.rez` remains
  2.25× parquet at 50 ms, 1.76× at 1 s. Fix 3 is the only lever and it trades
  against finalize. *Reopen:* if archive size becomes the binding constraint,
  price `max_rows` against finalize wall-clock again — the earlier decision was
  made when open cost was not yet understood.
- **The coarse-interval reversal recorded in #1070 is untested under this
  reader.** That entry measured the split layout running 1.13–1.42× *slower* at
  1 s and attributed it to the fixed cost of opening about twice as many tables.
  This work removed most of that fixed cost, and the 1 s arm here now beats
  parquet — but that is `.rez`-versus-parquet, a different comparison from
  #1070's split-versus-wide, so it does not settle that entry's finding.
  *Reopen:* re-run #1070's arms under the lazy reader before claiming the scale
  dependence is gone.
- **The last fixed cost is ~50 name probes.** One footer per table at open, to
  build the routing catalog. Caching per-table metric names in the SQLite
  catalog at write time would remove them, but the format change is no longer
  needed to be competitive. *Reopen:* if table counts grow well past 50.
- **No measurement of read cost on a live/unsealed archive.** Both arms here were
  finalized. Hindsight reads a buffer with a live WAL tail, which materializes
  differently. *Reopen:* when fix 1 lands, measure the hindsight read path too.
- **Query evaluation cost is not resolvable from this data.** The floor is
  91–95% of every `.rez` measurement, so the four queries land within ~50 ms of
  each other regardless of what they touch — the differences that remain are
  inside the open cost. Nothing here says whether the split layout's *evaluation*
  is faster than parquet's, only that the open swamps it. *Reopen:* re-run after
  fix 1, when the floor stops dominating.
