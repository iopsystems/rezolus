# Rezolus 6.0.0: release readiness

**Status: OPEN.** Plan opened 2026-10-08, updated 2026-10-12. `Cargo.toml` is
at `6.0.0-alpha.46`; no `v6.*` tag exists and the latest release is v5.27.0.
This entry lists what has to be true before 6.0.0 is tagged and in what order,
and records the decisions taken on 2026-10-08 and 2026-10-12. On 2026-10-12
6.x became `.dendro`-only for writing, decision 1 was withdrawn (an agent
older than 5.21.0 is refused), and 6.0.0 now waits for metriken path steps 4
to 7, the viewer move included. The parent issue is #1224.

Related entries in other repositories, opened the same day:

- metriken `docs/journal/2026-10-08-one-recording-stack.md`: one recording,
  exposition and viewing stack for every metriken producer, and readers for
  other languages. Its 2026-10-12 update and metriken
  `docs/journal/2026-10-12-metriken-recorder.md` (path step 4) record the
  decisions this entry took over on 2026-10-12.
- systemslab `docs/journal/2026-10-08-dendro-artifacts.md`: reading and then
  recording `.dendro` artifacts.
- cachecannon `docs/journal/2026-10-08-dendro-recording.md` and llm-perf
  `docs/journal/2026-10-08-dendro-recording.md`: getting each load generator
  into the same archive as the agent.

## Where 6.0 stands

Every mode that writes an archive defaults to `.dendro`: `record`, the
`hindsight` buffer and its dumps, the live viewer's temporary archive, and
Save as Report from a dendro source. `view` (server and WASM), `mcp` and every
`recording` subcommand that reads an archive read `.dendro` and `.rez` through
`RezReader`, which wraps metriken-storage's `ArchiveReader` (metriken-archive
until metriken path step 2). Parquet is read through
`metriken_query::ParquetReader`.

Measured on 2026-10-08:

- **The 5.25.1 reader reads 6.0 archives.** A recording made by
  6.0.0-alpha.41's agent and `record` on a 32-core Linux host (3 minutes,
  2.2 MB, 89 tables, 28 long tables with occupant streams, 18 cgroups) was
  queried with the v5.25.1 and the 6.0 binaries. Of 431 query results over
  172 metrics, 393 were identical. 23 differed by floating-point rounding
  (relative difference about 1e-14) and 15 were `histogram_quantiles`
  results where 6.0 returns one more point at the start, the shared points
  agreeing. Per-cgroup queries (`sum by (name)`, `sum by (name, state)`,
  `{state="user"}`) returned the same 62–124 series with identical values. The
  dendro schema (`user_version` 4) and the encoder version
  (`metriken-archive/1`) are the same in dendro 0.3.3 / metriken-archive 0.2.7
  (5.25.1) and dendro 0.3.4 / metriken-archive 0.3.5 (6.0). This is what lets
  a reader pinned at 5.25.1, such as systemslab's after its phase 0 bump, read
  6.0 output.
- **Long tables, checked against the values written.** An archive written by
  metriken-archive 0.3.5's `ArchiveWriter` with 57 task occupants replaced
  over 600 ticks, one vacant slot and a cgroup gauge group, finalized and
  with its occupant rows still in the WAL, returned every occupant with its
  written labels and rate from both binaries.

The extra first point is a query-engine difference between metriken-query
0.33 and 0.34, not a reader difference. Whether it was intended is not
recorded.

## Decisions (2026-10-08)

1. **Withdrawn 2026-10-12, replaced by decision 6.** It read: an agent that
   cannot stream is recorded to `.rez`, with a warning. A `record` run whose
   output was not named (no `-o`, no `--format`) and whose agent predates the
   stream (older than `STREAM_SINCE`, 5.21.0, `src/recorder/mod.rs:925`)
   writes `.rez` through the scrape path and logs a warning, as
   `demote_from_rez` (`src/recorder/mod.rs:2295`) already demotes a defaulted
   `--separate` run to parquet. A run writes one archive, so a defaulted run
   with any such agent among its endpoints writes `.rez` as a whole. An
   explicit `-o x.dendro` still refuses. `.rez` stays writable for as long as
   agents older than 5.21 are in use. It was never built.
2. **No single-schema parquet export.** A `.dendro` holds wide tables, long
   tables and occupant streams, and folding them into one wide parquet schema
   would undo the long layout. Consumers outside Rust get readers in their
   languages instead (metriken entry).
