# Recording to dendro archives: `record`, then hindsight, then the default

- **Opened:** 2026-09-28
- **Status:** OPEN — stages A (`record -o out.dendro`) and B (hindsight) built; C–E not.

## Goal

`record` and `hindsight` write dendro archives through metriken-archive's
`ArchiveWriter` (metriken `docs/journal/2026-09-28-archive-writer.md`), in
the layout of [the dendro archive layout entry](2026-09-25-dendro-archive-layout.md):
groups with slots long, their occupants in a `<table>/occupants` stream.
The `.rez` v3 writer (`crates/rez/src/rez_v3_writer.rs`) is then legacy,
kept for reading old files.

## Why now

The writer passed its gate against the `.rez` writer on four replayed
recordings (1 s and 100 ms, light and heavy thread churn): the same answers
to 213 of 213 queries on each, archives 3.3–8.8 times smaller, and a tick
p99 of 16–38 ms against 375–407 ms. The median tick at 100 ms was 12–28%
worse under an unpaced replay; that was accepted (2026-09-28). The numbers
are in the metriken entry's "Gate results".

## Decisions (2026-09-28)

- **Extension `.dendro`.** A dendro recording is named for its container,
  as `recording upgrade --to dendro` already names its output. Keeping
  `.rez` for both containers was the alternative; it was rejected.
- **Opt-in until the tools read it.** `-o out.dendro` or `--format dendro`
  selects it, and nothing selects it by default. The default flips once,
  at 6.0, after the `recording` subcommands accept dendro, so a default
  recording is never a file the tools refuse.
- **Stages.** Each is its own PR:
  - **A. `record`**: scrape path, Prometheus endpoints, several endpoints
    (one source each), endpoints that activate late, discard when nothing
    was captured.
  - **B. `hindsight`**: the rolling buffer and its eviction, and `/status`,
    `/dump` and `copy_range`, which read the `.rez` catalog directly
    (`src/hindsight/buffer.rs`) and need dendro's catalog and copy APIs.
  - **C. `record --stream`**: the stream sends rows plus identity-index
    frames, not schemas that carry slot labels; the recorder rebuilds the
    group schemas from the index before calling the writer (what the
    metriken entry leaves in rezolus).
  - **D. The `recording` subcommands** (`metadata`, `annotate`, `filter`,
    `combine`, `snapshot`) accept dendro. `annotate --event` and the
    viewer's Save-as-Report patch a source's metadata through dendro's
    `ArchiveMut::patch_source_metadata`.
  - **E. 6.0** flips `record` and `hindsight` to dendro by default.

## A: built

`src/recorder/`:

- `Format::Dendro` (`src/main.rs`), chosen by a `.dendro` extension or
  `--format dendro` (`config::format_from_extension`, the TOML `format`
  key). `config::is_archive` names the two archive formats, and archive
  mode (`wants_rez`) covers both.
- `RezStream` holds a `Sink`: `Rez` (the `.rez` v3 writer, as before) or
  `Dendro` (`ArchiveWriter` and one `SourceRecorder` per endpoint). Staging,
  the per-tick commit, sealing, finalize and discard dispatch on it; the
  recording loop is unchanged. Discard removes the archive and its SQLite
  sidecars through `RezDb::remove_archive`, which is container-agnostic.
- `--stream` with `.dendro` is refused at parse time until C, with the
  same message shape as for parquet and raw. `--separate` with several
  endpoints is refused for `.dendro` as for `.rez`, naming the format.
- User-facing text names the format the run chose: the start, open and save
  messages, the help's format list, OVERWRITING, `--separate` and `--label`.

Segments are sealed with zstd level 3 (metriken-archive 0.2.5): on the
replayed recordings, 55–57% smaller than LZ4 for 3–4% more encode time,
with the same tick latency and query time (metriken's writer entry).
Every rezolus reader already decodes zstd.

Found while building it: metriken-archive ingested only V3 snapshots, so a
`.dendro` recording of an agent serving V2 would have been empty with no
error. The recorder's own test fixture is a V2 snapshot, which is how it
showed. The writer now ingests V1/V2 the way the `.rez` writer does
(metriken#186).

Tests: `a_dendro_recording_round_trips_through_rezreader` (two endpoints,
one activated mid-run, each read back as its own recording at 1/s),
`discarding_a_dendro_recording_leaves_no_file_behind` (sidecars included),
`a_wrapped_dendro_run_writes_run_start_and_run_end_events`,
the format and refusal cases in `config.rs`, and
`a_prometheus_endpoint_records_into_a_dendro` through the binary.

Run events (`run_start`/`run_end`, #1323) reach a `.dendro` through
`SourceRecorder::update_metadata` (metriken-archive 0.2.1). `merge_events`
sends the `.rez` writer the whole metadata map, which it replaces, and
the dendro writer a patch of the `events` key alone, which it merges.
`RezStream` keeps each recording's last map, outside the `Sink`, for
both.

## B: built

`src/hindsight/`:

- An `output` ending in `.dendro` selects a dendro buffer
  (`hindsight.dendro`); anything else keeps the `.rez` buffer. The buffer's
  `Writer` is `Rez` or `Dendro` (`buffer.rs`); ingest stages and commits one
  tick, and `maintain` seals then evicts, the occupant streams one
  restatement period behind their data (`SourceRecorder::evict_before`).
  `segment_rows` sets the row cap on either.
- `/status` (`summarize`) recognizes the container by content and reads
  dendro's catalog: page statistics, and per stream the sealed and live
  spans. A long table's occupant stream is listed as a table of its own.
- A dump, whole or ranged, is dendro's `copy_sources_into` from the read
  handle, with `Encoder::for_streams` (metriken-archive 0.2.4) encoding the
  live tail into a final segment per stream; segments, clock offsets and
  caller rows are copied as they are. The dump is fully sealed, so a reader
  has no WAL tail to rebuild on every open, which a `VACUUM INTO` copy would
  carry over unsealed (decided 2026-09-28). The copy is then marked
  complete, as a `.rez` dump is. dendro's `vacuum_into` also fails on the
  read handle today, because `Archive::open` sets `query_only`; see the
  backlog.
- A ranged dump of a buffer with long tables starts one restatement period
  (300 s) early. An occupant's labels are written at first sight and
  restated every period, so one first seen before the range is named only
  by a restatement up to a period before it. The test for it fails without
  the lead.
- `GET /dump` names its download after the buffer's container.

Tests: `a_dendro_buffer_evicts_whole_segments_and_quiet_wal_rows`,
`a_dendro_dump_is_complete_and_readable`,
`a_ranged_dendro_dump_keeps_labels_of_an_occupant_seen_before_it`, and
`a_dendro_buffer_dumps_a_dendro_archive` through the binary
(`tests/hindsight_dump.rs`).

## Not in scope

- Replacing `.rez` reading. Old recordings stay readable.
- Sorting at seal. The writer seals in arrival order; sorting belongs to
  compaction (dendro's `CompactSpec`, open).

## Cross-references

- metriken `docs/journal/2026-09-28-archive-writer.md` (the writer, its
  gate) and `docs/journal/2026-09-28-high-cardinality-stack.md` (phases).
- [Reading dendro archives](2026-09-28-dendro-reader.md).
- 6.0: #1224.
