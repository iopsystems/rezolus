# Streaming segmented `.rez` writer — bounded-time finalization

- **Opened:** 2026-08-11
- **Status:** OPEN — design landed pre-build.
- **Arc:** continuation of the `.rez` container work in
  [per-sampler `.rez` archive](2026-07-13-per-sampler-rez-archive.md) and
  [`.rez` reader ecosystem](2026-07-15-rez-reader-ecosystem.md).
- **Owner:** Brian Martin
- **Repos:** rezolus only (`src/recorder/rez.rs`, `src/recorder/mod.rs`,
  `src/rez_reader.rs`).

This entry is the design spec (absorbs the brainstorm).

## Why

Long recordings must be stoppable quickly. The concrete driver: a client stops
the recorder on docker tear-down, where SIGTERM is followed by SIGKILL after a
short grace window (commonly ~10 s). Today the recorder's finalize cost scales
with recording length, so a long recording cannot finalize inside that window.

Two specific defects in the current `.rez` path (verified against
`src/recorder/mod.rs` and `src/recorder/rez.rs` on 2026-08-11):

1. **Double-write.** In `.rez` mode every scraped snapshot is ingested into
   `RezRecorder` *and* re-serialized to msgpack
   (`rmp_serde::encode::to_vec`, `mod.rs:794`) and written to a temp spool
   (`ew.writer.write_all`, `mod.rs:815`) — but `.rez` finalization never reads
   that spool (`rez_mode` short-circuits to `rez_recorder.finalize`,
   `mod.rs:918-930`). The re-serialization and spool write are pure waste.
2. **Unbounded memory + O(recording) finalize.** `RezRecorder` (`rez.rs:736`)
   holds the entire recording in in-memory `TableBuilder`s (`rez.rs:642`).
   `finalize` encodes every table to parquet and writes the whole tar at the
   end (`rez.rs:828`, `write_archive` at `rez.rs:391`). Both memory footprint
   and finalize time grow with recording length.

The classic single-parquet path has the same finalize shape (replay the whole
msgpack spool through `MsgpackToParquet`) but for a structural reason: its one
wide schema is only knowable once recording ends. It is a **non-goal** here;
`.rez` is the target because per-sampler tables mean per-sampler schemas,
which makes incremental parquet writing tractable.

## Goal / GO criteria

- Finalize time bounded by a constant (the segment-seal thresholds),
  independent of recording length. Target: well inside a 10 s docker grace
  window; expected sub-second to low seconds. Close-out must carry a measured
  finalize time for a long (multi-hour) recording.
- Memory bounded by one open segment per sampler plus a small bounded seal
  queue.
- No missed or late scrape ticks attributable to sealing — parquet encode and
  tar IO run off the scrape thread (see "Write pipeline threading").
- The msgpack spool double-write in `.rez` mode is eliminated.
- A recording killed without finalize (SIGKILL, crash) remains a readable
  archive up to its last sealed segment; after power loss, up to the last
  synced seal. Readers can distinguish a cleanly finalized recording from a
  recovered one (`complete` marker).
- Existing v1 `.rez` archives remain readable; all current consumers (viewer
  server, MCP, `parquet metadata/annotate/combine/filter`) work unchanged on
  v2 output.

Non-goals: the classic parquet path (above); multi-endpoint `.rez` (still
single-endpoint, guard at `mod.rs:661`); WASM viewer `.rez` support (it reads
only parquet today — `crates/viewer` has no `.rez` reader).

## Design

Two load-bearing observations from the code made this design small:

- `read_archive_reader` (`rez.rs:527`) already does last-entry-wins for
  duplicate tar names (both the manifest and the parquet-bytes map), so
  periodic `manifest.json` checkpoint entries are legal without touching that
  logic.
- `TableBuilder` already null back-pads late-appearing columns
  (`rez.rs:669-678`), so schema growth (new cgroups, interfaces, late
  samplers) needs no special casing at write time: within a segment the
  padding handles it; across segments the reader unions schemas.

### Writer (`src/recorder/rez.rs`)

`RezRecorder` becomes streaming:

- Per-sampler state is today's `TableBuilder` — including the `last_key`
  window-advance dedup — but bounded. When a builder reaches a **seal
  threshold**, the segment is sealed: encoded with the existing
  `write_table_parquet` (`rez.rs:197`) and appended to the tar as
  `<dir>/<sampler>/<seq>.parquet`, then the builder resets. `last_key`
  survives the reset so dedup works across the seal boundary. Empty builders
  are never sealed.
