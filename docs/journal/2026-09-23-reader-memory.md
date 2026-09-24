# Long recordings: memory proportional to the query, not the file

- **Opened:** 2026-09-23
- **Status:** **OPEN — step 1 of 3 in review.**
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

## Path forward

1. Segments on demand — this step.
2. The indexed reader (`crates/rez/src/indexed.rs`) becomes segment-wise on
   the same store, so the cutover (identity arc step 7) does not regress
   this.
3. Streaming aggregation in the engine for `sum`/`count`/`avg`/`min`/`max`
   over `rate`/`irate` and raw selectors: fold series as they are read
   instead of collecting them all. This is what gets the all-task query
   under a gigabyte.

## Related

- [Internal labels](2026-09-22-internal-labels.md), whose "Measured: open
  cost" section found the indexed reader's replay cost; this entry is the
  parquet path's.
- iopsystems/metriken#157 (step 1, metriken-query 0.27.0).
