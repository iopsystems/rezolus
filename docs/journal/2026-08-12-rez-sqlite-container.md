# `.rez` v3 — SQLite container with a real WAL

- **Opened:** 2026-08-12
- **Status:** OPEN — design landed pre-build; **gating measurements passed
  2026-08-12 (both GO)**, and they amended three design decisions including a
  reversal of open question 4. See "Gating measurements" below.
- **Arc:** container replacement for the `.rez` work in
  [per-sampler `.rez` archive](2026-07-13-per-sampler-rez-archive.md),
  [`.rez` reader ecosystem](2026-07-15-rez-reader-ecosystem.md) and
  [streaming segmented writer](2026-08-11-rez-streaming-writer.md).
- **Owner:** Brian Martin
- **Repos:** rezolus only. metriken-query needs **no changes** —
  `SegmentedParquetReader::open_bytes_with_pool` already takes
  `Vec<Vec<u8>>`, so where segment bytes come from is not its concern.

This entry is the design spec (absorbs the brainstorm).

## Why — and why the tar container is being replaced weeks after it shipped

The [streaming writer](2026-08-11-rez-streaming-writer.md) landed in `5de241d9`
(PR #1041) and does what it set out to: finalize is bounded by the open
segments rather than recording length (fleet-measured 258.7–404.3 ms across a
30× length range). **That work is not being undone.** The per-sampler segment
model, seal policy, monotonic clock anchoring, and the published
`SegmentedParquetReader` all carry over unchanged. What changes is the
container underneath them, and the reason is that two new requirements arrived
that tar structurally cannot serve:

1. **A real WAL**, so an unclean kill loses one tick rather than an unsealed
   segment. The fleet measurement made this concrete and urgent: at `kill -9`
   120 s into a run, **only 10 of 26 tables recovered anything** — the other 16
   had zero sealed segments because their seal period (180 s, byte-capped and
   low-volume) exceeded the run. Kill-safety today is *per-table*, and for a
   quiet table the loss window is the whole recording.
2. **Unifying hindsight with the recorder.** Hindsight is presently a separate
   mechanism — a fixed-size ring of 4 KB-aligned slots overwritten in place
   (`src/hindsight/state.rs`), dumped to parquet on demand
   (`perform_dump_to_file`, `src/hindsight/mod.rs:316`). Nothing else can read
   it. If hindsight instead wrote sealed immutable segments, a dump becomes
   "copy the segments plus the current WAL" with **no tearing**, because the
   writer never mutates a sealed segment. One format, one reader, one writer.

Tar cannot do either. Entries carry their size in the header, so they can be
neither resized nor deleted: no eviction (hindsight's bounded retention), no
in-place update (a WAL), and no index (the authoritative manifest is found by
scanning — measured at 19.3 s on a pathological archive, and the reader slurps
the **entire archive into memory** to build its name→bytes map, which is fine
at 7 MB and untenable at the 605 MB the fleet run produced).

A directory (Prometheus's layout: `wal/` + block dirs) solves all of it, and
was rejected for one reason: **the user-facing artifact must stay a single
file**. The tempting compromise — work in a directory, pack to one file at the
end — is dead on arrival, because packing at finalize is an O(recording size)
copy, precisely the cost the previous effort eliminated. Single file **and**
O(1) finalize means the live format must already be the single file.

That leaves: single file, written in place, supporting eviction, in-place
updates, and indexed access. SQLite is the boring answer that provides all
four, plus two things we would otherwise hand-roll badly — crash consistency,
and **tear-free concurrent reads** (WAL-mode readers see a consistent snapshot
and never block the writer), which is exactly hindsight's dump problem solved
by the container instead of by us being careful.

## Why parquet blobs inside a database — the sandwich, defended

The obvious objection: SQLite is a storage engine, parquet is a storage engine,
and this design stacks one inside the other. The accurate description is that
**SQLite is used as a transactional allocator with a queryable catalog, not as
a query engine.** The two halves earn it very differently, and it is worth
being explicit about which is which.

**The WAL is where SQLite clearly pays for itself.** Per-tick durability with
crash consistency and torn-write detection is what it is built for, and it is
precisely where the tar design got things wrong: the adversarial review found a
persistence-ordering defect (write order is not persistence order), a
silently-short-read defect, and four distinct truncation geometries each
needing its own reasoning. All of that collapses into `COMMIT`. Hand-rolling a
durable WAL means rediscovering those bugs, probably not all of them.

**The segments are the weaker case, and the single-file constraint is what
carries it.** Storing opaque blobs in a B-tree costs page and overflow-chain
machinery for bytes SQLite cannot see into — measured at ~1% — and forfeits
predicate pushdown: there is no `SELECT WHERE metric = ... AND ts > ...`.

**Rejected: metrics as real rows** (`sampler, metric, labels, ts, value,
window_*`). It would give SQLite something to actually index, and it fails on
two counts. It discards the query engine published as metriken-query 0.17.0 —
`SegmentedParquetReader` plus the whole PromQL implementation, `rate()`
windowing, histogram quantiles, and the uncertainty-band machinery — and
replacing that is a second engine, not a refactor. And columnar compression is
load-bearing here: a histogram cell is a 7,424-bucket array that parquet packs
~14:1 when sparse; as rows it would be a blob anyway, minus dictionary encoding
on repeated labels and RLE on flat counters.

**Rejected: DuckDB.** It is columnar, reads parquet natively, and has a
single-file format, so it could plausibly replace both halves and make the
sandwich disappear. Rejected on storage-format stability across versions,
dependency weight for a fleet agent, and because continuous small appends are
not what it is tuned for. Recorded as a deliberate no rather than an oversight.

**Rejected: a bespoke indexed container.** Leaner and dependency-free, at the
price of owning the crash-consistency story that SQLite has now demonstrated it
handles — 0 torn reads, 0 `SQLITE_BUSY`, +0.7 ms writer impact across 92 s of
concurrent hammering.

The resulting shape is the conventional one: Iceberg and Delta keep metadata in
a catalog and data in parquet; Prometheus keeps an index plus opaque chunk
files. Here the manifest, segment index, and clock offsets are **real rows**
that SQLite indexes — "give me this sampler's segments overlapping this time
range" is an indexed lookup, not a scan — and only the bulk column data is
opaque.

*Reopen:* if the single-file constraint ever relaxes, a directory layout is
strictly better and most of this reasoning evaporates.

## Goal / GO criteria

- **Kill-loss ≤ one tick, for every table** — including quiet ones. This is the
  headline: it fixes the per-table finding above.
- **Finalize stays bounded** and no worse than v2's fleet figure (~300 ms
  median). It should get *cheaper* — there is no tar footer to write and no
  rename; finalize becomes "seal tails, commit".
- **No read-path regression** vs. v2 at fleet scale (v2 fleet baseline:
  `parquet metadata` 0.21 s, `mcp query sum(rate(cpu_usage[1m]))` 0.71 s on a
  197 MB / 149-segment archive). Open must not load the whole file.
- **Bounded file under hindsight eviction without `VACUUM`** — measured, not
  assumed. If freed pages are not reused in practice, the design needs a
  different eviction story and that is a NO-GO for the unification.
- **Sealing/WAL writes do not perturb the scrape loop** at fleet scale: 0
  skipped ticks at an attainable interval, matching v2 (boundary/interior delta
  median ratio 1.0015).
- **`.partial` is gone.** A `.rez` is always a valid, openable file; "was it
  cleanly finalized" remains a queryable property, not a filename convention.
- v2 tar archives stay readable (detection by magic bytes — SQLite's
  `"SQLite format 3\0"` header vs. tar; `is_rez_reader` at `rez.rs:834`
  becomes a two-format sniff).

Non-goals: changing the parquet segment encoding, changing metriken-query,
changing the `.rez` extension, or supporting concurrent *writers* to one file.

## Design

### Schema (sketch)

```sql
recordings(id, labels JSON, metadata JSON, complete, clock_anchor_wall_ns)
segments(recording_id, sampler, seq, rows, first_ts, last_ts, bytes BLOB,
         PRIMARY KEY (recording_id, sampler, seq))
wal(recording_id, sampler, ts, wall_offset, row BLOB,
    PRIMARY KEY (recording_id, sampler, ts))
clock_offsets(recording_id, ts, offset_ns)
```

The manifest stops being a JSON document rewritten on every checkpoint and
becomes rows — which removes the checkpoint concept entirely, along with the
duplicate-tar-name trick and the two-sync ordering protocol that existed only
because write order is not persistence order. A transaction replaces all of it.

### The WAL is per-sampler rows, not raw snapshots

Two candidates were considered. Storing the **raw msgpack snapshot** per tick
is simplest — it is exactly what the recorder scrapes, and recovery replays it
through the existing ingest path. But pruning then couples samplers together:
the WAL must retain back to the *oldest* unsealed sampler, so one slow table
(drivehealth seals every 300 s) pins ~300 s of *every* sampler's data. At fleet
scale that is 278 KB × 300 ≈ 83 MB of WAL to protect a handful of rows.

So the WAL stores **per-sampler rows** — the same rows that would be appended
to a `TableBuilder` — keyed by `(sampler, ts)`. Pruning is then per-sampler and
exact: when sampler X seals a segment, delete its WAL rows at or below that
segment's `last_ts`. A busy table's WAL stays tiny; a quiet table's WAL holds
its handful of unsealed rows and nothing more. Recovery loads WAL rows straight
into `TableBuilder`s, which is where they were headed anyway.

### Reading

`RezReader` opens the file, reads each sampler's segment BLOBs in `seq` order,
and hands them to `SegmentedParquetReader` exactly as today. The WAL tail is
materialized at open into an in-memory parquet segment and appended as the
newest segment for that sampler — so the reader sees one continuous timeline
and **the splice machinery, conflict policy, and uncertainty bands all work
unchanged**. A recording being written concurrently reads consistently, by
SQLite's WAL-mode guarantee rather than by our own truncation tolerance.

### Hindsight

Becomes the same writer with retention configured: seal normally, and
`DELETE FROM segments WHERE last_ts < now - lookback` (plus the matching WAL
prune). Dump becomes a query, or a file copy, or `VACUUM INTO` — all
consistent by construction. The 4 KB slot ring, `snapshot_len`/`snapshot_count`
sizing, and the separate dump-to-parquet path all go away.

### Durability

SQLite in WAL mode with `synchronous=FULL` fsyncs on every commit — one commit
per tick, which at 1 Hz (and at the fleet-measured ~46 ms scrape-bound cadence)
is affordable, and is the setting that survives power loss rather than only
process death. This must be **measured**, not assumed; `synchronous=NORMAL` is
the fallback if per-commit fsync perturbs the loop, at the cost of power-loss
durability.

## Gating measurements (2026-08-12, `10.1.0.1`, NVMe/ext4, SQLite 3.53.2)

Standalone `rusqlite` harness, schema as sketched above, `journal_mode=WAL`,
`page_size=4096`. **All durability-sensitive timings on NVMe** — `/tmp` on that
host is tmpfs, where fsync is meaningless and would have made
`synchronous=FULL` look 3.4× cheaper than it is. Budget throughout is the
fleet-measured **~46 ms tick**.

### 1. Eviction without `VACUUM` — **GO**

Freed pages are reused. Steady state plateaus and *stays* there through 6–12×
turnover of the entire working set:

| BLOB | live | db / live | drift after plateau |
|---|---|---|---|
| 0.5 MiB | 226 MB | **1.0037** | none (5,000 cycles) |
| 1.4 MB | 634 MB | **1.0041** | none (5,000 cycles) |
| 4 MiB | 839 MB | **1.0062** | none (2,400 cycles) |
| 8 MiB | 839 MB | **1.0111** | none (1,200 cycles) |

The realistic mix (segments + 26 WAL rows/tick + prune) drifted **20 KB over
4,800 cycles**. With segment sizes drawn randomly 0.5–8 MiB, `page_count` was
flat across the last 2,600 cycles — 6× turnover — at 1.02× the high-water live
size. Overflow-page chains cost ~1% at the largest BLOB, not a blowup.

**Caveat, and it shapes the design:** the bound is the **high-water mark**, not
current size. Shrinking the working set 16× left the file at **16.0× live**
(1.5 GB parked on the free list, reusable but never returned to the OS). Bursty
fleet data — a syscall storm making one table seal far more often — would
permanently inflate a hindsight file to the worst minute it ever saw.

**Therefore: create the DB with `auto_vacuum=INCREMENTAL` from day one.** It is
free in steady state (per-cycle txn p50 **8.230 ms** vs **8.807 ms** for
`NONE`) and costs +0.12% space for pointer maps — and it **cannot be enabled
later without a full `VACUUM`**, so it is a build-time decision, not a tuning
knob. Reclaim then trickles: `incremental_vacuum(100)` is p50 **3.8 ms** /
p90 11.4 ms, inside the tick. Full reclaim of 1.5 GB takes 12.1 s if ever
wanted; `VACUUM INTO` runs at ~530 MB/s and *is already the hindsight dump
operation*, so hindsight gets compaction free at dump time.

### 2. Insert cost — **GO, after two changes**

Isolated costs fit with room. Segment insert (`synchronous=FULL`, NVMe):

| encoded BLOB | p50 / p99 |
|---|---|
| 0.5 MiB | 3.8 / 12.6 ms |
| **1.4 MiB (fleet average)** | **5.5 / 17.4 ms** |
| 4 MiB | 22.9 / 28.7 ms |
| 8 MiB | 41.6 / 47.5 ms |

Per-tick WAL commit (26 rows, one txn) is p50 **3.6 ms** / p99 12.1 ms at
measured row sizes, and still only p50 16.2 ms in the pathological case where
every sampler is as large as `cpu_usage`. Plain `INSERT` beats incremental
BLOB I/O — `blob_open` is **15–18% slower** at 4–8 MiB, so it is not needed.

**But the design as written stalls, and not where expected.** The combined
workload (46 ms paced ticks, fleet-derived seal periods, 120 s retention) gave
seal ticks of p90 **212.7 ms** / max 517.7 ms, 30 overruns per 4,000 ticks. The
culprit is the **in-transaction WAL prune** (p90 78 ms, max 245 ms, deleting up
to 12,855 rows), not the segment insert (p50 5.4 ms). A quiet sampler sealing
every 300 s has ~6,500 WAL rows to delete in one commit.

| variant | seal-tick p50 / p90 / max | overruns / 4000 |
|---|---|---|
| as written (full rows, prune in-txn) | 40.4 / 212.7 / 517.7 | 30 |
| value-only rows | 35.6 / 149.0 / 405.5 | 21 |
| prune bounded to 200 rows/txn | 34.8 / 84.8 / 199.4 | 15 |
| prune deferred outside txn | 25.4 / 44.4 / 100.9 | 5 |
| **value-only + deferred prune (adopted)** | **22.6 / 41.5 / 94.8** | **5 (0.125%)** |

**Keep `synchronous=FULL`.** Against `NORMAL` on the combined workload it is 5×
more expensive at p50 (3.78 vs 0.75 ms) and **no better at any percentile that
threatens the budget** (p99 33.5 vs 37.1; overruns 23 vs 27) — the tail is
checkpoint and prune work, not fsync. Above 4 MiB the two are indistinguishable
outright. Power-loss durability costs nothing where it matters.

### 3. Concurrent reader — clean pass

A second WAL-mode connection reading every segment BLOB and checksumming real
bytes against writer-maintained counters, for 92 s: **0 torn reads, 0
`SQLITE_BUSY`, 0 errors**. Writer impact **+0.7 ms** (p50 4.494 vs 3.780 ms).
The tear-free-dump premise holds.

### `page_size` selection — **measured, and 4096 kept**

`PRAGMA page_size` only takes effect on a database that does not yet exist;
changing it later means `journal_mode=delete` + full `VACUUM` + back to WAL, on
every file already written. So it is swept before any code writes a v3 file.
Sweep on `10.1.0.1`, **databases on NVMe** (`/dev/nvme0n1p2`, ext4, under
`$HOME`; only the throwaway harness lived on the tmpfs `/tmp`), SQLite 3.53.2,
`journal_mode=WAL` + `synchronous=FULL` + `auto_vacuum=INCREMENTAL` fixed
throughout. Budget is the fleet-measured **~46 ms tick**.

**Two of the effects that look like `page_size` are other knobs**, and finding
them changed the answer:

1. **WAL checkpoint volume.** `wal_autocheckpoint` defaults to *1000 pages*, so
   at 64 KiB it fires after 64 MB instead of 4 MB. Held at 1000 pages, larger
   pages looked faster at p50 (1.4 MiB insert 8.0 → 4.5 ms) and much worse at
   p99 (19.2 → 72.0 ms) — both artifacts of checkpoint spacing. Held at a
   **constant 4 MiB of WAL** (`wal_autocheckpoint = 4 MiB / page_size`), the
   steady-state p99 is flat at 15.6–18.2 ms for *every* page size. Everything
   below is measured at the constant-byte cap.
2. **The 2 MiB default page cache.** Most of the small-page read penalty is
   `cache_size`, not chain length: at 4096, warm sequential BLOB reads go
   **229.6 → 409.6 MB/s** with `cache_size=-262144`, and 460.2 MB/s adding
   `mmap_size`. The 4096-vs-65536 read gap shrinks from 2.3× to **1.35×**.

Segment insert, autocommit (one fsync each), 300/300/200 iterations, p50 / p99
ms:

| `page_size` | 0.5 MiB | 1.4 MiB (fleet avg) | 4 MiB |
|---|---|---|---|
| **4096** | 3.85 / 14.7 | **8.03 / 19.2** | 29.0 / 35.2 |
| 8192 | 3.61 / 13.7 | 7.48 / 17.5 | 24.1 / 30.5 |
| 16384 | 3.49 / 12.9 | 7.11 / 15.9 | 22.2 / 26.3 |
| 32768 | 3.40 / 12.2 | 7.07 / 16.6 | 21.8 / 28.2 |
| 65536 | 3.41 / 11.2 | 6.92 / 15.9 | 21.7 / 27.2 |

Per-tick commit (26 rows × 1,925 B, one txn), WAL bytes written per tick,
realistic mixed workload (2,500 ticks, 26 samplers, staggered seals, deferred
prune, 8 segments/sampler retained = 292.5 MB live), and `-wal` high-water:

| `page_size` | tick p50 / p99 / max | WAL B/tick | ×payload | mix db/live (late range) | freelist | `-wal` hw: default / 4 MiB cap | warm read: default / 256 MiB cache |
|---|---|---|---|---|---|---|---|
| **4096** | **2.70 / 11.1 / 15.1** | **156,920** | **3.14×** | **1.0117** (1.0084–1.0132) | 0.87% | **4.4 / 4.4 MB** | 230 / 410 MB/s |
| 8192 | 2.71 / 7.9 / 14.9 | 200,142 | 4.00× | 1.0108 (1.0076–1.0123) | 0.86% | 8.8 / 4.4 MB | 347 / 556 MB/s |
| 16384 | 2.77 / 8.5 / 35.0 | 236,726 | 4.73× | 1.0112 (1.0080–1.0127) | 0.88% | 17.8 / 4.6 MB | 464 / 564 MB/s |
| 32768 | 2.89 / 9.0 / 37.0 | 308,245 | 6.16× | 1.0171 (1.0139–1.0187) | 0.86% | 34.2 / 5.0 MB | 537 / 599 MB/s |
| 65536 | 3.26 / 9.7 / 43.2 | 410,570 | 8.20× | 1.0180 (1.0148–1.0196) | 0.86% | 67.3 / 5.6 MB | 534 / 624 MB/s |

**Eviction stays bounded at every page size** — the mix plateaus at 1.011–1.018×
live and does not drift, and the free list holds a near-constant **2.55–2.59 MB
regardless of page size** (631 pages at 4096, 39 at 65536). The
free-list-granularity worry did not materialize; what does show up is that
32768/65536 park 5.5 MB more on a 292 MB file (1.0180 vs 1.0117) because the
reuse unit is coarser.

**The overflow-chain hypothesis was right about the mechanism and small about
the magnitude.** The 1.4 → 4 MiB throughput dip is chains: the 4 MiB/1.4 MiB
throughput ratio goes 0.83 at 4096 to **0.96 at 65536** — the dip nearly
disappears. But flattening it is worth 145 → 193 MiB/s at 4 MiB, i.e. **7.3 ms
on a 4 MiB segment**, against a 46 ms tick. (This harness's 4096 baseline runs
~1.4× slower than measurement 2 above — `auto_vacuum` on, a `wal` table
present, shared host — so read the sweep as relative within itself.)

**Decision: keep `page_size=4096`. Measured and kept, not defaulted into.**

- What larger pages actually buy, after the two confounds are removed: **−11% on
  the average segment insert** (8.03 → 7.11 ms at 16384, 0.9 ms of a 46 ms
  tick), −23% at 4 MiB, and **+26% on warm reads** (460 → 578 MB/s) once
  `cache_size` and `mmap_size` are raised — down from +102% before them.
  `VACUUM INTO` — the hindsight dump — goes 470 → 667 →
  712 MB/s. Real, but none of it is binding: §"the binding constraint is co-seal
  lockstep" already showed segment insert is not what overruns the tick, and
  staggering fixes that at zero cost.
- What 4096 buys, and it is not tunable away: **the lowest per-tick WAL
  amplification, 3.14× vs 4.73× at 16384 and 8.20× at 65536.** That is
  156,920 B/tick vs 236,726 / 410,570 — **3.4 vs 5.1 / 8.9 MB/s written
  continuously, on every fleet host, forever**, to persist 50,050 B/tick of
  rows. It is the one operation that runs every tick, and it is the only place
  the page size shows up as an unavoidable cost rather than a preference.
- Per-tick commit *latency* does not discriminate (2.70 → 3.26 ms p50), but the
  per-tick **tail** does, in the wrong direction: mix tick max 15.1 ms at 4096
  vs 35.0–43.2 ms at ≥16384, from larger checkpoint units.
- 4096 also gives the smallest `-wal` sidecar under any checkpoint policy and
  the finest free-list granularity for hindsight's reuse.
- The asymmetry decides it. `cache_size` and `wal_autocheckpoint` are runtime
  pragmas, changeable on any file at any time; `page_size` is welded in at
  creation. **73–80% of the apparent large-page win came from those two
  reversible knobs** (80% of the 1.4 MiB insert gain, 73% of the read gain).
  Spending the irreversible decision to buy back the remainder — while paying
  1.5–2.6× the WAL write volume on every tick — is the wrong trade.

**Two runtime pragmas fall out of this and should ship with v3** (both
reversible, both worth more than the page size was): set
**`wal_autocheckpoint` in bytes, not pages** — at 4096 that is the ~1000-page
default already, so nothing changes today, but it stops the sidecar and the p99
from tracking any future page-size change; and **raise `cache_size` on reader
connections** (256 MiB measured `-262144`) — **+78% on segment reads at 4096**,
229.6 → 409.6 MB/s, the single largest read-side win found in this sweep and
entirely free.

## Design amendments from the measurements

1. **The WAL prune moves OUT of the seal transaction** — this reverses open
   question 4 below, which proposed the opposite. Putting the prune in the seal
   txn does make a straddle impossible, but costs p90 78 ms. The cheaper answer
   makes recovery tolerate the straddle instead: **on replay, drop WAL rows at
   or below each sampler's maximum sealed `last_ts`.** One idempotent rule,
   pruning becomes a background job with no correctness role, worth p90
   212.7 → 44.4 ms on seal ticks.
2. **WAL rows are values-only, not full msgpack** — settles open question 3.
   Measured on a real 283,673-byte fleet snapshot decoded into its 26 sampler
   groups: **1,925 B vs 10,908 B per sampler per tick**. That is 1.1 vs
   6.2 MB/s of WAL churn at fleet cadence, and a 74.8 MiB vs 424.1 MiB `wal`
   table at 120 s retention.
3. **`auto_vacuum=INCREMENTAL` at creation** — see the high-water caveat above.
   Must be decided before the first table exists.

## Per-sampler compression ratios (2026-08-12) — and why the cap is the wrong knob

Two 20-minute fleet recordings, per-segment sizes read from the tar, in-memory
bytes computed **exactly as `push_row` does** (non-null cells only) rather than
estimated. Validated against a known case: a byte-capped `syscall_latency`
segment computes to 8,414,208 B = 132 rows × 63,744 B/row — the first row
crossing the 8,388,608 B cap. All three prior anchors reproduced (mean segment
1.42 MB vs 1.4; `syscall_latency` 13.0:1 vs ~14:1; worst stall 85.9 ms vs 94.8).

**The ratio spans 1.32:1 to 62:1 — a 47× spread** — but is tight *within* a
table (±5%). It is predictable per-sampler and useless as a global constant.

| sampler | segs | encoded p99 | ratio | seal reason |
|---|---|---|---|---|
| `cpu_usage` | 39 | 6,530,074 | **1.32** | byte |
| `cpu_branch` / `cpu_l3` | 7 | ~5,183,000 | 1.62 | byte+row |
| `scheduler_runqueue` | 46 | 3,151,180 | 2.94 | byte |
| `syscall_latency` | 190 | 714,311 | 13.3 | byte |
| `blockio_latency` | 48 | 307,221 | 33.2 | byte |
| `tcp_connect_latency` | 12 | 190,024 | **62.3** | byte |
| `drivehealth` | 4 | 5,963 | 0.04 | short |

The driver is **entropy, not histogram-ness**: `cpu_bandwidth` is pure scalar
yet compresses 13.9:1 because its 14 counters are near-constant, while
`cpu_usage` manages 1.32:1 on 2,363 columns of distinct per-CPU monotonic
counters. Sparse histograms compress best (`tcp_connect_latency` pays 3,984 B
per row for a 496-bucket histogram that is nearly all zeros). Sub-1.0 ratios on
narrow tables are real: the archive writes `value + window_begin + window_width`
(24 B/scalar cell) against 16 B accounted, plus a ~6 KB parquet footer floor
per segment.

### The binding constraint is co-seal lockstep, not segment size

`maybe_seal()` seals every due table as one batch. The row-capped tables all
advance exactly one row per tick from row 0, so they reach 4,096 **in permanent
lockstep** — 12 tables sealing together every 4096 × 48 ms ≈ 197 s, for a
**16.16 MiB batch at 85.9 ms p99 ≈ 1.87 ticks**. That is almost certainly the
94.8 ms worst case observed in the SQLite combined workload. Only **7 of 467
batches (1.5%) exceed the tick, and every one is a co-seal event** — not a
single one is a large individual segment. The worst single segment
(`cpu_usage`, 6.23 MiB → 39.2 ms p99) fits, with 15% headroom.

At fleet cadence **nothing is age-capped**: 4,096 rows arrive in 197 s, well
inside the 300 s bound. 15 tables are byte-capped, 10 row-capped.

### Decision: keep `max_bytes` at 8 MiB

Lowering it is expensive and aims at the wrong target. Halving to 4 MiB would
take total segments from 582 to ~1,100 (and `syscall_latency` from 190 to 380,
deep into the superlinear region) to shrink segments that are *already* 0.68 MB
— and would do **nothing** about the six lockstep events, because every table
in them is row-capped, not byte-capped. Two targeted changes beat it:

1. **Stagger seal deadlines per sampler** (offset by a hash of the sampler
   name, or randomize the initial deadline). Caps the batch at its largest
   member — 5.18 MiB → 27 ms p99 — and eliminates all 7 over-budget batches at
   **zero segment-count cost**. This is the whole problem.
2. **Fix `approx_bytes`** (below). It is the precisely-targeted version of
   "lower the cap": it shrinks only the poorly-compressing scalar tables and
   leaves the 13–62:1 histogram tables alone.

| | worst encoded | p99 | total segments |
|---|---|---|---|
| today | 6.23 MiB | 39.2 ms | 582 |
| accounting fixed | 2.49 MiB | **22.1 ms** | 758 |
| cap halved to 4 MiB | 3.11 MiB | 24.8 ms | ~1,100 |

### Found in passing: `approx_bytes` undercounts memory 2.5× (a **v2 bug**)

`push_row` charges 16 B per scalar cell, but every cell also pushes an
`Option<Window>` into `col.windows`, which is never counted. Measured on the
host: `Option<Window>` is **24 B**, so a scalar cell truly costs **40 B — 2.50×
the accounted 16 B**. Histogram cells are honest (1.01×). Consequence: an
8 MiB-capped scalar table holds **20.0 MiB** of builder RSS. The cap is a
memory bound that is 2.5× wrong in the dangerous direction, and this is live in
merged v2 (`5de241d9`), not just a v3 concern.

### For v3: express the cap as a target *encoded* size

A single global in-memory cap is mismatched at both ends — it makes
`syscall_latency` emit 190 segments of 0.63 MB (7.6× past the ~25/table
guidance, and 10× smaller than they need to be) while letting `cpu_usage` emit
6.23 MiB ones. Now that the per-table ratio is measured and stable within ±5%,
a cap of *target encoded size × an EWMA of the table's observed ratio* fixes
both ends: `syscall_latency` drops to ~48 segments, `cpu_usage` to ~3 MiB each.

### Still un-tuned (measured, but not optimized)

- ~~**`page_size` left at the 4096 default and untested.**~~ **Swept
  {4096…65536} and 4096 kept** — see "`page_size` selection" above. Larger pages
  do shorten the chains, but the win is ≤26% on non-binding operations while
  per-tick WAL amplification rises 3.14× → 8.20×.
- **The `-wal` sidecar** reaches 24–79 MB depending on `wal_autocheckpoint` and
  persists at its high-water size; it must be counted in hindsight's footprint,
  or capped with `journal_size_limit` plus a checkpoint at finalize. The
  autocheckpoint is best expressed **in bytes** (`4 MiB / page_size` pages): at
  4096 that is the ~1000-page default, and the high-water then holds at 4.4 MB.
  **Under a dump it does not hold** — a held read mark stops the log being
  recycled — see § "Dump semantics, verified" for the measured shape.

## Dump semantics, verified (2026-08-12) — and one bug found doing it

The dump's snapshot semantics were correct by design and thin on tests. Three
things came out of closing that gap; the third is a defect.

### "No eviction pause is needed" is now a test, not an argument

`copy_range` reads inside one transaction, so retention deleting a segment
mid-dump cannot disturb the copy. That was previously argued and skipped as a
timing race. It does not have to be one: `copy_range` takes a `listed: &dyn
Fn()` that fires **after `read_recordings` has pinned the snapshot and before
the first segment BLOB is read**, and is `&|| {}` in production. The test evicts
three of six segments from a second connection inside that callback and asserts
the dump still holds all six with readable bytes.

A callback over a `cfg(test)` hook or a list/copy split, deliberately: it is the
only one of the three where the statements that ship and the statements the test
runs are the same statements in the same order. Removing `read_snapshot`'s
`BEGIN DEFERRED` turns the test red on exactly its own assertion (the dump comes
back with the three surviving segments), and — worth noting — turns *no other
test* red. Nothing else covered it.

### `-wal` under a dump: bounded by the dump's duration, not by the buffer

The journal had the sidecar measured only for the no-dump case (4.4 MB at the
4 MiB `wal_autocheckpoint`). Under a dump it is unbounded in principle, because
a held read mark stops SQLite recycling the log.

Measured locally (macOS, 10 Hz, 2,000-counter payload, two-row segments, a
buffer of a dozen segments so a dump takes ~250 ms; `tests/hindsight_dump.rs`):

| | `-wal` |
|---|---|
| steady state, no dump | **4.67 MB** (plateau; matches the 4.4 MB figure) |
| after ONE ~250-330 ms dump | 4.67-4.86 MB (**+0 to +185 KB**) |
| after ~2.2 s of back-to-back dumps | **13.9-18.8 MB** (+9.1 to +13.9 MB, **3.7-6.4 MB/s**) |
| 1 s after the last dump returned | unchanged, **+0 B**, every run |

The shape: it grows at the writer's WAL byte rate for as long as a read mark is
held, stops dead when it is released, and **does not shrink** — SQLite recycles
the log in place, so the file is the high-water mark. A short dump often costs
nothing at all, because its frames fit in the headroom the file already has.

The rate is not portable (it is payload × cadence × the 3.14× amplification),
so the test asserts the shape — the file passes its *unpinned* high-water by
half again — and prints the numbers. Removing the read mark leaves the same
window growing by **4,120 B, one page**, which is what a threshold of `during >
before` would have accepted; the assertion is written against the plateau for
that reason.

The fleet-scale version (a multi-GB buffer, 26 samplers, a `VACUUM INTO` lasting
seconds) has not been run. The scaling law to check it against is
`growth ≈ WAL byte rate × dump duration`, capped by nothing.

### BUG (fixed): `POST /dump/file` and the SIGHUP capture pause the recording

`rezolus hindsight --help` promises "a snapshot is a consistent point-in-time
copy taken without pausing the recording", and for `GET /dump` that holds — it
builds the copy on a blocking task while the loop ticks on. `POST /dump/file`
does not. The request is handed to the recording loop's own `select!`, and the
handler body awaits `spawn_blocking(dump)`; `select!` does not poll the tick
branch while a handler body is running, so **the scrape loop is stopped for the
whole dump**. `MissedTickBehavior::Skip` then discards the ticks, so they are
lost rather than deferred. The SIGHUP path is plainer still: `dump_to_file` is
called synchronously in the loop body.

Measured at 10 Hz against a buffer whose dump takes ~250 ms, over a one-second
window of back-to-back dumps:

| | ticks | segments sealed |
|---|---|---|
| `GET /dump` | **12** | 6 |
| `POST /dump/file` | **4** | 2 |

It scales with dump duration, i.e. with buffer size — so it is worst on the
large buffers an incident is actually captured from, and it drops data from the
minutes *after* the trigger, which for a rolling window is data an operator was
counting on.

**Pre-existing, not a v3 regression:** the same `select!` shape is in the v2
loop (`git show a569546e^:src/hindsight/mod.rs`, lines 226 and 286). What v3
added was a dump slow enough to measure it.

**Fixed** by taking every dump off the recording loop. The loop now *spawns*
each dump — HTTP requests into a `JoinSet`, the SIGHUP capture onto its own
task reporting back through a channel — and goes straight back to the
`select!`; the reply still travels the request's `oneshot`, so a caller (and a
failure) reaches its destination exactly as before, just from the spawned task
rather than the arm. The capture's completion is logged where it always was, in
the loop, off a `capture_rx` arm. Dumps are serialized against each other by a
`Mutex` taken *inside* the spawned task, which keeps the one-at-a-time property
the in-loop version had for free without giving the loop back the wait. Same
numbers, same fixture, after the fix:

| | ticks | segments sealed |
|---|---|---|
| `GET /dump` | 14 → **12** | 6 → 5 |
| `POST /dump/file` | 2 → **13** | 1 → 6 |
| SIGHUP | 1 → **12** | 1 → 5 |

(10 ticks and ~5 seals are due per second; the left-hand figures are the same
build with only `src/hindsight/mod.rs` reverted.)

Note the seal count is the blunter instrument of the two: seals are row-driven,
so pausing `maintain()` alone *delays* seals rather than losing them. It is
blunt but not useless — a loop stopped for most of a second does show up in it,
as 1 seal against the 5 due.

Two decisions the fix had to make and did not have to before. **Two dumps at
once** serialize rather than run concurrently or be rejected: they all write the
same output path, and while `buffer::dump` stages and renames (so nothing
tears), a caller told "your dump is at *path*" should not be reading another
request's copy. **A dump in flight at shutdown** is waited for, not abandoned:
its caller is still holding a request open, and the buffer it is reading lives
in the staging directory the daemon deletes on the way out. A third signal
still exits immediately, which is the escape hatch if that wait is unwelcome.

The SIGHUP capture's fixture needed its own calibration, and it is a good
example of how easily this kind of test goes vacuous: SIGHUP takes no time
range, so it is a whole-file `VACUUM INTO` rather than the ranged copy the HTTP
tests take, and at the HTTP fixture's 12 segments it finished in ~66 ms —
*inside* one 100 ms tick, where a fully paused daemon still makes nearly every
tick and the test passes on broken code. At 40 segments (~220 ms) the paused
daemon still scraped 3 of the 4 seals the assertion demands, through the gaps
between captures. The test runs at 60 segments (~330 ms), where the unfixed
daemon manages 1.

## Open questions to settle during implementation

1. ~~**Does eviction keep the file bounded without `VACUUM`?**~~ **Measured:
   yes** (1.004–1.011× steady state). But the bound is the high-water mark, so
   `auto_vacuum=INCREMENTAL` is adopted at creation — see amendments.
2. ~~**BLOB insert throughput at fleet scale.**~~ **Measured:** 5.5 ms p50 at
   the fleet's 1.4 MB average; plain `INSERT` beats `blob_open` by 15–18%. The
   stall was the WAL prune, not the insert — see amendments.
3. ~~**WAL row encoding.**~~ **Settled by measurement: values-only** (1,925 B
   vs 10,908 B per sampler per tick, from a real fleet snapshot).
4. ~~**Recovery ordering.**~~ **REVERSED by measurement.** In-transaction
   pruning costs p90 78 ms / max 245 ms. The prune is now deferred outside the
   seal transaction, and recovery tolerates the straddle: drop WAL rows at or
   below each sampler's max sealed `last_ts`.
5. **Multi-recording archives.** v2 supports several recordings in one `.rez`
   (multi-host / A-B, built by `parquet combine`). `combine` becomes an
   `INSERT ... SELECT` across files — likely simpler, but the ergonomics need
   checking.
6. **Dependency weight.** `rusqlite` with the bundled feature compiles SQLite
   from source (slower builds, no system dep) vs. linking the system library
   (faster, but a runtime dependency for a tool that ships as a static-ish
   binary). Bundled is probably right for a fleet agent; confirm against the
   Debian packaging in `Cargo.toml`'s `[package.metadata.deb]`.

## Testing plan

- Kill-loss: SIGKILL at fleet scale; **every** table recovers to within one
  tick (the v2 result was 10 of 26 tables recovering at all).
- Hindsight bounded-file: run past the retention window and show the file size
  plateaus without `VACUUM`.
- Concurrent read during write: dump/query while recording, assert no tearing
  and no writer stall.
- Read-path parity vs. the v2 fleet baseline on an equivalent archive.
- v2 tar archives still open (format sniff by magic bytes).
- Cadence: 0 skipped ticks at an attainable interval, seal-boundary deltas
  indistinguishable from interior — matching v2.
- Crash consistency: kill mid-commit repeatedly, assert the file always opens
  and never reports a segment whose bytes are absent.

## Deferred

- **v2 → v3 conversion tool.** Reading v2 stays supported; a converter is only
  needed if someone wants to bring old recordings into the new tooling.
  *Reopen:* on demand.
- **Compactor.** The v2 backlog item (merge segments offline) may be
  unnecessary here — `DELETE` + re-`INSERT` of merged BLOBs is a transaction,
  and the read path no longer pays per-segment footer costs the same way.
  *Reopen:* if segment counts (144 for `syscall_latency` in 900 s at fleet
  scale) prove expensive to query.
