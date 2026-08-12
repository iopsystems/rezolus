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

## Still open after the measurements

- **Segment byte cap.** The cap is on *in-memory* bytes and encoded segments are
  much smaller (the fleet's 8 MiB cap yielded ~585 KB encoded for
  histogram-heavy `syscall_latency`, against a 1.4 MB all-segment average), so
  the fleet never approached the 41.6 ms insert. But the compression ratio
  varies by sampler and **has never been measured per-sampler**, so the worst
  case is unknown. Options: trim the cap (more segments — overhead grows
  superlinearly past ~25/table, and `syscall_latency` already hits 144 in
  900 s), or size the writer's bounded channel to absorb the worst observed
  burst (94.8 ms ≈ 2.1 ticks, so ≥3 ticks of depth). **Measure the per-sampler
  encoded/in-memory ratio before choosing** — it is one short fleet recording.
- **`page_size` left at the 4096 default and untested.** Larger pages would
  shorten overflow chains for multi-MB BLOBs. Treat as un-optimized, not chosen.
- **The `-wal` sidecar** reaches 24–79 MB depending on `wal_autocheckpoint` and
  persists at its high-water size; it must be counted in hindsight's footprint,
  or capped with `journal_size_limit` plus a checkpoint at finalize. The default
  autocheckpoint (1000 pages) measured best for tail latency.

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
