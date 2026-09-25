# Long recordings: memory proportional to the query, not the file

- **Opened:** 2026-09-23
- **Status:** **SHIPPED** in v5.22.0 (rezolus #1283, #1284, #1286, #1287;
  metriken-query 0.27.0 to 0.30.0) and taken by systemslab in
  iopsystems/systemslab#6263, measured there 2026-09-24 (below). Follow-up
  in v5.22.1 (#1296). Updated 2026-09-24.
- **Driver:** a 1.28 GB `.rez` (one host, 9.6 hours at 1 s, 49 tables, 5,874
  segments) opened and queried, but memory scaled with the file: 4.1 GB
  resident after the viewer built its dashboard, 9.4 GB after one query over
  the per-task table. The question was whether long files can be handled
  without that, and the answer was no, for three separate reasons.
- **Owner:** Brian Martin

## The fixture

`metrics.rez`, written by a 5.20-era recorder (schema 3, so no identity
index; everything here is the parquet path). The task table
(`cpu_usage/cpu_usage_task`, metric `task_cpu_usage`) is the multiplier:

| | |
|---|---|
| segments | 159 (of 5,874 in the file) |
| columns per segment | 487–2,851 |
| distinct tasks over the day | 6,644 |
| non-null samples | 76,182,021 |
| bytes | 668 MB of the 1,280 MB file |

Every other table has 116 segments and tens of columns.

## Measured before

Current build (5.21.1-alpha.3), `/usr/bin/time -l` peak RSS for `mcp`, and
the viewer's resident size after each step:

| operation | wall | memory |
|---|---|---|
| `mcp describe-metrics` | 5.0 s | 4.4 GB |
| `mcp query rate(task_cpu_usage{pid="1"}[1m])` | 2.1 s | 2.3 GB |
| `mcp query sum(rate(task_cpu_usage[1m]))` | 18.3 s | 11.9 GB |
| `mcp query sum(rate(cpu_usage[1m]))` | 0.16 s | 0.26 GB |
| viewer: open | 2 s | 0.05 GB |
| viewer: after `/sections` | 3.8 s | 4.1 GB |
| viewer: after the all-task query | 6.9 s | 8.6 GB, 9.4 GB on repeat |

Same numbers on 5.20.1-alpha.20, so nothing recent caused it.

## Three costs

1. **Segment bytes were held.** Opening a table read every segment blob
   out of SQLite and kept it for the reader's life, and the dashboard
   listing opens every table: the whole 1.27 GB resident after `/sections`.
2. **Parsed footers for wide tables.** A filtered query over one task series
   cost 2.3 GB, of which bytes were 0.67 GB. The rest was per-segment
   parsed schema — 2,500 fields with 9 labels each, times 159 segments, as
   arrow metadata plus a column descriptor with cloned labels. On this table
   parsed metadata was about 2.4x the bytes.
3. **Full materialization per query.** `sum(rate())` over 6,644 series
   collects all 76M samples (timestamp, value, window: 32 B each, 2.4 GB)
   and then every series' rate vector before summing. About 4.5 GB per
   query on top of the open cost, and it is not released between queries.

A fourth, for later: the indexed reader from #1280 decodes a whole table
into a `MemoryStore` at first query. On this file that is worse than the
parquet path. Step 7 of the identity arc makes it the only path, so it has
to become segment-wise too.

## Step 1: segments on demand (iopsystems/metriken#157)

`SegmentedParquetReader::open_with_pool` takes a `SegmentStore`, a trait
that supplies segment bytes by position. Open reads every footer once,
builds the identity indexes, a per-segment span catalog and the column map,
and keeps neither the bytes nor the footer. A query opens only the segments
its range touches, one at a time — an iterator, deliberately, because a
collected list held every touched segment open for the whole query and the
cache bound then bounded nothing — through an LRU cache charged bytes plus
4 KiB per column (measured: a 500 MB budget under a 2 KiB charge held 850
MB) and bounded by the pool's byte budget. A segment the store has lost
since open is skipped, which is what a live hindsight buffer wants.

rez's `DbSegmentStore` implements it over the catalog: sealed segments by
sequence number through a lazily opened connection (or the shared one for a
byte-backed archive), and the live WAL tail materialized once at first
query as the newest segment. `SegmentSource::all()` — every segment's bytes
at once — survives only for the indexed path, until step 2.

Same fixture, after:

| operation | before | after |
|---|---|---|
| `mcp describe-metrics` | 4.4 GB, 5.0 s | 0.20 GB, 3.7 s |
| one filtered task series | 2.3 GB, 2.1 s | 0.67 GB, 3.7 s |
| all task series | 11.9 GB, 18.3 s | 10.1 GB, 19.6 s |
| viewer after `/sections` | 4.1 GB | 0.19 GB |
| viewer after one filtered task query | 4.4 GB | 1.34 GB |
| viewer after the all-task query | 8.6 GB | 5.6 GB, 6.4 GB on repeat |

The filtered query got slower because its footers are parsed twice now,
once at open and once when the query touches them; repeated queries find
them in the cache. The all-task query is cost 3, untouched by this step.

## Step 2: the indexed reader on the same store (iopsystems/metriken#159)

The indexed reader decoded every segment of a table into `RezTable`s and
assembled a `MemoryStore` of split series at first query — the whole table
in memory, which on the fixture above is the one thing step 1 had just
stopped doing for every other table.

It is now a relabelling of the segmented reader rather than a reader of its
own. metriken-query gained `ColumnRelabel`: at open a column says what label
sets it can present as (`identities`), which is what the identity indexes,
listings and column map are built from; at query time a counter or gauge
column's samples are cut into runs by occupant (`split`), histogram rows are
relabelled one at a time by timestamp (`at`), and each run is spliced as a
piece of the series it presents as, exactly as a plain series is. A filter
on a key the index supplies (`comm="redis"`) is not on the column, so
`segment_filter` turns it into the slots whose occupants match
(`id="3|17"`), the segment decodes those columns only, and the original
filter is applied to the relabelled runs afterwards.

rez's `OccupantRelabel` implements it over `Occupants`: one binary search
per run boundary, the overlay computed once per run. A slot the index never
named is read as it is. The `MemoryStore` assembly, the eager decode and
`SegmentSource::all()` are gone; `TableReader` has one variant.

Measured on delta's churn archive (the identity arc's fixture: ten minutes,
248k index entries on the task stream), `sum(rate(task_cpu_usage[1m]))`
five times each:

