# `.rez` format hardening — identity, versioning, and a reader that is atomic against its writer

- **Opened:** 2026-09-11
- **Status:** **IMPLEMENTED** (all seven items, PR #1201). Deferred items
  are in `docs/backlog.md` under "`.rez` — format hardening".
- **Arc:** follows the [SQLite container](2026-08-12-rez-sqlite-container.md),
  the [read-path fix](2026-08-27-rez-vs-parquet-read-path.md), and
  [multi-endpoint record](2026-08-28-multi-endpoint-rez-record.md). Those built
  the container and made it fast; this is the pass that asks whether it can
  be trusted as a *format* rather than as a rezolus artifact.
- **Owner:** Brian Martin
- **Repos:** rezolus (`crates/rez`, `src/recorder`, `src/parquet_tools`,
  `src/agent/exposition`). A consumer-side change in systemslab (composition of
  redundant recordings) is named but not owned here.

## Why

The container design was priced carefully and the layout has been measured
three times. What has not been reviewed is the layer that makes it a format:
identity, versioning, the contract between a live writer and a reader, and the
public surface the crate offers to anything that is not rezolus. A full review
of `crates/rez` (~18k lines; 190 crate tests, all passing at `e1942a5d`) found
that layer thin. The verdict is that the design is sound and stays; the
findings below are all fixable without touching the segment layout.

The immediate trigger was a systemslab composition bug: two `record` streams
scraping the same agent were summed as if independent, and nothing in the
archive could say they were the same producer. The review confirmed there is
**no identity anywhere in the chain** — the V3 snapshot's top-level `metadata`
map (`metriken-exposition` `SnapshotV3.metadata`) carries nothing identifying,
and a recording is a SQLite rowid plus a labels JSON
(`crates/rez/src/rez_sqlite.rs:1459`).

## Findings (confirmed against code at `e1942a5d`)

Ranked. Each was read end to end by the reviewer and re-read before recording.

1. **The reader is not atomic against a live writer.** `table_segments`
   (`crates/rez/src/reader.rs:1504`) reads the sealed segments and then the
   live WAL as two separate autocommit statements. A seal committing between
   them removes those rows from `LIVE_WAL_PREDICATE` (`ts > MAX(last_ts)`), so
   the reader sees neither the new segment nor the WAL rows. The open-time
   probe in `from_v3_db` has the same shape. `RezDb::read_snapshot`
   (`rez_sqlite.rs:824`) exists and hindsight's dump uses it; the reader never
   does. `SegmentSource::Db` reopens the file per lazy read, so a viewer on a
   hindsight buffer can hit this on any query. The 2026-08-12 entry's claim
   that reads are consistent "by SQLite's WAL-mode guarantee" holds per
   transaction, and this is two.
2. **No version gate on the SQLite container.** `schema_version` is only ever
   inserted (`rez_sqlite.rs:208`, `:339`); `open`/`open_bytes` never read it,
   and no `PRAGMA application_id` is stamped. `looks_like_v3`
   (`crates/rez/src/rez.rs:1048`) is "starts with `SQLite format 3\0`", so any
   SQLite file is classified as a v3 `.rez` and fails later with
   `no such table`. A future v4 opens as v3. The tar path has a gate
   (`REZ_MAX_SUPPORTED_VERSION`, `rez.rs:912`) and a test; the container has
   neither.
3. **Routing catalogs are probed from the first segment only.**
   `TableReader::names` (`reader.rs:276`) is documented as sufficient because
   "a table's segments share a schema", but the writer explicitly supports
   schema drift inside a table (`rez_v3_writer.rs:3692`, the per-segment
   re-anchor rule; `wal.rs:63` for V2 cells), and cgroup and Prometheus series
   appear mid-recording. A metric first seen after seal 0 is unroutable:
   "query references no metric present in this .rez".
4. **Retention can silently lose rows.** Metadata is anchored on the first WAL
   row of a segment span and `evict_before` (`rez_sqlite.rs:1125`) deletes by
   timestamp with no regard to anchors. Group tables drop the un-anchored rows
   (`wal.rs:371`); V2 sampler tables keep them with **empty metadata**
   (`wal.rs:214`), which is worse. Nothing enforces hindsight `duration` >
   `SealPolicy::max_age`; the shipped 15 min vs 300 s is a 3× margin by
   coincidence. The writer test at `rez_v3_writer.rs:3704` asserts the loss.
5. **One bad tick kills the writer thread for every recording.** Any
   `rusqlite::Error` in `writer_loop` (`rez_v3_writer.rs`, the `Msg::Wal` and
   `Msg::Evict` arms) returns `Err`, the thread exits, and every
   `RecordingWriter` in a multi-endpoint archive fails on its next send. No
   retry, no classification (ENOSPC vs busy vs constraint), no per-recording
   isolation. This is a direct consequence of every error being a `String`.
6. **`combine` silently merges recordings with identical label sets.**
   `combine_rez_v3` (`src/parquet_tools/combine.rs:320`) appends every input's
   recordings under fresh ids with no check; only the live `record` path warns
   (`src/recorder/mod.rs:891`). The result is two recordings no selector can
   name, which MCP then refuses (`src/mcp/recording_selector.rs:927`).
7. **Sidecar adoption.** `RezDb::create` and the in-place `filter`/`upgrade`
   renames (`src/parquet_tools/filter.rs:314`, `mod.rs:684`) do not clear a
   stray `-wal` beside the target; the crate's own doc (`rez_sqlite.rs:235`)
   says SQLite adopts it silently.
8. **Smaller, all confirmed:** `annotate` on a multi-recording `.rez` is one
   autocommit per recording (`src/parquet_tools/annotate.rs:415`);
   `upgrade_tar_to_v3` marks complete outside its transaction; readers open
   `SQLITE_OPEN_READ_WRITE` (`rez_sqlite.rs:351`) so a read mutates a finished
   archive on close; `parquet_ingest` trusts the timestamp column to be sorted
   (`parquet_ingest.rs:118`) and lets a `sampler` value containing `/`
   masquerade as a group table; `RezReader::interval()` fabricates `1.0` when
   unknown (`reader.rs:1424`); `SegmentSource::Bytes` clones every segment so
   a browser upload is resident three times; `metric_metadata()` fetches every
   BLOB of a table to read one footer; routing refusals surface as
   `QueryError::ParseError`.

**API.** `lib.rs` is a module list with no facade. Two unrelated public types
are both named `RezArchive` (`rez.rs:603`, `rez_v3_writer.rs:140`). Three error
conventions coexist: `String`, a *private* `Box<dyn Error>` alias appearing in
public signatures (`rez.rs:180`), and `Box<dyn Error>` on the reader. A third
party can write a `.rez` without metriken only by hand-tracking `seq`,
`SegmentMeta`, and clock offsets. The rezolus-specific vocabulary is small —
`sampler` as the table unit, the `/`-in-key group rule, `source`/`host`
auto-labels, the `systeminfo`/`descriptions`/`service_queries` metadata keys —
and everything else (recording, table, segment, window, WAL, seal policy, clock
offsets) is already generic.

**Docs.** There is no format specification. The v3 layout, WAL row msgpack
shapes (`WalCell`, `WalGroupRow`), `LIVE_WAL_PREDICATE`, and the column
conventions (`:wall_offset`, bare and per-metric `:window_*`, `metric_type`
field metadata, the slash rule) live in doc comments and eight journal entries,
unversioned. `rez.rs:1` and `docs/parquet_metadata.md:9` still describe a tar.
README omits `recording snapshot`, `.rez` `filter`, and `.rez` `annotate`, and
its Prometheus-plus-`.rez` paragraph (README.md:214) contradicts the code and
its own `--help` (`src/recorder/mod.rs:74` vs `:91`).

**Tests.** Numerous and happy-path shaped. The "kill" tests
(`rez_v3_writer.rs:2058`, `:2103`) are graceful drops. No SIGKILL, torn
sidecar, disk-full, busy-injection, newer-schema-version, seal-vs-open race,
corrupt-BLOB, or duplicate-label-combine test. `wal.rs` has zero in-module
tests.

## Identity — the design decision

Two IDs, at different layers, and both are needed:

- **A producer epoch**, emitted by the agent, identifying the observed
  producer's *counter epoch* — regenerated whenever the cumulative counters
  reset, not per process lifetime (OpenTelemetry's `start_time_unix_nano` for
  cumulative series is the precedent; Prometheus lacks it and has the same
  class of bug). It answers "are these the same monotonic series". Two
  recordings with the same epoch and overlapping time are redundant
  observations of one series, so a consumer can **merge coverage** rather than
  refuse or double. It also makes an agent restart mid-recording a detectable
  discontinuity. Today the query engine applies the Prometheus reset heuristic
  (`metriken-query` treats a drop as a reset), so a restart is a small silent
  error rather than a negative delta, and a reset landing above the previous
  value is invisible; the epoch turns a lossy heuristic into an exact signal.
  Matching on `host` was rejected: EC2 reuses private IPs, the observed instance
  carries a stale hostname tag, and one host can legitimately run two agents
  with different sampler sets.
- **A recording UUID**, minted at `insert_recording` and preserved by every
  copy, answering "is this the same file I already combined". It gives
  `combine` a content-based dedup, and gives MCP `--recording` and the viewer's
  A/B slots something to select on that labels cannot.

A writer-session marker is a third thing, needed the day reopen-for-append
exists so a reader can tell "writer 2 appended here" from continuity. It is
sequenced last (item 7) because it depends on the version gate (item 1) having
a place to declare it.

**Transport.** The producer epoch rides in the V3 snapshot's free-form
top-level `metadata` map — zero change to `metriken-exposition`. The recorder
copies it into recording metadata on the first tick and records a change of
value as a discontinuity. Absent means unknown; consumers fall back to
host+overlap heuristics, which is no worse than today.

## Plan — ordered, closed out below as each lands

1. Stamp `application_id` and check `schema_version` on open, with a typed
   error. Refuse newer. Test: a v3 file with `schema_version = 4` is refused
   by message; a non-rez SQLite file is refused as not-a-rez.
   **DONE.** `RezDb::create` stamps `application_id = 0x5245_5A00` (`REZ\0`)
   and `user_version = 3` into the header; `open`/`open_bytes` return a typed
   `OpenError { NotRez | Unsupported | Db }` (`From<OpenError> for String`
   keeps every caller's `?`), and `check_format` decides from the stamp:
   stamped → `user_version` must equal the build's; id `0` (every archive
   written before this) → the `schema_version` table decides, and a missing
   catalog names the copied-from-under-a-writer case; any other id → not a
   `.rez`. `looks_like_v3` now reads the id from the 100-byte header, so
   `detect_rez_format` says `NotRez` for a stamped foreign database without
   opening it. One limit, documented and tested: an *unstamped* foreign SQLite
   file (id 0) is indistinguishable from a pre-stamp archive at the header, so
   detection says v3 and `open` refuses. Seven tests in `rez_sqlite::tests`,
   including that `VACUUM INTO` carries the stamp (hindsight's dump and
   `recording snapshot` depend on it).
2. Wrap the reader's per-table fetch and the open-time probe in
   `read_snapshot`. Test: a seal committed between the two reads by a second
   connection leaves the table complete.
   **DONE.** `table_segments` reads segments and WAL in one `BEGIN DEFERRED`
   snapshot (`table_segments_with` carries a between-reads hook, `&|| {}` in
   production); `from_v3_db`'s whole catalog phase — every recording's table
   list, probe segment, sealed span and live span — is one snapshot, so no
   two answers can straddle a seal. Test
   `a_seal_committed_between_the_two_reads_does_not_open_a_hole` seals from
   a second connection inside the hook and asserts all three rows survive.
   Negative control run before committing: on the two-statement form the
   test reads **0 of 3 rows** — the finding, reproduced exactly. The
   incomplete-recording warning now says "last committed tick" rather than
   the tar-era "last checkpoint".
3. Add a `uuid` column to `recordings`, preserved through every copy; refuse
   identical label sets in `combine_rez_v3` unless `--allow-duplicate-labels`,
   and refuse identical UUIDs outright. Test: `combine a.rez a.rez` fails.
   **DONE.** `recordings.uuid TEXT`, a v4 UUID minted at insert from SQLite's
   own `randomblob(16)` (no new dependency, and it works in the wasm reader
   build, which has no random source of its own). `copy_recordings_into`
   carries it verbatim via `RezTx::insert_recording_with_uuid`; `VACUUM INTO`
   copies it as data, so hindsight dumps and `recording snapshot` keep it.
   `read_recordings` probes the schema and reports `None` for archives
   written before the column — an additive, nullable column, so
   `SCHEMA_VERSION` stays 3 and old readers ignore it (an old *copier* drops
   it, which degrades to "unknown", not to wrong). `combine` now refuses two
   recordings with equal uuid unconditionally ("assembling it twice would
   double every value") and identical label sets unless
   `--allow-duplicate-labels`; the check runs before the output is created.
   `recording metadata` prints the uuid (text and JSON). Tests: minting shape
   and uniqueness, copy preservation, a pre-column archive reads as unknown,
   and three `combine` cases (same file twice and a `VACUUM INTO` copy,
   identical labels with and without the flag, distinct uuids carried
   through). Selection by uuid (`--recording uuid=…`, A/B slots) is left to
   the backlog.
4. Emit a producer epoch from the agent into snapshot metadata; record it and
   its changes. Test: a restarted agent's epoch change lands as a discontinuity
   the reader can enumerate.
   **DONE.** The agent mints one v4 UUID per process
   (`exposition::http::snapshot::producer_epoch`, 16 bytes from
   `/dev/urandom`, a pid+time fallback) and carries it in every V2 and V3
   snapshot's top-level metadata under `producer_epoch` — the counter epoch
   *is* the process for rezolus, since every counter it exposes lives in it.
   `StreamRecorderV3::stage` reads it on every tick, so the recorder and
   hindsight get it without a line of their own: the first sighting writes
   `producer_epoch` and a `producer_epochs` history
   (`[{"epoch","from_ts"}]`) into the recording's metadata; a change appends
   to the history and writes a timeline event (`kind: producer_epoch`,
   `id: producer_epoch:<new>`, the shape `dashboard::events::Event` reads) so
   the viewer draws the discontinuity where it happened. Persisted through a
   new writer message (`Msg::UpdateMetadata` → `patch_recording_metadata`),
   in order with the ticks, not at finalize — a killed recording still names
   its epoch. Absent means unknown: a producer that sends no epoch (older
   agent, Prometheus target) leaves no keys behind. Keys are constants in
   `rez::rez` (`PRODUCER_EPOCH_KEY`, `PRODUCER_EPOCHS_KEY`) so the spec and
   both crates cite one name. Tests: the agent's epoch is constant across
   snapshots in a process and v4-shaped; the writer records first sighting,
   change (history + event), and finalize; no-epoch writes nothing. The
   consumer half — merging two recordings of one epoch into one series —
   belongs to systemslab and is not done here.
5. Write `docs/rez-format.md` as a specification with a conventions version;
   fix the stale module doc, `docs/parquet_metadata.md`, and README.
   **DONE.** `docs/rez-format.md` specifies the container (detection by
   header stamp, SQLite geometry, the sidecar), the five catalog tables and
   what each column means, the table-key `/` rule, the live-WAL predicate,
   the segment encoding (column order, both window shapes, value-column
   naming and required field metadata, histogram lists), both WAL row
   encodings as positional msgpack with the anchoring rules, the reserved
   metadata keys including the identity keys, the anchored time model,
   retention and its known anchor-loss limit, what copies preserve, and the
   compatibility rule for what bumps `SCHEMA_VERSION` versus what is
   additive. Every claim was checked against the code before it was
   written; one was corrected in the process (histogram lists are dense,
   not sparse). Stale text fixed: `rez.rs`'s module doc described a tar;
   `docs/parquet_metadata.md`'s `.rez` section described the manifest and
   omitted `--metrics`, events, `snapshot`, `upgrade`; README's output-format
   paragraphs and multi-endpoint section said Prometheus forces parquet
   (only `--separate` demotes, since #1191); `record --help` said the same
   in its format table while contradicting itself two paragraphs later;
   CLAUDE.md's recorder paragraph carried the same stale fallback. README's
   tools section now lists `snapshot`, `upgrade`, and the `.rez` forms of
   `filter`/`annotate`/`combine`. `tests/help_text.rs` passes.
6. Replace `String` errors with a `thiserror` enum; let `writer_loop` retry the
   retryable class and isolate a constraint failure to the recording it
   belongs to.
   **DONE, scoped to the container boundary.** Every `RezDb`/`RezTx` method
   now returns `DbError { code: Option<rusqlite::ErrorCode>, extended_code,
   message }` — plain `std::error::Error`, no `thiserror` (the crate takes no
   new dependency) — with `From<DbError> for String` and `From<String> for
   DbError` so the rest of the crate and the binary keep their `?` unchanged;
   the crate's other `String`/`Box<dyn Error>` signatures are left as they
   are, because the value was at the writer, where the SQLite code was being
   stringified away. `is_retryable()` is BUSY/LOCKED/FULL/IOERR/NOMEM/
   INTERRUPT/SCHEMA; `is_constraint()` is CONSTRAINT; anything else stops the
   writer as before. The writer loop: a tick, seal, retention, metadata
   update or finalize that fails retryably is retried three times over
   ~310 ms (on the writer thread, so the bound-1 channel backpressures the
   scrape loop for that long); a tick that still fails is dropped with a
   warning, and 30 consecutive drops stop the writer with the last error; a
   seal that still fails is deferred — its rows stay in the WAL and go out
   with the next batch, and `seq` is now advanced only after the commit so a
   retried batch reuses its numbers; retention and metadata updates that
   still fail are logged and skipped. A tick that fails on a **constraint**
   (one recording repeating a `(sampler, ts)`) is re-committed per recording
   and only the colliding recording's rows are dropped, warned once per
   recording. Tests: the classifier over the SQLite codes;
   `a_recordings_colliding_tick_is_dropped_without_taking_the_archive_down`
   (two recordings, one collides, the other's rows land, the writer answers
   a third tick); `a_busy_database_is_retried_rather_than_fatal` (a second
   connection holds `BEGIN IMMEDIATE` for 150 ms against a 20 ms
   `busy_timeout`, via a test-only `create_with_busy_timeout`). Both fail on
   the old single-`?` commit path — run as a negative control before
   committing. `open_read_only` and the remaining `String`/`Box` signatures
   stay on the backlog.
7. `RezArchive::open` for append with a session marker: seed `next_seq` from
   `MAX(seq)`, re-anchor the clock, record the session. This is what lets
   hindsight survive an agent restart (today its buffer lives in a `TempDir`,
   `src/hindsight/mod.rs:170`).
   **DONE in the crate; hindsight wiring deferred.** `RezArchive::open`
   (`RezDb::open_for_write`, the same format gate as a reader) spawns the
   writer over the existing file; `writer_loop` seeds `next_seq` from
   `RezDb::next_seqs` (`MAX(seq)+1` per table) and `observed` from
   `observed_clock_offsets`, so a resumed table continues its sequence and
   finalize never writes a second offset at an old timestamp.
   `resume_recording(id, anchor)` runs on the writer thread: it refuses an
   anchor at or before the recording's newest row ("the wall clock went
   backwards across the restart"), clears `complete`, appends a
   `writer_sessions` entry with the new anchor and `resumed_after_ts`, writes
   a `writer_session` event at the anchor, and returns a handle carrying
   `floor_ts`; `StreamRecorderV3::stage` refuses any tick at or before it.
   Every recording now records its first session at insert, so
   `writer_sessions` has one entry for a recording written in one go. The
   resuming process uses its OWN wall reading as the anchor — its monotonic
   clock restarted — and rows keep `timestamp + :wall_offset = wall`. Tests:
   a finalized archive reopens, resumes, seals twice more (seqs `0..=3`,
   rows 4, one offset at the old finalize, `complete` down then up, two
   sessions, one event) and reads as one table; a backwards anchor and a
   backwards tick are refused before anything is written; a missing
   recording is refused and the writer stays usable. Spec §10.1.
   **Not done:** hindsight still creates its buffer in a `TempDir` and drops
   it on exit; keeping it at a stable path and resuming on start is a
   hindsight behavior change (a stale buffer from a previous run is then
   retained — desirable for an incident, but it changes what `duration`
   bounds and what a clean exit removes) and goes to the backlog with the
   crate primitive now available.

Items 3 (retention anchor loss) and the remaining "smaller" findings are
tracked in the backlog rather than sequenced here; they are real but none
blocks the format question.

## GO / NO-GO

There is no measurement gate — none of the items changes the layout, the seal
policy, or the tick path. The gate is behavioral: each item lands with the
test named beside it, and the crate's wasm reader build
(`cargo check -p rez --no-default-features --target wasm32-unknown-unknown`)
stays green, since items 1, 2, 3 and 6 touch the read path.

## Deferred / reopen

- **Retention anchor loss (finding 4).** Fix candidates: clamp `max_age` ≤
  `lookback/2` in `HindsightBuffer::create`; make eviction anchor-aware (never
  delete a span's first live row); or force-seal a span whose anchor would be
  evicted. *Reopen:* when any hindsight config sets `duration` under 10 min.
- **First-segment-only routing probe (finding 3).** Probe the union of first
  and last footers (the last is already opened for `span`), or persist a
  per-table name catalog in the SQLite catalog at write time — which the
  read-path entry already lists as the last fixed open cost. *Reopen:* with
  the first Prometheus recording whose series appear after the first seal.
- **Readers open `READ_WRITE`.** Add `open_read_only` with `immutable=1` when
  the caller knows no writer exists. Cheap; sequenced after item 6 so it gets
  the typed error.
- **`lib.rs` facade and the `RezArchive` name collision.** Rename the writer
  handle, re-export the six real entry points, gate the test-only helpers.
  *Reopen:* with the first external consumer.
- **Metriken-free `TableWriter`** that owns `seq`/meta/clock offsets and takes
  `Cell`s, so a third party can write a `.rez` without reimplementing the
  bookkeeping. *Reopen:* same trigger.
- **Size gap** (2.25× parquet at 50 ms) is unchanged by anything here and
  remains owned by the read-path entry.
