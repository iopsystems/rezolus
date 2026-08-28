# `.rez` v3 versus parquet on the read path — the comparison nobody had run

- **Opened:** 2026-08-27
- **Status:** **MEASURED — REGRESSION.** Against a single parquet file holding
  the same window, a v3 `.rez` is **2.25× larger and 8.0× slower to query** at a
  50 ms interval (1.76× / 4.4× at 1 s). The cause is isolated and is **not the
  layout**: the reader materializes the entire archive at open, so 91% of a
  query's wall time is spent opening tables the query never reads.
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

## Deferred / reopen

- **The size gap is unaddressed by fixes 1 and 2.** They target open latency.
  Fix 3 is the only one of the three that moves bytes, and it is untested here.
  *Reopen:* after fix 3, re-run this benchmark's size arm.
- **No measurement of read cost on a live/unsealed archive.** Both arms here were
  finalized. Hindsight reads a buffer with a live WAL tail, which materializes
  differently. *Reopen:* when fix 1 lands, measure the hindsight read path too.
- **Query evaluation cost is not resolvable from this data.** The floor is
  91–95% of every `.rez` measurement, so the four queries land within ~50 ms of
  each other regardless of what they touch — the differences that remain are
  inside the open cost. Nothing here says whether the split layout's *evaluation*
  is faster than parquet's, only that the open swamps it. *Reopen:* re-run after
  fix 1, when the floor stops dominating.
