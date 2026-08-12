# Streaming segmented `.rez` writer — bounded-time finalization

- **Opened:** 2026-08-11
- **Status:** **IMPLEMENTED & MEASURED**, macOS 2026-08-12 and **Linux fleet
  2026-08-12**. Finalize is bounded by the **open segments, not recording
  length**: on a 26-table Linux BPF agent (Debian 13, kernel 6.12, 278 KB
  snapshots) a **30× longer recording — 28× more bytes, 13× more segments —
  costs 1.25× more finalize**, at **258.7–404.3 ms, median 303.7 ms**. That is
  ~10.4× the 3-sampler macOS figure (19.6–37.1 ms, median 29.2 ms) and still
  ~33× inside a 10 s container grace window. Kill-safe: SIGKILL leaves a
  `.partial` that opens, self-reports "not cleanly finalized", and answers
  PromQL. Sealing does not perturb the scrape loop (seal-boundary vs interior
  delta medians differ by 0.15 %; 0 skipped ticks at an attainable interval).
  Under genuine backpressure SIGTERM→exit was 55 ms; at `--interval 30s` it is
  ~3 ms on Linux. The Linux release build (eBPF via libbpf-cargo + clang 19)
  succeeds — previously untested outside CI.
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
  append to the recording's `clock_offsets` observation series (see
  "Recorder loop": monotonic row stamps, one anchor + many observations).
- **An empty checkpoint manifest is the first tar entry**, written at
  recording start (version, the recording's labels/metadata,
  `clock_anchor_wall_ns`, zero tables, no `complete`). Without it, nothing identifies the file as `.rez` until
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
  recording, immune to recorder-side steps.
- **The anchor is recorded in the archive: one defining anchor, both clocks
  captured per tick, never re-anchored.** The recording's manifest entry
  carries `clock_anchor_wall_ns` (the `SystemTime` reading at recording
  start — present from the initial manifest entry onward); all row
  timestamps are `anchor + monotonic elapsed` by definition. Re-anchoring
  mid-recording is explicitly rejected — it would reintroduce the steps
  this exists to eliminate. Instead, **each tick captures both clocks**
  (the loop already calls `SystemTime::now()` per tick today, so this is
  free) and every row also stores the raw wall reading as a
  **`:wall_offset` sidecar column** (i64: wall − anchored at that tick;
  near-constant, delta-encodes to ~nothing). The monotonic-anchored
  timestamp is the timeline; the wall observation is data *about* the
  clock, per row — an NTP step locates to the exact tick, and post-hoc
  correlation against external logs has row precision. The query engine
  skips `:wall_offset` the same way it skips the `:window_*` sidecars
  (metriken-query change, already in scope with the segmented source). As
  an at-a-glance summary that needs no table decode, each checkpoint also
  appends one `(anchored_ts, wall_minus_anchored_ns)` observation to a
  per-recording `clock_offsets` series in the manifest (~16 B per
  checkpoint — derivable from the columns, kept for `parquet metadata`
  display). Residuals, accepted and recorded: absolute wall accuracy of
  the *timeline* drifts with clock-rate error (order seconds/day worst
  case) — but the per-row observations make drift and steps fully
  reconstructible rather than silently corrupting; agent-side window
  anchors remain the agent's system clock (cross-host skew lives in the
  window offset columns, as today), and an agent-side step-back still
  regresses dedup keys (rows dropped — a gap, not corrupt math;
  pre-existing ingest behavior).
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
  seal boundaries); every row carries a `:wall_offset` observation (the
  query engine skips the sidecar; a simulated wall step shows up in the
  offsets, not the timeline); `clock_anchor_wall_ns` present from the
  initial manifest; the `clock_offsets` summary series grows per
  checkpoint and survives into the final manifest.
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

## Outcome — measured (2026-08-12)

Live-agent measurements, release build, macOS 3-sampler agent, `--interval
10ms`. Raw logs/scripts were kept out of tree; every figure below is from a
recorded run, none estimated.

