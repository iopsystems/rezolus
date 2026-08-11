# Streaming segmented `.rez` writer — bounded-time finalization

- **Opened:** 2026-08-11
- **Status:** OPEN — design landed pre-build.
- **Arc:** continuation of the `.rez` container work in
  [per-sampler `.rez` archive](2026-07-13-per-sampler-rez-archive.md) and
  [`.rez` reader ecosystem](2026-07-15-rez-reader-ecosystem.md).
- **Owner:** Brian Martin
- **Repos:** rezolus (`src/recorder/rez.rs`, `src/recorder/mod.rs`,
  `src/rez_reader.rs`) and metriken (`~/workspace/metriken`,
  metriken-query: the segment-aware source — in scope, see Reader).

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
- Memory bounded by one open segment per sampler plus a seal queue of depth
  1–2 (each slot holds a whole segment, so depth is itself a memory bound).
- No missed or late scrape ticks attributable to sealing **while the disk
  keeps up** — parquet encode and tar IO run off the scrape thread (see
  "Write pipeline threading"). Under a disk bottleneck, backpressure blocks
  the loop by design; the SIGTERM→exit bound is then: one in-flight send +
  (queue depth + 1) segment writes + tail seals + syncs (+ 2 s child grace
  in wrapped mode, `child::TERM_GRACE`). Queue depth 1–2 keeps that inside
  the docker window; close-out must measure it.
- The msgpack spool double-write in `.rez` mode is eliminated.
- A recording killed without finalize (SIGKILL, crash) leaves a
  `<output>.partial` readable up to its last checkpoint; after power loss,
  up to the last **synced** checkpoint (two-sync protocol below). Readers
  can distinguish a cleanly finalized recording from a recovered one
  (per-recording `complete` marker).
- **No read-path regression.** The read path is heavily optimized
  (footer-only open, lazy row-group decode against the `BufferPool`) and
  stays that way: opening a segmented archive performs no row-group decode,
  and a query decodes the same row groups a single-segment equivalent
  would. Close-out carries measured open + query timings, segmented vs
  compacted.
- Row timestamps are strictly monotonic within a recording (anchored
  monotonic stamping — see "Recorder loop"), so a recorder-side NTP step
  can never bake non-monotonic time into an immutable sealed segment.
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
- **Seal thresholds are byte-first, via incremental in-memory accounting.**
  There is no cheap estimator of *serialized* parquet size — a dry-run
  encode is exactly the cost this design moves off the scrape thread, and
  static guesses are off by 10–100× for histograms. So the threshold is
  accumulated in-memory bytes, maintained O(1) per entry in `push_row`
  (`rez.rs:680`): ~16 B per scalar cell + window; `total_buckets() × 8` per
  histogram cell (`h.value.clone()` at `rez.rs:708` clones the bucket
  `Box<[u64]>`; gp=7/mvp=64 ≈ 58 KB per cell). That is exact for the two
  things the cap must bound — memory footprint and encode input — and the
  compile-time constant is calibrated to a target encoded size once the
  in-memory→encoded ratio is measured. Row cap and age bound are secondary.
- **The age bound exists for the kill-loss window, not finalize cost.** The
  byte/row caps alone bound finalize and memory (a slow sampler's open
  segment is naturally tiny — drivehealth accrues ~5 rows in 5 min). Age
  sealing only bounds how much data an unclean kill loses, and it drives
  segment count: ~5 min seals on a 25-sampler agent over 24 h is ~7,000 tar
  entries and a ~288-way read-time merge per slow table. The trade (loss
  window vs segment count / merge cost) is deliberate. To keep checkpoint
  overhead flat, all due builders are sealed together and followed by
  **one** shared `manifest.json` checkpoint entry.
- **Seal checks are tick-driven, not ingest-driven.** They run every loop
  iteration whether or not a scrape succeeded, so an unreachable endpoint
  still gets its pre-outage rows sealed and the kill-loss window stays
  bounded in time. This makes tick cadence load-bearing for durability, so
  scrapes and endpoint probes get a `tokio::time::timeout` (~one interval):
  today `scrape_one` (`mod.rs:262-276`) and the probe path have no timeout
  and a *hung* (not failing) endpoint parks the loop for TCP-timeout scales
  — stalling both age seals and SIGTERM response.