- **Seal thresholds are byte-first**: estimated segment bytes primary, with a
  row cap and an age bound (compile-time constants to start; order of a few
  MiB / a few thousand rows / ~5 min, finalized with measurement during
  implementation). Rows alone are a poor size proxy — histogram cells are
  KBs, so 512 rows of `syscall_latency` and 512 rows of a counter-only
  sampler differ by orders of magnitude in memory and encode cost.
- **The age bound exists for the kill-loss window, not finalize cost.** The
  byte/row caps alone bound finalize and memory (a slow sampler's open
  segment is naturally tiny — drivehealth accrues ~5 rows in 5 min). Age
  sealing only bounds how much data an unclean kill loses, and it drives
  segment count: ~5 min seals on a 25-sampler agent over 24 h is ~7,000 tar
  entries and a ~288-way read-time merge per slow table. The trade (loss
  window vs segment count / merge cost) is deliberate; to keep checkpoint
  overhead flat, all due builders are sealed together and followed by **one**
  shared `manifest.json` checkpoint entry (a few KB), then `fdatasync`.
  SIGKILL leaves page cache intact, but the docker story often ends in host
  shutdown — the sync at seal cadence turns "readable after kill" into
  "readable after power loss up to the last synced seal."

### Write pipeline threading

Sealing must not run on the scrape loop (a single-worker tokio runtime,
`mod.rs:518-522`): a large parquet encode there delays the next tick and
skews the sampling cadence. So the write side is a **dedicated writer thread**
(plain `std::thread` — the work is blocking CPU + file IO):

- The writer thread owns the `tar::Builder` (opened on the output path at
  recording start), the segment sequence numbers, the running per-table
  totals (`rows`, first/last timestamps for `cadence_ns`), and the manifest
  checkpoint state.
- The scrape loop does only ingest + builder rotation (cheap), and hands
  sealed builders over a **bounded channel** as seal jobs. Tar appends are
  inherently serialized, so one thread doing encode+append is the simplest
  correct shape; channel FIFO preserves per-sampler segment order.
- A full channel blocks the loop — that is the intended backpressure signal
  (if the disk can't keep up, the recording is doomed anyway), and it bounds
  memory to open segments + queue depth.
- Writer-thread failure (e.g. disk full) surfaces on the next hand-off: the
  send fails, the loop reports the writer's error and aborts the recording,
  instead of logging per-tick against a corrupt archive.
- Finalize is a handshake: the loop sends a finalize message (final partial
  builders + the `complete` marker), the writer seals tails, appends the
  final manifest and tar footer, syncs, and the loop joins the thread.

### Finalize

Seal the current partial segments (small by construction), append the final
`manifest.json` — the only one carrying `complete: true` — write the tar
footer, sync. No re-encoding of sealed segments. Cost is bounded by the seal
thresholds.

### Manifest (`REZ_SCHEMA_VERSION` 1 → 2)

`RezTableIndex.file: String` (`rez.rs:36`) becomes `files: Vec<String>` in
segment order; `rows` and `cadence_ns` become totals across segments. Serde
keeps v1 readable: `file` becomes `Option<String>`, `files` gets
`#[serde(default)]`, and readers treat a v1 `file` as a one-element segment
list. `parquet combine`/`filter`/`annotate` continue to emit single-file
tables, now under `files`.

`RezManifest` gains **`complete: bool`** (`#[serde(default)]` — absent means
false). Checkpoint manifests never set it; only finalize writes
`complete: true`. Without it, a checkpoint and a final manifest are
indistinguishable, and tools would silently present a killed recording as a
complete one. Readers/tools surface "recording was not cleanly finalized;
data after \<last row timestamp\> may be missing" when it's absent. v1
archives (no field) predate unclean-kill recovery, so absent-on-v1 is not
flagged.

### Forward compatibility (v1 binaries reading v2 archives)

Considered and **rejected as structurally impossible without giving up the
goal**: the v1 `RezTableIndex` requires exactly one `file` per table, and any
value we could write there is wrong —

- a merged single file per table reintroduces O(recording) finalize;
- pointing `file` at one segment makes a v1 binary silently read partial
  data;
- appending every segment under the same `<sampler>.parquet` name exploits
  the v1 reader's last-entry-wins into the same silent-partial trap.

An error beats silent partial data, so a v1 binary reading a fresh v2
archive fails with its serde error (`missing field 'file'`). Recorded here
so nobody "fixes" it into the silent-partial trap. The compatible path is
the **offline compactor** (Deferred): compaction merges each table to a
single file, so its output sets `file` alongside `files` and is fully
v1-readable. `.rez` is weeks old (first landed 2026-07-13), so the
v1-binary population is small.

### Reader (single change point: `read_archive_reader`)

All `.rez` consumers funnel through `read_archive_reader` and its
`RecordingBytes` output (`rez.rs:517`), which stays unchanged downstream:

- A multi-segment table is merged into one parquet byte blob at open: decode
  each segment's record batches, union the schemas (columns absent in earlier
  segments null-fill), concatenate, re-encode. Read-time cost is O(table) —
  the same order as reading is today, and read time is not
  teardown-constrained.
