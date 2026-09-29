# Recording to dendro archives: `record`, then hindsight, then the default

- **Opened:** 2026-09-28
- **Status:** OPEN — stages A (`record -o out.dendro`), B (hindsight), C (`record --stream`), D1 (`recording` metadata, annotate, check, snapshot), D2 (`filter`), D3 (`combine`) and D4 (Save-as-Report) built; E not.

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

## C: built

The design above expected the stream to carry identity only in its index
frames, so that the recorder would rebuild group schemas from them. It
does not: the agent builds the stream from the same `create_v3` pass as a
scrape (`latest_rows` → `wire::encode_snapshot`), so a streamed schema's
slotted members already carry `id` and the identity labels, `__uid__`
included, and the index frames repeat them. So the dendro arm needs no
index at all:

- `stream::StreamSchemas` turns each pass's `WalGroupRow`s back into a V3
  snapshot for `SourceRecorder::stage`. The producer sends a stream's
  schema whenever its hash differs from the last one it sent on that
  stream (`FrameProducer::interval`), so one schema per stream is all it
  keeps, and a row carries its schema into the snapshot exactly when the
  schema changed; the writer validates and lays out a schema once per
  change. A row naming a hash the connection never sent is skipped and
  counted.
- The recorder still applies the index frames (they gate rows on the
  source's index state), but a `.dendro` does not write them: its occupant
  streams hold the same identity, taken from the schemas.
- `--stream` is accepted for `.dendro` (`reject_stream_without_rez` refuses
  only the non-archive formats).

Tests: `a_streamed_dendro_recording_takes_occupants_from_the_schemas` (a
slot changing hands mid-stream, with its counter restarting, reads back as
two occupants with their own labels and rates beside a steady one; no
caller rows), `a_streamed_schema_is_attached_where_it_arrived`,
`a_streamed_row_with_an_unsent_schema_is_skipped`.

## D1: built

D is split by what each subcommand needs from dendro. D1 is the part with
no layout questions: catalog reads, metadata writes and whole copies.

- `recording metadata` describes a dendro archive from its catalog, as a
  v3 `.rez`: sources as recordings, every stream as a table (a long
  table's occupant stream included), rows, segments, unsealed WAL rows,
  cadence and the clock line (`read_dendro_summary`); `--json` has
  `container: "dendro"` and no `.rez` `version`. Rows are the catalog's,
  which counts ticks (the WAL rows a segment consumed), not a long table's
  rows per occupant.
- `recording annotate` and `check --annotate` write into a dendro archive
  through `MetadataStore`, which lists and replaces each recording's
  metadata in either container; the rest of annotate is unchanged. A
  dendro archive a writer still holds cannot be opened for writing, and
  says so. `check`'s refusal of `--annotate` on a dendro archive is gone.
- `recording snapshot` of a dendro archive is a sealed copy
  (`dendro_copy::copy`, `CopySpec::everything()`): the live tail is
  encoded into the copy's last segments, staged beside the output and
  renamed. The copy code is shared with hindsight's dump
  (`src/dendro_copy.rs`).
- `recording upgrade` names a dendro input ("is a dendro archive; there is
  nothing to upgrade", "is already a dendro archive"), and `recording
  convert` refuses a SQLite archive by name rather than failing to decode
  it. The generic refusal left in `RezDb::open` (still reached by `filter`
  and `combine`) says "this command reads .rez archives only".
- dendro 0.3.2, whose `vacuum_into` works on the read handle; nothing here
  uses it (a snapshot seals its tail instead).

Tests: `a_copy_seals_the_live_tail`, `a_dendro_snapshot_is_sealed_and_whole`,
`a_dendro_archive_is_described_from_its_catalog`,
`annotate_writes_into_a_dendro_archive`, `upgrade_names_a_dendro_input`,
`a_sqlite_archive_is_recognized`, and `check_annotates_a_dendro_archive`
through the binary (`tests/recording_check.rs`). A fixture records a long
table through the real writer (`dendro_copy::fixtures::recorded`).

Still to do in D:

- **D2, `filter`:** built (below).
- **D3, `combine`:** built (below).
- **D4, Save-as-Report:** built (below).

## D2: built

`recording filter` on a dendro archive (`filter_dendro`), through
`dendro_copy::copy`:

- `--samplers` is a stream predicate on the stream's sampler; an occupant
  stream counts as its table's, so it goes with it.
- `--metrics` is metriken-archive's `KeepMetrics` (0.2.6): the `.rez`
  column rules (structural columns always, a value column by name, base or
  `metric` metadata, a per-metric window with its metric) plus a long
  table's `occupant` column, with occupant streams copied whole. Segments
  are re-encoded with the writer's properties (`segment_props`, zstd-3).
- Two things this needed from dendro (0.3.3): a projection now keeps the
  file's key-value metadata, without which a projected long segment lost
  `metriken.layout` and read as wide; and `ColumnFilter::projects`, which
  lets an occupant stream be copied unprojected, since a field-by-field
  filter cannot tell its columns from a table's.
- A table left with no kept metric is dropped by the copy; its occupant
  stream is then evicted from the staged copy, since it names occupants of
  nothing.
- The `.rez` guards carry over: an unknown sampler is refused, as is a
  filter that keeps no table, and the output is staged and renamed.

Tests: `filter_dendro_by_sampler_keeps_the_occupant_stream_with_its_table`,
`filter_dendro_by_metric_keeps_long_tables_long` (the kept metric answers
the same by `comm`),
`filter_dendro_refuses_an_unknown_sampler_and_an_empty_result`; in
metriken, `keep_metrics_trims_a_long_table_and_keeps_it_long`.

## D3: built

`recording combine` into a `.dendro` output (`combine_dendro`):

- The output's extension picks the container. `.dendro` inputs are copied
  as they are; a `.rez` input (either container) is first converted as
  `recording upgrade --to dendro` converts it, into the staging directory.
  Parquet inputs are refused with a pointer (combine them into a `.rez`,
  then convert); there is no parquet-to-dendro ingest.
- Every input is copied into one transaction through `copy_sources_into`,
  each in its own read snapshot with its live tail sealed; a source keeps
  its uuid. The same source given twice (the same uuid: a file and its
  snapshot, say) is refused through dendro's `shared_sources`, since it
  would count every value twice. The output is refused if it exists, and
  is staged and renamed.
- A dendro input with a `.rez` output is refused before anything is
  created. The `.rez` path creates its output before opening the inputs,
  so a dendro input used to leave a half-made `.rez` behind.

Tests: `combine_assembles_dendro_archives` (two sources, each still long,
readable as two recordings), `combine_refuses_the_same_dendro_source_twice`,
`combine_into_dendro_converts_a_rez_and_refuses_the_rest`.

## D4: built

Save-as-Report on a dendro source writes a dendro report
(`report_save::build_dendro_report`), in the server viewer and the browser
viewer alike, since both call `build_rez_report_from_rez`, which now
recognizes a dendro source by its bytes:

- The copy is D2's: `copy_sources_into` into an in-memory archive, with
  `KeepMetrics` when the save is trimmed, segments re-encoded with
  `segment_props` (zstd-3), and occupant streams whose table was dropped
  evicted afterwards.
- The source's live tail is sealed into the report, so a report of a
  running recording carries its last rows as segments. The tail encoder
  (`Encoder`) was behind metriken-archive's `write` feature, which the
  browser build cannot enable; metriken-archive 0.2.7 moves it outside
  (metriken #196), since it uses only metriken-segment.
- The selection, report and events keys go on the report's first source,
  where the `.rez` report puts them on its first recording.
- The download is named `rezolus-report.dendro`: the server names it from
  the source path, the browser from `report_extension()`. The server
  frontend took any name other than `.rez` for `.parquet`, and now keeps
  the name's extension; the upload picker accepts `.dendro`.

Tests: `a_dendro_source_saves_a_trimmed_dendro_report` (an unfinalized
source; the long table stays long, the markers are on the source, the
report opens from its bytes and the kept metric answers the same by
`comm`), `a_dendro_report_drops_the_occupant_stream_of_a_dropped_table`
(fails with the eviction removed), and
`an_untrimmed_dendro_report_keeps_every_stream`.

## Seal policy

Measured on three replayed recordings (metriken
`docs/journal/2026-09-28-archive-writer.md`, "Seal policy, measured"):
rezolus's combination of 8 MiB, 900 rows and 5 minutes is kept. Longer
segments were 7–12% smaller and up to 10% faster to query, but their seals
took 260–630 ms against 41–96 ms, on the tick; aligned seals put every
stream on one tick (1.8–4.2 s); a byte or row cap alone never seals a slow
stream. Sealing only at finalize was ten times the disk while recording, a
5.3 s open of the live archive and a 21 s finalize on the busy host.

## Not in scope

- Replacing `.rez` reading. Old recordings stay readable.
- Sorting at seal. The writer seals in arrival order; sorting belongs to
  compaction (dendro's `CompactSpec`, open).

## Cross-references

- metriken `docs/journal/2026-09-28-archive-writer.md` (the writer, its
  gate) and `docs/journal/2026-09-28-high-cardinality-stack.md` (phases).
- [Reading dendro archives](2026-09-28-dendro-reader.md).
- 6.0: #1224.