| | parquet path | indexed, `MemoryStore` (#1282) | indexed, relabel |
|---|---|---|---|
| query wall | 0.48–0.54 s | 0.58–0.65 s | 0.71–0.78 s |
| query max RSS | 193 MB | 180 MB | 199 MB |
| `describe-metrics` | 0.23 s, 104 MB | 0.86 s | 0.54 s, 142 MB |

Slower per query than the store it replaces on this small table — the
footers are parsed again per query, and the store had them decoded once —
and no longer proportional to the table: memory is the parquet path's plus
the occupancy spans. The reader oracle tests (both paths agree on a
dual-carrying archive; an index-only archive splits at the handover; a
retained tail attributes from the restatement) run through the relabel
path unchanged.

## Step 3: rate over a sample stream (iopsystems/metriken#161)

The all-task query was two materializations deep. `DataSource::counters`
returned every series whole, so 76M samples were resident before the first
point was computed; then the dispatcher ran each series' rate producer to
completion into a `Vec<Point>` before the pipeline saw it, because the
producer borrowed the samples and could not outlive them. The aggregate
downstream only ever needed one point per series.

The first cut — producers own their samples, no collected points — changed
nothing measurable (10.3 GB), which said the samples, not the points, were
the bulk. So the engine reads a series as a stream. `counter_streams` hands
out one `CounterStream` per series; the segmented source implements it by
reading one column of one segment at a time, from positions indexed at
open (sixteen bytes per column per segment, column labels interned), and
a relabelled series reads its column and keeps its runs. The grid rate
producer consumes the stream and keeps only the samples bracketing its
current interval — pulling the first nine for the typical spacing, as the
slice version took them — and lets the rest go. Its results are unchanged,
which the ten rate-semantics tests (holes, bands, interpolation flags,
explicit points, span) pin; the vector constructor those tests drive is a
stream over vectors.

Two things the profile found on the way, each a third or so of the query's
CPU before it was fixed: `Schema::index_of("duration")` on a schema without
that column formats every field name into its error, and it ran once per
column read, a million times; and the typical spacing was cloned and sorted
per emitted point.

Same fixture, release build, `/usr/bin/time -l`:

| operation | before | after step 1 | after step 3 |
|---|---|---|---|
| `mcp describe-metrics` | 4.4 GB, 5.0 s | 0.20 GB, 3.7 s | 0.25 GB, 4.3 s |
| one filtered task series | 2.3 GB, 2.1 s | 0.67 GB, 3.7 s | 0.46 GB, 3.6 s |
| all 6,644 task series, `sum(rate())` | 11.9 GB, 18.3 s | 10.1 GB, 19.6 s | 0.98 GB, 15.8 s |
| viewer after `/sections` | 4.1 GB | 0.19 GB | 0.25 GB |
| viewer after the all-task query | 8.6 GB, 9.4 GB on repeat | 5.6 GB | 3.0 GB |

The viewer's resident size after the all-task query is three times the
`mcp` peak for the same query because its two caches — opened segments and
decoded row groups — are each budgeted at `--cache-size-mb` (500 MB by
default) and both fill on that query; `mcp` runs with 256 MB. That is the
one knob left, and it is bounded.

Not done: gauge queries still materialize `Gauges` whole (the producers own
their samples, so no collected points, but the samples are resident); the
same stream shape would apply. Histogram streams were lazy already.

## The composition path (iopsystems/metriken#163)

Sean measured the same file from the other entry point: systemslab
composes every table of every `.rez` artifact through
`RezReader::composition_sources()`, which opened every table up front — 14 s
and 4 GB on this archive before step 1, and after it still every segment
footer of every table before any query. His `CompositionSource::lazy` is a
composition child that answers names, time range, interval and metadata
from a catalog and loads its source the first time a query names one of
its metrics; rez's `composition_sources()` now hands out one per table,
built from the catalog the reader already probes at open. `total_series_count`
on a composed reader asks each child (`DataSource::series_count`), so a
child with a catalog count answers without loading; rez's children carry no
count, so the fallback label walk loaded them. On an uncomposed `RezReader`
the same call opened every table, and the only consumer was a `num_series`
badge neither viewer rendered, so v5.22.1 removed the field and the call
(#1296; systemslab dropped its badge in iopsystems/systemslab#6268).

Two things the streaming rate needed there: `MultiParquetSource` and the
lazy child both hand out `counter_streams`, so a composed `rate()` streams
as an uncomposed one does. Without that the composition path had fallen to
the trait default and materialized every child's series again.

Measured through systemslab on 2026-09-24, upgrading its server from a
build pinned to the pre-step reader to one on v5.22.0 (iopsystems/systemslab#6263),
against an 82 MB `.rez` artifact:

| | before | after |
|---|---|---|
| server RSS idle | 1.78 GB (9 days up) | 0.14 GB |
| after `dashboard/sections` | 2.05 GB | 0.25 GB |
| after four plot queries | 2.08 GB | 0.29 GB |
| `dashboard/sections` | 0.33 s | 0.08 s |

Query values differed from the old server's and matched `rezolus mcp query`
on the downloaded artifact exactly: the old build pinned metriken-query
0.21.0, which still rounded sample timestamps to the grid (removed in
0.24.0).

## Path forward

1. Segments on demand — done.
2. The indexed reader on the same store — done.
3. Rate over a sample stream — done. Gauges the same way when a wide gauge
   table shows up.
4. The composition path lazy and streaming — done, and measured through
   systemslab (above).
5. The format itself: the sealed phase still lives in the live phase's
   container, so open probes footers through whole-blob reads and a
   consumer downloads a whole archive to read a source name. Designed as
   two forms of one archive in iopsystems/dendro#17, with three live-form
   additions (a caller-owned `stream_summary`, a range-readable `header`
   table, incremental blob reads) marked ready to build.

## Related

- iopsystems/dendro#17, the design this entry's costs argue for.
- iopsystems/systemslab#6263 (the bump), #6268 (the badge).
- [Internal labels](2026-09-22-internal-labels.md), whose "Measured: open
  cost" section found the indexed reader's replay cost; this entry is the
  parquet path's.
- iopsystems/metriken#157 (step 1, metriken-query 0.27.0).
