# `.rez` v3 — SQLite container with a real WAL

- **Opened:** 2026-08-12
- **Status:** OPEN — design landed pre-build.
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

## Open questions to settle during implementation

1. **Does eviction actually keep the file bounded without `VACUUM`?** SQLite
   reuses freed pages via its free list, so a steady-state hindsight buffer
   should plateau. If it does not, options are `auto_vacuum=INCREMENTAL` or
   periodic `VACUUM INTO` — both with real costs. **Measure first, on
   `10.1.0.1`, before building the rest.**
2. **BLOB insert throughput at fleet scale.** Segments are up to 8 MiB and the
   fleet run produced 22–605 MB archives; a segment insert must not stall the
   scrape loop. Incremental BLOB I/O exists if a single large insert is too
   coarse.
3. **WAL row encoding.** Msgpack per row is the obvious choice (reuses
   exposition types), but a row is a whole sampler's metrics at one timestamp —
   worth checking the size against the alternative of one WAL row per metric.
4. **Recovery ordering.** WAL rows for a sampler may straddle a segment that
   sealed but whose WAL prune did not commit (crash between). Pruning in the
   same transaction as the segment insert makes this impossible; confirm that
   is what the writer does.
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
