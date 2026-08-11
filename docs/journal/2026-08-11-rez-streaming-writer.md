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
- Memory bounded by one open segment per sampler.
- The msgpack spool double-write in `.rez` mode is eliminated.
- A recording killed without finalize (SIGKILL, crash) remains a readable
  archive up to its last sealed segment.
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

- On creation it opens a `tar::Builder` directly on the output path.
- Per-sampler state is today's `TableBuilder` — including the `last_key`
  window-advance dedup — but bounded. When a builder reaches a **seal
  threshold** (row count or age; compile-time constants to start, ~512 rows
  or ~5 min), the segment is sealed: encoded with the existing
  `write_table_parquet` (`rez.rs:197`) and appended to the tar as
  `<dir>/<sampler>/<seq>.parquet`, then the builder resets. `last_key`
  survives the reset so dedup works across the seal boundary.
- After every seal, append a refreshed `manifest.json` checkpoint entry
  (a few KB — negligible). This is what makes an unclean kill leave a valid
  archive.

### Finalize

Seal the current partial segments (small by construction), append the final
`manifest.json`, write the tar footer, sync. No re-encoding of sealed
segments. Cost is bounded by the seal thresholds.

### Manifest (`REZ_SCHEMA_VERSION` 1 → 2)

`RezTableIndex.file: String` (`rez.rs:36`) becomes `files: Vec<String>` in
segment order; `rows` and `cadence_ns` become totals across segments. Serde
keeps v1 readable: `file` becomes `Option<String>`, `files` gets
`#[serde(default)]`, and readers treat a v1 `file` as a one-element segment
list. `parquet combine`/`filter`/`annotate` continue to emit single-file
tables, now under `files`.

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
  (`mod.rs:782`): opening the output tar can fail and needs real error
  handling (report and abort, not panic).
- A tar append error mid-recording (e.g. disk full) fails the recording with
  a clear message instead of logging per-tick and producing a corrupt
  archive.

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
- Existing `write_archive`/`write_archive_bytes` users (combine, filter,
  annotate) keep passing.

## Deferred

- **Seal thresholds as compile-time constants.** Reopen if real workloads
  need tuning (a `--flag` or config knob).
- **Fast finalize for the classic parquet path.** By design out of scope
  (whole-file schema only knowable at end). Reopen if a client needs `.parquet`
  output with fast stop — the likely shape is record to `.rez`, convert
  offline.