**Finalize is independent of recording length** (the central claim). Two
clocks per run — external signal→exit, and the recorder's own
`finalizing recording…` → `wrote .rez archive` pair:

| recorded | in-process finalize (3 runs, ms) | median |
|---|---|---|
| ~30 s | 25.59, 27.81, 34.77 | 27.81 |
| ~300 s | 29.18, 33.74, 19.60 | 29.18 |
| ~900 s | 22.61, 32.12, 37.13 | 32.12 |

All 9: min 19.60 / max 37.13 / median 29.18 ms. The shortest finalize in the
set is a 300 s recording and the longest a 900 s one — no trend. The 900 s
archive held 22 sealed segments per table and 23 manifest checkpoints,
confirming finalize re-encodes nothing. (An earlier D-phase run measured
148 ms at `--interval 1s`; that is the same claim at a different interval —
signal→exit = the wait for the next loop-top `STATE` check, ≤ one interval,
plus the finalize work.)

**Kill-loss window.** `kill -9` at t=200 s: 16,384 rows durable across 4
segments, 35.34 s of data lost against the 40.96 s the seal policy
structurally implies at that rate. Note the practical rule: at high row rates
the loss window is set by `max_rows` (41 s at 10 ms), not by `max_age`.

**In-progress readability** holds from t=0: at 5 s (before the first seal) the
`.partial` opens, sniffs as `.rez`, reports "not cleanly finalized", and shows
the clock anchor with zero tables — the initial-empty-manifest guarantee. At
100 s it reported 8,192 rows / 2 segments; a mid-recording `.partial` also
answered `mcp query` with 123 points.

**Cadence is untouched by sealing.** Over 89,961 intervals with 21
mid-recording seals: **0 skipped ticks**, mean delta exactly 10.000 ms,
strictly monotonic, and the deltas spanning seal boundaries (max 12.179 ms)
are indistinguishable from the overall distribution (max 12.653 ms).
Degradation is graceful and only under pathological policies: at 10 seals/s,
0.37 % of ticks slip; at 100 seals/s (18 GB in 180 s), 14.8 %.

**Size cost of segmentation: +1.28 %** at the production default (6
segments/table), measured by replaying one captured 24,576-snapshot live
stream through two policies (identical input bytes). The cost is superlinear
in segment count — 24 segments +17.7 %, 96 segments +116 % — which quantifies
the loss-window trade: shrinking it is cheap to ~10 segments and expensive
beyond ~25.

**No read-path regression** (the other GO criterion): open is flat in segment
count (6.1 → 5.7 → 6.1 → 8.9 ms for 1/6/24/96 segments — footer-only, as
designed) and at production segmentation a `rate()` query costs +0.4 ms
(+5 %). Pathological segmentation does grow query cost (23.5 ms at 96).

**Backpressure, previously never exercised.** Driven into real backpressure
(~100 MB/s, ~200 fsyncs/s, 14.8 % of ticks blocked on the depth-1 channel),
SIGTERM→exit measured **55 ms** — ~180× inside the docker grace window. The
GO criteria required this bound to be measured rather than argued; it now is.

**ENOSPC** (20 MB disk image, pre-filled) produced exactly the designed
behavior: one error naming the sampler and the OS error, no per-tick spam, and
a readable `.partial`. It also exposed a real gap — `rezolus record` exited **0**
after that fatal failure, so a supervisor or docker healthcheck could not tell
a failed recording from a good one. Fixed in `1c21bb79`; the failure flag
outranks a wrapped command's own status.

## Outcome — Linux fleet scale (2026-08-12)

Re-measured on `hv01` (Debian 13, kernel 6.12, 64 cores) against a live
26-table BPF agent with 278 KB snapshots — the case the macOS figures could
not exercise. Binary built on the host from the branch (release build with
eBPF compilation succeeds; this was the first Linux build outside CI).