- Tar iteration becomes **truncation-tolerant**: on a truncated final entry,
  stop iterating and use the last complete manifest checkpoint. Sealed
  segments referenced by that manifest always precede it in the tar, so the
  prefix is self-consistent. A missing manifest or a manifest referencing an
  absent table file is still an error.

### Recorder loop (`src/recorder/mod.rs`)

- In `.rez` mode, don't create the `EndpointWriter` temp spool and don't
  re-serialize snapshots to msgpack — the double-write disappears.
- `RezRecorder` construction moves out of the `get_or_insert_with` closure
  (`mod.rs:782`): it now opens the output tar and spawns the writer thread,
  both of which can fail and need real error handling (report and abort, not
  panic).
- Mid-recording write errors surface through the writer-thread hand-off (see
  "Write pipeline threading") and fail the recording with a clear message.

## Alternatives considered

- **Incremental per-sampler parquet to temp files, tar-copy at finalize.**
  No format change, but the final tar copy is O(recording size) — gigabytes
  still cost seconds at teardown — and cross-segment schema growth forces
  either a finalize-time rewrite (unbounded again) or the same reader-side
  union this design needs anyway. Rejected: pays most of the cost for a
  weaker guarantee.
- **Keep the msgpack spool authoritative, convert concurrently in the
  background.** Keeps the double-write, adds concurrency, and still needs
  incremental parquet writers underneath. Rejected as a superset of the
  first alternative's problems.

## Testing plan

- Seal/roll at thresholds: segment sequence names and manifest `files` order.
- Window-advance dedup across a seal boundary (`last_key` persistence).
- Late-appearing column: null-padded within a segment; unioned with null-fill
  across segments at read.
- v1 archive back-compat read.
- Truncated-archive recovery: chop a tar mid-entry; reader returns the sealed
  prefix from the last checkpoint manifest.
- Round-trip: multi-segment recording queried through `RezReader` matches a
  single-segment equivalent.
- `complete` marker: finalize sets it, checkpoints don't; tools flag an
  archive recovered from a checkpoint as not cleanly finalized.
- Writer-thread error surfacing: a failing tar append (e.g. ENOSPC) aborts
  the recording with the writer's error, not per-tick log spam.
- Scrape cadence under sealing: no missed/late ticks attributable to a seal
  (measured during implementation, in the close-out).
- In-progress readability smoke test: a `.rez` still being recorded opens via
  the truncation-tolerant reader up to its last checkpoint (hindsight-flavored
  bonus of the design, not a supported feature yet).
- Existing `write_archive`/`write_archive_bytes` users (combine, filter,
  annotate) keep passing.

## Deferred

- **Offline `.rez` compactor.** Segmented archives trade size and open-time
  merge cost for durability — accepted deliberately (per-segment dictionaries
  and footers cost some compression ratio; the number gets measured at
  implementation). A compaction tool (likely under `rezolus parquet`) merges
  each table's segments into a single file offline, recovering the size and
  open-speed of a single-segment archive — and its output is **fully
  v1-readable** (sets `file` alongside `files`), making it the compatibility
  downgrade path too. Not needed for the streaming writer to ship.
- **Seal thresholds as compile-time constants.** Reopen if real workloads
  need tuning (a `--flag` or config knob).
- **Fast finalize for the classic parquet path.** By design out of scope
  (whole-file schema only knowable at end). Reopen if a client needs `.parquet`
  output with fast stop — the likely shape is record to `.rez`, convert
  offline (see the compactor above).