3. **`cachecannon view` reads `.dendro`** after a metriken bump (cachecannon
   entry), and is not retired.
4. **Recording, exposition and viewing converge.** Per-project recorders and
   viewer implementations are replaced by one stack: producers serve dendro's
   replication stream, `rezolus record` records any of them, and one viewer
   implementation reads the result (metriken entry). `cachecannon view` stays
   as a command and moves onto the shared viewer. The pieces become metriken
   crates (metriken entry, "Structure"): `metriken-types` (the acquisition
   window and the `__uid__` label key, below `metriken-core`),
   `metriken-model` (the data model, with no `metriken` dependency),
   `metriken-storage` (today's `metriken-archive` with `metriken-segment`'s
   tables, this repository's `crates/rez` and the parquet reader and writer),
   the stream route in `metriken-exposition`, `metriken-recorder` (from
   `src/recorder`, with `rezolus record` as its CLI), `metriken-dashboard` and
   `metriken-viewer` (from `crates/dashboard` and the viewer crates; planned
   for after 6.0.0 until decision 7 moved them before it). Applications own
   their dashboard templates, and the viewer loads them from the archive.

## Decisions (2026-10-12)

Taken with the metriken-recorder design (metriken
`docs/journal/2026-10-12-metriken-recorder.md`).

5. **rezolus 6.x writes `.dendro` only.** `record`, hindsight, the live viewer
   (already `.dendro` only), Save as Report in the server and the WASM viewer,
   and every `recording` subcommand that writes a new file produce `.dendro`.
   `.rez`, parquet and raw output are removed from 6.x. Reading `.rez` and
   parquet stays, in `view`, `mcp` and the `recording` subcommands. The
   inventory of writers to remove is under "Removing the `.rez`, parquet and
   raw writers" below. `metriken-recorder` writes `.dendro` only and has no
   sink trait as a consequence.
6. **An agent older than 5.21.0 is refused by 6.x**, with a pointer to a 5.x
   recorder. At startup the run exits 1 and writes nothing; mid-run that
   endpoint is refused, the rest are recorded and finalized, and the run
   exits 1 (`refuse_mid_run`, as today for an agent that cannot stream). This replaces decision 1. rezolus's 5.x line does not adopt
   `metriken-recorder`: it keeps its own recorder and dependencies, and its
   scrape paths keep recording every agent version.
7. **6.0.0 ships after metriken path steps 4 to 7 and the follow-ups listed in
   the plan's step 7.** The steps: `metriken-recorder` (4), templates in the
   archive (5), `metriken-py` (6), `metriken-dashboard` and `metriken-viewer`
   (7). The follow-ups: the stream's frame size, sending only changed
   histogram members or sparse buckets; dendro's compaction keeping
   long-layout metadata; whether the route helper refuses an interval below
   its floor; the histogram query path; the storage scan's gaps. Each
   follow-up is kept only if measured a clear win. The viewer moving into
   metriken crates, out of scope on 2026-10-08, is in scope for 6.0.0, and
   6.0.0 no longer ships "from whichever side holds" the moved crates: all
   of them are on the metriken side by then.

## Plan

### Compatibility (blocks 6.0.0)

No test opens an archive written by a released version. The tests build their
inputs with current code, and the only checked-in files from a release are the
parquet files in `site/viewer/data/`. v5.23.0–v5.25.1 already wrote `.dendro`
on request (`record -o x.dendro`, a hindsight `output` ending in `.dendro`,
`recording upgrade --to dendro`), so such files exist.

- Check in small archives written by released versions under
  `tests/fixtures/`: a tar `.rez` from a 5.17.1 prerelease
  (`v5.17.1-alpha.*`; no stable release wrote tar, since 5.18.0, the first
  release with `.rez`, writes v3 only), a v3 `.rez` from 5.18.0, a v5.25
  `.dendro`, a parquet from a 5.x release before 5.18, and the 6.0 recording
  described above. Store golden query results for each; a test opens each,
  queries it, runs `recording upgrade --to dendro` where it applies, and
  compares. A 5.x `record --stream -o x.rez` (with `caller_rows`) is not
  included: 5.x's stream support is a preview with no compatibility promise
  (decided 2026-10-08), and the identity index it wrote is not carried into
  `metriken-storage`. With decision 5 these fixtures are also the only
  `.rez` and parquet files the read path is tested on that 6.x did not
  build itself with the fixture writers.
- A CI job that runs the v5.25.1 binary against the current agent, the
  current recorder against a v5.25.1 agent, and the v5.25.1 reader on 6.0
  output. The v5.22.1 reader on a 6.0 `.rez` (systemslab's current pin) is
  dropped: 6.0 writes no `.rez`, and 5.22.1 does not read `.dendro`, so
  systemslab's pin must move to a reader that does (5.23.0 or later) before
  it consumes 6.x output (systemslab entry).
- A test that 6.x refuses an agent older than 5.21.0 at startup and mid-run
  (decision 6; metriken-recorder entry, check 3).
- Smaller gaps: `recording convert` is tested on V2 raw snapshots only and
  6.0 agents produce V3; the WASM viewer has no `.dendro` test;
  `tests/viewer_smoke.sh` (A/B tarballs, real combined parquet) runs in no
  workflow; `site/viewer/data/mr2-report.parquet` (a 5.11.0 report) is
  checked in and read by no test; no test upgrades a v1 tar archive.

### Behaviour

- **Endpoint detection.** `record` takes a 2xx answer on `/metrics/binary`
  that decodes as a metriken-exposition `Snapshot` as a Rezolus agent
  (`probe_endpoint`, `src/recorder/mod.rs:334`), and probes at all only when
  the URL's path is `/`. cachecannon serves msgpack there, so a `.dendro` run
  against it tries to stream it and is refused at startup. With `.dendro` the
  only output: stream any source whose `/metrics/stream` handshake opens,
  Rezolus agent or not. A source that answers `/metrics/binary` and
  identifies as a Rezolus agent (`/status`) but cannot stream is refused
  (decision 6); the agent serves no `/metrics` route to scrape. Any other
  source is scraped at `/metrics`. The metriken entry's plan for producers
  depends on this: cachecannon and llm-perf are streamed once they serve the
  route. This is the metriken-recorder entry's step 2, in rezolus's own
  recorder, before the library is extracted.

### Removing the `.rez`, parquet and raw writers (blocks 6.0.0)

Inventory taken at upstream/main 4ffded35. The `.rez` container code (the v3
writer, `parquet_ingest`, `rez_v3_rewrite`, `to_dendro`) is metriken-storage's
(feature `rez`), re-exported by `crates/rez/src/lib.rs`; the rows below are
rezolus's callers of it and its own parquet and raw writers. The fixture
writers behind `test-support` (`rez::rez::recorder_tests_support`,
`rez_stream`) stay, because the read path is still tested on `.rez`.

`record` and hindsight. The metriken-recorder entry's step 2 removes these and
states the refusals: `record -o x.rez`, `.parquet`, `.raw`, `--format` other
than `dendro` and `--separate` are refused with a pointer to a 5.x recorder or
`rezolus recording upgrade --to dendro`; hindsight refuses an `output` ending
in `.rez` or `.parquet` at startup.

| Writer | Where | Writes |
|---|---|---|
| `Format::{Rez, Parquet, Raw}` and their defaults | `src/main.rs:42`, `src/recorder/config.rs:60-124` | output selection |
| `RezStream`'s `Sink::Rez`, `start_rez_recorder`, `build_rez_metadata`, `build_rez_labels` | `src/recorder/mod.rs:1494-2090` | `.rez` v3 through `rez_v3_writer::{RezArchive, StreamRecorderV3}` |
| `build_parquet_converter`, the `Format::Parquet` arms | `src/recorder/mod.rs:1238`, `:3503`, `:3541` | parquet through `MsgpackToParquet` |
| the `Format::Raw` arm | `src/recorder/mod.rs:3476` | raw msgpack |
| `--separate`, `separate_output_path`, `demote_from_rez` | `src/recorder/mod.rs:247`, `:1220`, `:2301` | one parquet or raw file per endpoint |
| hindsight `Writer::Rez` | `src/hindsight/buffer.rs:73-150` | `.rez` v3 buffer |
| hindsight `.rez` dump | `src/hindsight/buffer.rs:485-515` (`vacuum_into`, `copy_range`) | `.rez` dump of a `.rez` buffer |
| hindsight `output` ending `.parquet` | `src/hindsight/mod.rs:180` | warns and writes `.dendro`; becomes a refusal |

Save as Report, server (`src/viewer/actions.rs`, `save_with_selection` from
:751) and WASM (`crates/viewer/src/lib.rs`, `save_with_selection` from
:1018); the builders are in `crates/report-save/src/lib.rs`:

| Source | Builder | Writes today | Call sites |
|---|---|---|---|
| `.dendro` | `build_dendro_report` (:606) | `.dendro` (stays) | via `build_rez_report_from_rez` |
| `.rez` | `build_rez_report_from_rez` (:535) | `.rez` via `rez_v3_rewrite::copy_recordings_into` | `src/viewer/report_save_rez.rs:20`, `actions.rs:863`, `crates/viewer/src/lib.rs:1055` |
| two parquets (compare) | `build_rez_report_from_parquets` (:720) | `.rez` via `parquet_ingest` | `actions.rs:910`, `crates/viewer/src/lib.rs:1088` |
| one parquet | `save_single_parquet` (:183) | parquet | `src/viewer/report_save.rs:21`, `crates/viewer/src/report_save.rs`, `actions.rs:922`, `crates/viewer/src/lib.rs:1099` |

`recording` subcommands (`src/parquet_tools/`):

| Subcommand | Writer | Writes today |
|---|---|---|
| `combine` | `combine_rez_v3` (`combine.rs:344`) | `.rez` from `.rez` inputs |
| `combine` | `combine_parquet_to_rez` (`combine.rs:500`) | `.rez` from parquet inputs |
| `combine` | `combine_and_write` → `write_parquet` (`combine.rs:790`, `:1467`) | row-merged parquet |
| `combine --ab` | `write_ab_tarball` (`combine.rs:859`) | `.parquet.ab.tar` |
| `combine` | `combine_dendro` (`combine.rs:406`) | `.dendro` from `.rez` and `.dendro` inputs (stays); refuses parquet inputs |
| `convert` | `convert_file` with `MsgpackToParquet` (`convert.rs:372`, `:446`) | parquet from raw |
| `filter` | `filter_rez_v3` (`filter.rs:364`), tar inputs upgraded first (`filter.rs:187`) | `.rez`, in place unless `-o` |
| `filter` | `filter_parquet_file` (`filter.rs:80`) | parquet, in place unless `-o` |
| `snapshot` | `snapshot_rez` (`mod.rs:799`) | `.rez` copy of a `.rez` (`vacuum_into`, or a file copy for tar); a `.dendro` input already gives a `.dendro` |
| `upgrade` | `upgrade_rez` (`mod.rs:845`), the default `--to v3` | v3 `.rez` from tar, in place unless `-o` |
| `annotate` | `annotate_rez_any` → `annotate_rez_v3` (`annotate.rs:403`, `:487`), tar upgraded in place first | in-place `.rez` |
| `annotate` | `annotate_parquet`, `annotate_systeminfo`, `set_source_metadata`, `set_node_metadata`, `unannotate_parquet` over `rewrite_parquet` (`annotate.rs:170-865`, `mod.rs:1027`) | in-place parquet |
| `annotate --event`, `check --annotate`, MCP `add_event`, `remove_events`, `run_checks` with `annotate` | `events::{add_events_selected, remove_events_selected}` (`events.rs:200`, `:280`), `write_events` and `append_to_parquet` (`:458`, `:171`), `check::annotate_events` (`check.rs:838`) | in-place `.rez` (annotate path) or parquet |
| MCP `export_query` | `src/mcp/export.rs:50`, `:237` | CSV or parquet of one query's result rows |

Three pieces of work the removal needs and that do not exist today:

- **parquet to `.dendro`.** No path writes a parquet's rows into a `.dendro`:
  `parquet_ingest` writes `.rez`, and `combine` into a `.dendro` refuses
  parquet inputs (`combine.rs:208-216`). Save as Report from a parquet
  source, `combine` and `filter` of parquet inputs, and annotating a parquet
  as a new file all need one.
- **`.rez` to `.dendro` in the WASM viewer.** `to_dendro` is re-exported only
  under `crates/rez`'s `write` feature (`crates/rez/src/lib.rs:14-15`), which
  the WASM viewer turns off. Whether metriken-storage's conversion builds for
  wasm32 without that feature is not checked.
- **raw to `.dendro`.** `convert` writes through `MsgpackToParquet` only.
  metriken-storage's writer stages V1 to V3 snapshots (metriken-recorder
  entry, "Left alone"), so this is a new caller, not new format code.

**Per-subcommand behaviour (decided 2026-10-12).** Decision 5 says which
files are written; these say how each subcommand behaves on an input it can
no longer write back. They were proposed here and accepted as a set, with
`export_query` removed rather than kept.

- **`annotate` on a `.rez` or a parquet** (and `--event`, `check --annotate`,
  MCP `add_event`, `remove_events`, `run_checks` with `annotate`, which share
  its writer): refused, with a pointer to `recording upgrade --to dendro` for
  a `.rez` and to the parquet-to-`.dendro` path for a parquet. 6.x keeps no
  `.rez` or parquet rewriter (`annotate_rez_v3`, `rewrite_parquet` go).
- **`filter` on a `.rez` or a parquet:** writes a `.dendro` at `-o`, which
  becomes required for those inputs (no in-place filter, since the container
  changes).
- **`combine`:** writes `.dendro` only. `.rez` inputs go through
  `combine_dendro`'s existing conversion; parquet inputs go through the new
  parquet-to-`.dendro` path. `--ab` and the row-merged parquet output are
  removed; a 2-source `.dendro` is the A/B form.
- **`convert`:** raw to `.dendro`, default output `rezolus.dendro`.
- **`snapshot` of a `.rez`:** writes a `.dendro` through `upgrade --to
  dendro`'s conversion. Whether `convert_v3_to_dendro` reads a consistent
  snapshot of a `.rez` that a 5.x writer is still appending to (a 5.x
  hindsight buffer) is not checked; if not, a `.rez` input is refused.
- **`upgrade`:** `--to dendro` becomes the default and `--to v3` is removed,
  so a tar `.rez` converts straight to `.dendro`; `-o` is required, as it is
  for `--to dendro` today (`mod.rs:732`).
- **Save as Report:** a `.rez` source converts to a `.dendro` report;
  parquet sources, single and compare, go through the parquet-to-`.dendro`
  path. If the WASM viewer cannot convert, its `.rez` and parquet sources
  offer no report save, with a message pointing at `rezolus view`.
- **MCP `export_query`:** removed, with its `--export-dir` flag. It wrote
  one query's result rows as CSV or parquet.

### Callers that ask `record` for parquet or `.rez` (block 6.0.0)

Each must ask for `.dendro` (and, where it combines files afterwards, combine
`.dendro`) before 6.0.0, because 6.x refuses the other outputs.

- `docker/rezolus-capture` and `scripts/rezolus-capture`: record parquet
  (`docker/rezolus-capture:197-198`, `:246`, `:255`;
  `scripts/rezolus-capture:258-259`, `:314`) and then run `parquet combine`
  (`scripts/rezolus-capture:331`). One `record` with several `--endpoint`
  flags writes one `.dendro` instead.
- `labels/specs/cpu_saturation.toml` and `scheduler_contention.toml`: `-o
  /tmp/recording.rez` and collect `recording.rez` (`cpu_saturation.toml:63`,
  `:73-74`; `scheduler_contention.toml:64`, `:74`), and a comment that any agent
  version serves (`cpu_saturation.toml:57`, `scheduler_contention.toml:58`); with decision 6 an agent older
  than 5.21.0 does not.
- `data/test_fixtures/compare-demo.sh`: records `baseline.parquet` and
  `experiment.parquet` (`:30`, `:37`).
- CI smoke steps in `.github/workflows/cargo.yml`, release (`:330-351`) and
  debug (`:361-382`): `-o rezolus.parquet`, `-o rezolus.rez` and the wrap-mode
  runs to `wrap.parquet`, `false.parquet` and `cap.parquet`.
- llm-perf's server metrics: an `output` not ending in `.dendro` asks for
  parquet (llm-perf `src/server_metrics.rs:22,46-53`, per the
  metriken-recorder entry).
- systemslab's runner: asks a rezolus from 5.19 for `-o metrics.rez`
  (systemslab `crates/runner/src/metrics.rs:238-260`, `MIN_REZ_VERSION` :376,
  per the metriken-recorder entry). Not yet written; iopsystems/systemslab#6371
  covers reading `.dendro`.

### metriken path (blocks 6.0.0)

Decision 7: metriken path steps 4 to 7 released and rezolus on them, and each
follow-up listed in decision 7 either landed or measured and dropped, with the
result recorded in the metriken entry. Step 4 moves `record`, hindsight and
the live viewer onto `metriken-recorder`; step 7 moves `crates/dashboard` and
the viewer crates into `metriken-dashboard` and `metriken-viewer`.

### Measurement

Most numbers behind the format were measured once, while deciding one design
question; the scripts behind several are on a test host and not in any
repository. Before 6.0.0:

- commit the measurement harness (recorder transport comparison, open cost,
  writer replay) to the repository;
- record one agent to `.dendro` with 6.0 and to `.rez` and parquet with a 5.x
  recorder at the same time (6.0 writes neither), at the settings `record`
  uses, at 1 s and 100 ms, with and without per-task series, and compare
  sizes;
- run `hindsight` and `record` on a Linux host for 24 hours and log RSS, CPU,
  file size, free-list size and `-wal` size.

### Documentation

- One 5.x → 6.0 upgrade page. The material is now spread across the
  CHANGELOG, `docs/usage.md` and dendro's `FORMAT.md` and `WIRE.md`. It says
  that 6.x writes `.dendro` only and reads `.rez` and parquet, how to convert
  (`recording upgrade --to dendro`), and that an agent older than 5.21.0
  needs a 5.x recorder or an agent upgrade.
- `site/docs/usage.html` and `site/docs/architecture.html` describe parquet
  and `.rez` and never mention `.dendro`.
- The README embedded in the viewer bundle by `.github/workflows/release.yml`
  says the viewer reads parquet; it also reads `.rez` and `.dendro`.
- The `recording` command's about text names `.parquet` and `.rez` only
  (`src/parquet_tools/mod.rs`).
- `src/mcp/skill/SKILL.md` gives `rezolus record -o out.rez` as the way to
  make a recording.
- `config/hindsight.toml`'s `output`, the path a dump is written to, moves
  from `/var/lib/rezolus/rezolus.rez` (5.25.1) to
  `/var/lib/rezolus/rezolus.dendro`. On a package upgrade an unmodified config
  follows it, and anything that reads dumps at the `.rez` path finds the last
  5.x dump. The upgrade page says so. A modified config that still names a
  `.rez` output is refused at startup under decision 5, and the upgrade page
  says that too. The config's comment on a `.rez` output
  (`config/hindsight.toml:17`) goes.
- `CLAUDE.md`, `docs/usage.md` and `record`'s help text describe `.rez`,
  parquet and raw output, `--separate` and the `.rez` hindsight buffer; they
  change with the writers.

### Distribution

- The Homebrew tap (`iopsystems/homebrew-iop`, `Formula/rezolus.rb`) is at
  5.19.0. Sixteen rezolus bump PRs from two workflows are open and unmerged
  (#134–#151; #135 and #144 in that range are systemslab bumps).
  `install.sh` sends macOS users to the tap.
- Hosts whose agents are older than 5.21 must be upgraded before a 6.0
  recorder can record them; until then a 5.x recorder records them
  (decision 6).

### Release candidate

Run a release candidate's agent, `record` and `hindsight` for a day on a Linux
host and on one fleet host, and read the results with the 5.25.1 and the 6.0
reader.

## GO criteria for tagging 6.0.0

- The fixture tests and the cross-version job pass in CI.
- The endpoint detection fix and the refusal of agents older than 5.21.0
  (decision 6) are merged, with tests.
- No 6.x mode writes `.rez`, parquet or raw: every writer in the inventory
  above is removed or writes `.dendro`, as the per-subcommand decisions
  say.
- Every caller listed under "Callers that ask `record` for parquet or `.rez`"
  records `.dendro`.
- metriken path steps 4 to 7 are released, rezolus is on them, and each
  decision 7 follow-up is landed or recorded as dropped.
- The upgrade page and the documentation fixes above are merged.
- The size comparison and the 24-hour run are recorded here, with the
  harness that produced them in the repository.
- The Homebrew formula is at the release being tagged, or the tap's bump
  automation is fixed so the tag produces one PR that is merged.
- The release candidate ran for a day, and in its recordings no gap between
  consecutive rows of a table is longer than two of that table's measured
  intervals, checked by a query with both readers (groups that skip ticks by
  design are listed here and exempt), and no endpoint was refused other than
  an agent older than 5.21.0.

## Out of scope for 6.0.0

- Segment compaction (dendro `CompactSpec`), and the "20 segments to 1"
  acceptance of #1224's phase 4 that depends on it. The decision 7 follow-up
  on compaction keeping long-layout metadata is in scope; compaction itself
  is not.
- #1224's optional phase 5, an in-agent archive, which overlaps #1144.
- Reading `producer_epochs`, which 6.0 writes and nothing reads yet
  (`2026-10-05-metadata-after-agent-restart.md`).
- Removing the `.rez` writer from metriken-storage. rezolus stops calling it;
  whether storage keeps it for other users is metriken's decision.

Until 2026-10-12 this list began with "Moving the viewer into metriken crates
(metriken entry, path step 7)", and said 6.0.0 ships from whichever side holds
the recorder and `crates/rez` when it is tagged. Decision 7 moved both into
scope.