| recorded | finalize (3 runs, ms) | archive | segments (total / max table) |
|---|---|---|---|
| ~30 s | 289.8, 303.7, 309.7 | 22 MB | 33 / 5 |
| ~300 s | 270.7, 261.4, 258.7 | 203 MB | 157 / 49 |
| ~900 s | 367.6, 404.3, 378.3 | 605 MB | 432 / 144 |

All 9: **258.7 / 303.7 / 404.3 ms** (min/median/max) — a 1.56× spread across a
30× length range, and **non-monotonic** (the 300 s runs are the fastest). What
finalize flushes is the tails: 16.2 MB at 30 s, ~20.6 MB at 300 s, 25.6–27.7 MB
at 900 s, against a structural cap of 26 tables × 8 MiB = 208 MB. The only
strictly O(length) term is the manifest, which grows 74.5 → 112.9 KB over 30×.

**Three things differ materially from macOS and are worth stating plainly:**

1. **`--interval 10ms` is unattainable at fleet scale.** One scrape of this
   agent costs ~39 ms for 278 KB, so the loop runs scrape-bound at ~46–48 ms.
   Every macOS figure quoted "at 10 ms" is really "at 46 ms" here; cadence had
   to be re-tested at 100 ms to isolate sealing.
2. **Per-length finalize clusters do not overlap** (macOS's did). They are not
   monotonic in length, so the no-trend conclusion holds, but the honest claim
   is *bounded by the open segments, sub-linear in length* — not strictly
   independent of it.
3. **Kill recovery is per-table.** At `kill -9` 120 s into a run, only 10 of 26
   tables recovered anything; the other 16 had **zero sealed segments** because
   their seal period is 180 s (byte-capped, low volume) — longer than the run.
   Correct by policy and invisible on a 3-sampler agent, but it means the loss
   window for a quiet table can be the entire recording. **This is the
   strongest argument for the WAL follow-on:** a WAL covering the unsealed tail
   would have recovered all 26.

Surviving tables lost 4.03–27.32 s against a 195 s structural bound
(`max_rows` 4096 × 47.6 ms). Cadence at 900 s on the heaviest-sealing table
(144 segments): strictly monotonic, boundary/interior median ratio **1.0015**,
boundary max (83.5 ms) *below* interior max (92.4 ms). At an attainable 100 ms
interval: **0 skipped ticks in 1,201 intervals**. Read path on a 197 MB /
149-segment archive: `parquet metadata` 0.21 s, `mcp query` 0.71 s.

Heterogeneous cadence — the reason `.rez` exists — is visibly working: seal
periods within one archive range from **6.2 s** (`syscall_latency`, 144
segments) to 180 s (15 light tables) to 300 s (`drivehealth`, which samples at
56.5 s), a 29× spread driven entirely by the byte cap.

**Still unmeasured:** the size cost of segmentation at fleet scale. The macOS
figure (+1.28 % at the default policy) came from a bespoke replay harness and
**should not be quoted as the fleet number** — `syscall_latency` reached 144
segments in 900 s, well past the ~25 the macOS sweep called expensive, though
these are 8 MiB byte-capped segments rather than the small-segment pathology
that sweep explored.

## Post-landing findings (2026-08-12)

Two defects in the merged writer (`5de241d9`), both surfaced by the per-sampler
compression measurement done for the v3 container work. The evidence — segment
tables, ratios and the co-seal breakdown — lives in
[`2026-08-12-rez-sqlite-container.md`](2026-08-12-rez-sqlite-container.md),
"Per-sampler compression ratios"; it is not duplicated here. Both are fixed in
the PR that adds this section.

1. **`approx_bytes` undercounted scalar cells 2.5×.** `push_row` charged 16 B
   per scalar cell but also pushes an `Option<Window>` into `col.windows` that
   was never counted. Measured on the fleet host: `Option<u64>` is 16 B and
   `Option<Window>` is 24 B, so a scalar cell really costs **40 B — 2.50× the
   accounted 16 B**. Histogram cells were off by the same 24 B against ~58 KB
   of buckets (1.01×, immaterial). The consequence is that the byte cap is a
   *memory bound that was 2.5× wrong in the optimistic direction*: an 8 MiB
   capped scalar table held **~20 MiB** of builder RSS. Counting the window
   slot means the effective cap is now reached ~2.5× sooner for scalar-heavy
   tables, dropping the worst measured segment from **6.23 MiB to 2.49 MiB**
   encoded. That is the intended effect, and it is the precisely-targeted
   version of "lower the cap" — it shrinks only the poorly-compressing scalar
   tables and leaves the 13–62:1 histogram tables alone.

2. **Co-seal lockstep was the real tail-latency driver.** `maybe_seal` seals
   every due table as one batch, and every row-capped table advances exactly
   one row per tick from row 0 — so they reach `max_rows` in *permanent*
   lockstep. Measured: **12 tables sealing together every ~197 s as a single
   16.16 MiB batch at 85.9 ms p99** (≈1.9 ticks). **All 7 of 467 over-budget
   batches were co-seals**; not one was a large individual segment, and the
   worst single segment (`cpu_usage`, 6.23 MiB → 39.2 ms) fits with 15%
   headroom. Fixed by staggering only each sampler's *first* seal by a
   deterministic FNV-1a-of-the-sampler-name fraction (up to 50% of `max_rows`
   and `max_age`), then restoring the full policy. A phase offset, not a period
   change: the tables desync for the life of the recording at a cost of one
   short segment per sampler at startup, with steady-state segment size and
   count unchanged.

**`max_bytes` stays at 8 MiB.** Lowering it is expensive and aims at the wrong
target: halving to 4 MiB would take total segments from 582 to ~1,100 — and
`syscall_latency` from 190 to 380, deep into the superlinear region — merely to
shrink segments that are *already* 0.68 MB. It would also do nothing about the
lockstep, because every table involved in a co-seal is row-capped, not
byte-capped.

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

## Deferred (surfaced during implementation)

- ~~**Linux fleet re-measurement.**~~ **Done 2026-08-12** — see the fleet
  Outcome section above (~300 ms median, 26-table BPF agent).
- **Fleet-scale size cost of segmentation** — the only figure still macOS-only
  (+1.28 % at the default policy, from a bespoke replay harness). *Reopen:*
  before quoting a fleet size overhead, or if segment counts per table (144 in
  900 s for `syscall_latency`) start looking expensive.
- **Per-table kill-loss for low-volume tables.** A quiet table's seal period is
  180–300 s, so an unclean kill can lose its entire recording while busy tables
  lose seconds. Superseded by the WAL work if that lands; until then it is a
  documented property, not a bug.
- **WASM viewer has no `.rez` support at all** (`crates/viewer/` is
  parquet-only). Pre-existing, not a regression — but the static-site viewer
  silently cannot open any streamed recording. Exactly the divergence the
  `viewer-parity` skill exists to catch.
- **Recovered-archive state isn't surfaced to consumers.** `RezReader` warns and
  `parquet metadata` says "not cleanly finalized", but nothing carries that
  through the viewer API or MCP output, so a consumer can silently analyze a
  truncated recording.
- **Unbounded startup probe.** `probe_endpoint`/`fetch_agent_metadata` have no
  timeout (D2 bounded only the per-tick scrape/probe). A SIGSTOPed agent hangs
  `rezolus record` at startup and the first ctrl-c does not break out.
- **Manifest resolution is O(archive bytes), not O(footer)** — the authoritative
  manifest is the last tar entry, so `parquet metadata` on a pathological
  18 GB / 15k-segment archive took 19.3 s (sub-10 ms at production settings).
- **Cross-sampler queries still unsupported** (`route` errors when a query spans
  two samplers) — unchanged by this work, but now the next thing a real `.rez`
  user hits.

## Deferred (from the design)

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