- **Checkpoint durability is a two-sync protocol**: `fdatasync` after the
  seal batch, then append the checkpoint manifest, then `fdatasync` again.
  A single post-manifest sync is not enough — write order is not persistence
  order: power loss can persist the one-page manifest while an earlier
  segment's data blocks are still unwritten, leaving the recovery manifest
  pointing at garbage and failing the whole open instead of falling back.
  With the two-sync order, any persisted manifest byte implies durable
  segments (a partially-persisted manifest fails JSON parse and recovery
  falls back to the previous checkpoint). Also: one fsync of the parent
  directory after creating the output file (else the dirent itself can be
  lost), and the tar `File` stays unbuffered — no `BufWriter` (the `tar`
  crate writes straight through). SIGKILL leaves page cache intact, but the
  docker story often ends in host shutdown — the synced checkpoint cadence
  turns "readable after kill" into "readable after power loss up to the
  last synced checkpoint."
- **Checkpoint manifests omit the per-table `columns` list** (final manifest
  only). `columns` is O(total column count) — cgroup-heavy tables reach
  thousands of columns, making a full checkpoint plausibly 100 KB+ rather
  than "a few KB" — and recovery doesn't need it to load segments. A
  checkpoint's `rows`/`cadence_ns` describe **exactly the segments it
  references** — sealed rows only, never open builders — so a recovered
  archive's manifest never over-reports recoverable data. Checkpoints also
  carry `clock_offset_ns`, the observed wall-vs-anchor offset at checkpoint
  time (see "Recorder loop": monotonic row stamps).
- **An empty checkpoint manifest is the first tar entry**, written at
  recording start (version, the recording's labels/metadata, zero tables,
  no `complete`). Without it, nothing identifies the file as `.rez` until
  the first seal batch: `is_rez_reader` (`rez.rs:574`) would scan MBs of
  segment data hunting for a manifest, and an early-killed or in-progress
  file would sniff as not-`.rez` and be misrouted to the parquet path by
  every dispatcher (viewer, MCP, all four parquet_tools). With it,
  detection stays O(first entry) and "readable up to the last synced
  checkpoint" holds from t=0.
- **Output goes to `<output>.partial`, renamed on clean finalize.** Writing
  the output path directly would `File::create`-truncate any pre-existing
  file at t=0 (today nothing is written until finalize) and leave stubs on
  failed starts. The `.partial` is created exclusively (a concurrent writer
  is a clear error; a leftover `.partial` from a previous crash is renamed
  aside with a warning — it may hold recoverable data — never clobbered);
  rename on success preserves today's UX (the output appears only when
  complete), and an unclean kill leaves a self-describingly incomplete
  `.partial` as the recovery artifact. A recording that ends with no data
  aborts the writer and unlinks the `.partial`.

### Write pipeline threading

Sealing must not run on the scrape loop (a single-worker tokio runtime,
`mod.rs:518-522`): a large parquet encode there delays the next tick and
skews the sampling cadence. So the write side is a **dedicated writer thread**
(plain `std::thread` — the work is blocking CPU + file IO):

- The writer thread owns the `tar::Builder` (opened on the output path at
  recording start), the segment sequence numbers, the running per-table
  totals (`rows`, first/last timestamps for `cadence_ns`), and the manifest
  checkpoint state.
- The scrape loop does only ingest + builder rotation (cheap; rotation is
  `mem::replace`, copying `last_key` and the byte accumulator baseline into
  the fresh builder), and hands sealed builders over a **bounded channel of
  depth 1–2** as seal jobs. Tar appends are inherently serialized, so one
  thread doing encode+append is the simplest correct shape; channel FIFO
  preserves per-sampler segment order.
- A full channel blocks the loop — that is the intended backpressure signal
  (if the disk can't keep up, the recording is doomed anyway), and it bounds
  memory to open segments + queue depth. The cost is honest and recorded in
  the GO criteria: while blocked, `STATE` is only re-checked at the loop top,
  so a SIGTERM during backpressure waits out the in-flight writes — the
  small queue depth is what keeps that bound inside the grace window.
- Writer-thread failure (e.g. disk full) surfaces on the next hand-off: the
  send fails (receiver dropped), the loop then **joins the thread** (which
  returns immediately once the writer has exited) and reports the writer's
  stored error, instead of logging per-tick against a corrupt archive.
  Send-failure → join → report is the required order, and the writer always
  exits its receive loop on the first error rather than continuing.
- The writer is **panic-free by construction** — every fallible operation
  returns `Err`. This is a hard requirement, not style: the global panic
  hook (`main.rs:57-62`) prints and calls `process::exit(101)` *before*
  unwinding, so a writer panic never reaches the send-error path, skips
  finalize (the archive recovers at the last checkpoint), and in wrapped
  mode orphans the child (its process group is never terminated). `Err` is
  the only supported failure path; that is the accepted panic contract.
- The writer thread is **joined on every path out of the async block** —
  including the no-data early returns (`mod.rs:908-915`, which today
  `return`/`exit` before rez finalization) and the wrapped-mode `Outcome`
  path whose `std::process::exit` (`mod.rs:1106-1111`) skips destructors.
  A tar left without its terminating zero-blocks by `exit(2)`/`exit(101)`
  is expected, not corruption: the reader treats a missing footer as
  end-of-archive.
- Finalize is a handshake: the loop sends a finalize message (final partial
  builders + the `complete` marker), the writer seals tails, appends the
  final manifest and tar footer, syncs, and the loop joins the thread and
  renames the `.partial` into place.

### Finalize

Seal the current partial segments (small by construction), append the final
`manifest.json` — the only one carrying `complete: true` — write the tar
footer, sync, rename `<output>.partial` to the output path. No re-encoding
of sealed segments. Cost is bounded by the seal thresholds.

### Manifest (`REZ_SCHEMA_VERSION` 1 → 2)

`RezTableIndex.file: String` (`rez.rs:36`) becomes `files: Vec<String>` in
segment order; `rows` and `cadence_ns` become totals across segments. Serde
keeps v1 readable: `file` becomes `Option<String>` with
`#[serde(skip_serializing_if = "Option::is_none")]` (never serialize
`"file": null`), `files` gets `#[serde(default)]`, and readers treat a v1
`file` as a one-element segment list.

**`write_archive_bytes` owns index canonicalization**: it rewrites each
table's index entry to name exactly the files it emits — `files` in
emission order, `file: Some(..)` iff the table is single-file. This is
load-bearing, not cosmetic — `combine` (`combine.rs:301-311`), `filter`
(`filter.rs:147-159`), and `annotate` (`annotate.rs:257-271`) all carry the
input's `RezTableIndex` verbatim; a stale index referencing files that
don't exist in the output breaks every downstream reader. The tools
themselves stay segment-oblivious: `read_archive_reader` hands per-segment
bytes through untouched and the tools pass them along byte-identical
(filter drops whole tables, annotate rewrites only manifest metadata,
combine re-dirs recordings), so tool output preserves segmentation — only
the compactor merges. (Tool output carries a single final manifest;
checkpoint history is not copied through — accepted, now documented.)

**`complete: bool` lives on `RezRecording`, not `RezManifest`**
(`#[serde(default)]` — absent means false). A top-level bool cannot survive
`combine`, which merges recordings from different archives (one recovered +
one clean has no truthful top-level value); per-recording, the flag
propagates verbatim through combine/filter/annotate with zero code, and the
viewer can surface it per-arm in an A/B. Checkpoint manifests never set it;
finalize sets it on its recording, and the atomic whole-archive writers
(`write_archive`/`write_archive_bytes` callers producing complete data)
set it on theirs. Readers/tools surface "recording was not cleanly
finalized; data after \<last row timestamp\> may be missing" when absent.
v1 archives predate unclean-kill recovery, so the flag is only interpreted
when `version >= 2` — and v2 readers gain a **forward version gate** (error
clearly on `version >` supported): v1 shipped without one, which happens to
make compacted-output compat possible below, but gates cannot be
retrofitted, so v2 adds it now.

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
single file, so its output sets `file` alongside `files` — and it emits
`version: 1`, because its output *is* v1-shaped (the extra
`files`/`complete` fields are ignored by v1's serde — verified: no
`deny_unknown_fields` on the derives, and no v1 reader checks
`manifest.version`). Emitting `version: 1` makes the claim robust instead
of resting on v1's *absence* of a version gate. One accepted edge: a v1
binary reading compacted output of a *recovered* archive cannot see the
per-recording `complete: false` and presents it as whole — the compactor
preserves the flag for v2 readers and warns when compacting an incomplete
archive. `.rez` is weeks old (first landed 2026-07-13), so the v1-binary
population is small.

### Reader (single change point: `read_archive_reader`)

All `.rez` consumers funnel through `read_archive_reader` and its
`RecordingBytes` output (`rez.rs:517`), which stays unchanged downstream:

- **Reading is segment-aware, in metriken-query — merge-at-read is rejected
  outright, eager or lazy.** The read path is heavily optimized (footer-only
  open via `ArrowReaderMetadata::load`; row groups decode per query against
  the shared `BufferPool` budget) and a regression there is not acceptable.
  The design's original claim ("merge cost O(table), same order as reading
  today") was wrong and is retracted: any decode + re-encode merge costs
  transient multi-GB RSS and tens of seconds for exactly the multi-hour
  recordings that motivate this design, at every viewer open and every
  one-shot MCP CLI invocation. Instead, a **segment-aware source lands in
  metriken-query as part of this effort**: it opens each segment footer-only
  (KBs per footer), unions schema and metadata across segments (applying the
  column-identity policy below), and serves queries by decoding only the row
  groups the query touches, in segment order, splicing one timeline per
  series. Today's `MultiParquetSource` is explicitly not this — it
  duplicates same-identity series across files rather than splicing
  (parquet.rs:586-588); the new source subsumes that gap.
  `read_archive_reader` hands per-segment bytes through untouched;
  `RezReader` opens one segmented source per sampler table.
- **Column identity in the segmented source's schema union is `name +
  metric_type` (+ histogram
  `grouping_power`/`max_value_power`); conflicts split into distinct
  columns — never a hard error, never silent coercion.** Column names are
  the snapshot's numeric-id names (`rez.rs:70-71`), and an agent restart
  mid-recording (the recorder reconnects via the Pending→Active path)
  remaps ids arbitrarily. Naive schema union then: hard-fails the whole
  archive on a counter→gauge flip (`UInt64` vs `Int64` field merge), or
  *silently succeeds* on a counter→histogram flip (different physical
  column names sharing one `:window_*` pair) or on drifted `metric_type`
  metadata (the reader would decode a gauge as a counter — the value shape
  keys on that metadata). On conflict, the later run becomes a distinct
  column with disambiguated identity and its own windows, with a warning;
  non-identity metadata drift is last-writer-wins. Related latent bug to
  fix in the same change: `push_row`'s type-mismatch arm (`rez.rs:705-711`)
  skips the value but still pushes the window, desyncing a column's
  values/windows vectors.
- **The source still neither sorts nor dedups; v2 rows are monotonic by
  construction, v1 rows are not.** `last_key` guarantees strictly
  increasing *keys*, not timestamps. The clocks, precisely: v2 row
  timestamps are the recorder's **monotonic-anchored hybrid** (wall anchor
  at recording start + monotonic elapsed — see "Recorder loop"), so within
  a v2 recording timestamps are strictly increasing; v1 archives stamped
  raw `SystemTime::now()` per tick and carry no such guarantee. Dedup keys
  are agent-side window ends — wall-anchored with monotonic width
  (`src/agent/timing.rs`: an NTP step *during* a read cannot corrupt a
  window, but successive window anchors move with the agent's system
  clock). Keys and timestamps are therefore different hosts' clocks: an
  agent-side step-back regresses the key → rows silently dropped until
  windows pass the old key (pre-existing ingest behavior,
  `rez.rs:803-807`) — a gap, not corrupt math. The source concatenates in
  segment order and leaves timestamp semantics exactly as a single-segment
  table would have them.
- Tar iteration becomes **truncation-tolerant**, with the rule stated
  precisely because the `tar` crate does not error where it matters: on
  mid-data truncation the final entry is yielded `Ok` and `read_to_end`
  returns a *short buffer silently* (the error only surfaces skipping to
  the next entry); truncation at a block boundary ends iteration cleanly,
  indistinguishable from a missing footer. So: an entry counts only if
  bytes read == header size; any tar error, short read, or unparseable
  tail manifest ends iteration; recovery uses the last fully-read,
  parseable manifest. Segments referenced by a checkpoint always precede
  it in the tar and (two-sync protocol) are durable whenever the
  checkpoint is. A missing footer is expected on unclean exit. Accepted
  limitation: mid-archive corruption (bad checksum block) is
  indistinguishable from truncation — the archive presents as unclean up
  to the last good checkpoint, under-reporting but never fabricating data.
  A manifest referencing an absent table file is still an error.

### Recorder loop (`src/recorder/mod.rs`)

- In `.rez` mode, don't create the `EndpointWriter` temp spool and don't
  re-serialize snapshots to msgpack — the double-write disappears.
- **Row timestamps become monotonic by construction.** Today `snapshot_ts`
  is `SystemTime::now()` per tick (`mod.rs:728-731`) — a recorder-side NTP
  step-back produces duplicate/decreasing row timestamps, which feed
  `rate()`'s dt ≤ 0, and which streaming makes *permanent* (sealed segments
  are immutable; the old in-memory finalize could at least have sorted).
  The recorder now anchors once at recording start (wall `t0` + monotonic
  `t0`) and stamps rows `t0_wall + monotonic elapsed` — the same hybrid the
  agent already uses for window widths (`src/agent/timing.rs`), and
  consistent with tick *scheduling*, which is already monotonic
  (`aligned_interval`): today we schedule monotonically but stamp wall,
  which is the inconsistency. Rows are strictly increasing within a
  recording, immune to recorder-side steps. Residuals, accepted and
  recorded: absolute wall accuracy drifts with clock-rate error over long
  recordings (order seconds/day worst case), so each checkpoint manifest
  records the observed wall-vs-anchor offset (`clock_offset_ns` — one
  field, updated per checkpoint) making drift and steps visible metadata
  instead of silent data corruption; agent-side window anchors remain the
  agent's system clock (cross-host skew lives in the window offset columns,
  as today), and an agent-side step-back still regresses dedup keys (rows
  dropped — a gap, not corrupt math; pre-existing ingest behavior).
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
- v1 archive back-compat read; v2 forward gate errors clearly on
  `version > 2`; compacted output opens under a v1-era reader shape
  (`file` set, `version: 1`).
- Truncated-archive recovery, one test per failure geometry: chop mid-data
  (tar yields the short entry `Ok` — the length check must catch it),
  mid-header (tar errors), at a block boundary (clean EOF, looks like a
  missing footer), and mid-checkpoint-manifest (parse fails → previous
  checkpoint used). Each recovers the sealed prefix.
- Segmented-source conflict policy: counter→gauge flip, counter→histogram
  flip, and `grouping_power`/`max_value_power` drift across segments each
  open successfully as split series with a warning — never a hard error,
  never a misdecoded series.
- Read-path parity: opening a segmented archive performs no row-group
  decode (footer-only per segment, asserted); a query decodes the same row
  groups as the single-segment equivalent and returns identical results;
  open + query timings measured segmented vs compacted at close-out.
- `write_archive_bytes` canonicalization: segmented v2 input through
  combine/filter/annotate yields manifests whose entries name exactly the
  files present (segments pass through byte-identical; `file` set iff
  single-file); per-recording `complete` propagates verbatim.
- `push_row` values/windows desync (`rez.rs:705-711` mismatch arm) fixed
  with a regression test.
- Monotonic stamping: row timestamps derive from the recording anchor +
  monotonic elapsed (derivation unit-tested; strictly increasing across
  seal boundaries); checkpoint manifests carry `clock_offset_ns`.
- Initial manifest entry: a just-started recording sniffs as `.rez`
  (`is_rez_path`) and opens as a valid empty unclean recording; a
  pre-first-seal kill recovers the same way.
- No-data path: recording that captures nothing joins the writer and
  unlinks the `.partial`; a pre-existing output file is untouched.
- Round-trip: multi-segment recording queried through `RezReader` matches a
  single-segment equivalent.
- `complete` marker: finalize sets it (per recording), checkpoints don't;
  tools flag an archive recovered from a checkpoint as not cleanly
  finalized; combine of one clean + one recovered archive preserves each
  recording's flag.
- Writer-thread error surfacing: a failing tar append (e.g. ENOSPC) aborts
  the recording with the writer's error, not per-tick log spam.
- Scrape cadence under sealing: no missed/late ticks attributable to a seal
  (measured during implementation, in the close-out).
- In-progress readability smoke test: a `.rez` still being recorded opens via
  the truncation-tolerant reader up to its last checkpoint (hindsight-flavored
  bonus of the design, not a supported feature yet).
- Existing `write_archive`/`write_archive_bytes` users (combine, filter,
  annotate) keep passing.

## Adversarial design review (2026-08-11, pre-build)

Three parallel adversarial subagents attacked the design against the actual
code and vendored crates (tar-0.4.46, histogram-1.5.0, metriken-core-0.3.0,
metriken-query), one per seam: tar container & crash recovery, threading &
process lifecycle, reader merge & ecosystem compat. Every finding checked
out; all were folded into the sections above. The load-bearing ones:

- Two claims were **wrong** and are corrected above: merge cost is *not*
  "the same order as reading today" (today's open is footer-only lazy —
  resolved by pulling a segment-aware metriken-query source **into scope**
  and rejecting merge-at-read outright; a read-path regression is not
  acceptable), and segments do *not* have "disjoint increasing timestamps"
  (`last_key` orders keys, not timestamps — two hosts' system clocks).
- Single-sync checkpoints had a real power-loss ordering hole → two-sync
  protocol; plus parent-dir fsync and the no-`BufWriter` rule.
- The `tar` crate silently yields short entries on mid-data truncation →
  the precise length-checked tolerance rule.
- No manifest existed before the first seal batch → initial empty manifest
  as the first tar entry (found independently by two reviewers; also fixes
  `is_rez_reader` detection cost).
- Direct output writing truncated pre-existing files at t=0 →
  `.partial` + rename.
- Top-level `complete` could not survive `combine` → per-recording;
  un-canonicalized indexes broke tool output on segmented input →
  `write_archive_bytes` owns canonicalization.
- Numeric-id column names remap on agent restart → the column-identity
  merge policy; plus the latent `push_row` values/windows desync.
- "Estimated segment bytes" was unimplementable as written → incremental
  in-memory accounting in `push_row`.
- Panic-hook `exit(101)` bypasses the send-error path → panic-free-by-
  construction writer contract; no-data early returns skipped the join →
  join-on-every-path rule.
- Age seals had to be tick-driven, and hung endpoints (no scrape timeout)
  could stall seals *and* SIGTERM response → tick-driven checks + scrape/
  probe timeouts.

Verified safe (recorded so it isn't re-litigated): duplicate tar names are
mechanically last-wins in our reader, GNU/bsdtar, and Python `tarfile`;
sealed-builder hand-off types are all plain owned data (`Send + 'static`);
`filter --samplers` matches `idx.sampler`, not filenames; the read path is
entry-order-agnostic; in-progress reads are sequential (no mmap/seek-back),
so a racing appender can only look like a longer prefix or truncated tail.

Accepted risks, explicit: backpressure under a disk bottleneck can still
consume the grace window (bound recorded in GO criteria; queue depth 1–2);
mid-archive corruption presents as truncation (under-reports, never
fabricates); a v1 binary reading compacted output of a recovered archive
cannot see `complete: false`.

## Deferred

- **Full metric-identity column keys.** Column names are per-agent-process
  numeric ids; the merge policy handles restarts by splitting columns, but
  keying columns on metric name + labels at *write* time would make
  restarts seamless. Write-format change. *Reopen:* if agent-restart-heavy
  recordings (fleet rollouts) make split columns a real annoyance.
- **Offline `.rez` compactor.** Segmented archives trade some size for
  durability — accepted deliberately (per-segment dictionaries and footers
  cost compression ratio; the number gets measured at implementation). A
  compaction tool (likely under `rezolus parquet`) merges each table's
  segments into a single file offline, recovering the size and per-segment
  footer overhead — and its output is **fully v1-readable** (sets `file`
  alongside `files`, emits `version: 1`), making it the compatibility
  downgrade path too. With the segment-aware read path in scope, compaction
  is an optimization, not a read-speed requirement. Not needed for the
  streaming writer to ship.
- **Seal thresholds as compile-time constants.** Reopen if real workloads
  need tuning (a `--flag` or config knob).
- **Fast finalize for the classic parquet path.** By design out of scope
  (whole-file schema only knowable at end). Reopen if a client needs `.parquet`
  output with fast stop — the likely shape is record to `.rez`, convert
  offline (see the compactor above).
