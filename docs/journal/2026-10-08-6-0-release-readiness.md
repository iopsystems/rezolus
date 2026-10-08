# Rezolus 6.0.0: release readiness

**Status: OPEN.** Plan opened 2026-10-08. `Cargo.toml` is at
`6.0.0-alpha.41`; no `v6.*` tag exists and the latest release is v5.25.1. This
entry lists what has to be true before 6.0.0 is tagged and in what order,
and records the decisions taken on 2026-10-08. The parent issue is #1224.

Related entries in other repositories, opened the same day:

- metriken `docs/journal/2026-10-08-one-recording-stack.md`: one recording,
  exposition and viewing stack for every metriken producer, and readers for
  other languages.
- systemslab `docs/journal/2026-10-08-dendro-artifacts.md`: reading and then
  recording `.dendro` artifacts.
- cachecannon `docs/journal/2026-10-08-dendro-recording.md` and llm-perf
  `docs/journal/2026-10-08-dendro-recording.md`: getting each load generator
  into the same archive as the agent.

## Where 6.0 stands

Every mode that writes an archive defaults to `.dendro`: `record`, the
`hindsight` buffer and its dumps, the live viewer's temporary archive, and
Save as Report from a dendro source. `view` (server and WASM), `mcp` and
every `recording` subcommand that reads an archive reads `.dendro` and `.rez` through `RezReader`,
which wraps metriken-archive's `ArchiveReader`. Parquet is read through
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

1. **An agent that cannot stream is recorded to `.rez`, with a warning.** A
   `record` run whose output was not named (no `-o`, no `--format`) and whose
   agent predates the stream (older than `STREAM_SINCE`, 5.21.0,
   `src/recorder/mod.rs:925`) writes `.rez` through the scrape path and logs a
   warning, as `demote_from_rez` (`src/recorder/mod.rs:2295`) already demotes
   a defaulted `--separate` run to parquet. A run writes one archive, so a
   defaulted run with any such agent among its endpoints writes `.rez` as a
   whole. An explicit `-o x.dendro` still refuses. `.rez` stays writable for as
   long as agents older than 5.21 are in use.
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
   crates: `metriken-storage` (today's `metriken-archive`), the stream's
   producer side (a `stream` feature of `metriken-exposition` is recommended,
   a `metriken-streaming` crate is the alternative), `metriken-recorder` (from
   `src/recorder`, with `rezolus record` as its CLI), `metriken-dashboard` and
   `metriken-viewer` (from `crates/dashboard` and the viewer crates, after
   6.0.0). Applications own their dashboard
   templates, and the viewer loads them from the archive.

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
  release with `.rez`, writes v3 only), a v3 `.rez` from 5.18.0, a 5.2x
  `record --stream -o x.rez` (with `caller_rows`), a v5.25 `.dendro`, a
  parquet from a 5.x release before 5.18, and the 6.0 recording described
  above. Store golden query results for each; a test
  opens each, queries it, runs `recording upgrade --to dendro` where it
  applies, and compares.
- A CI job that runs the v5.25.1 binary against the current agent, the
  current recorder against a v5.25.1 agent, the v5.25.1 reader on 6.0 output,
  and the v5.22.1 reader on a 6.0 `.rez` (systemslab's current pin).
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
  against it tries to stream it and is refused at startup. For `.dendro`
  output: stream any source whose `/metrics/stream` handshake opens, Rezolus
  agent or not. A source that answers `/metrics/binary` and identifies as a
  Rezolus agent (`/status`) but cannot stream is handled by decision 1; the
  agent serves no `/metrics` route to scrape. Any other source is scraped at
  `/metrics`. `.rez` and parquet output keep scraping `/metrics/binary`. The
  metriken entry's plan for producers depends on this: cachecannon and
  llm-perf are streamed once they serve the route.
- **Decision 1** above.

### Measurement

Most numbers behind the format were measured once, while deciding one design
question; the scripts behind several are on a test host and not in any
repository. Before 6.0.0:

- commit the measurement harness (recorder transport comparison, open cost,
  writer replay) to the repository;
- record one agent to `.dendro`, `.rez` and parquet at the same time, at the
  settings `record` uses, at 1 s and 100 ms, with and without per-task
  series, and compare sizes;
- run `hindsight` and `record` on a Linux host for 24 hours and log RSS, CPU,
  file size, free-list size and `-wal` size.

### Documentation

- One 5.x → 6.0 upgrade page. The material is now spread across the
  CHANGELOG, `docs/usage.md` and dendro's `FORMAT.md` and `WIRE.md`.
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
  5.x dump. The upgrade page says so.

### Distribution

- The Homebrew tap (`iopsystems/homebrew-iop`, `Formula/rezolus.rb`) is at
  5.19.0. Sixteen rezolus bump PRs from two workflows are open and unmerged
  (#134–#151; #135 and #144 in that range are systemslab bumps).
  `install.sh` sends macOS users to the tap.
- Hosts whose agents are older than 5.21 must be upgraded before a 6.0
  recorder can stream from them; until then decision 1 keeps them recordable.

### Release candidate

Run a release candidate's agent, `record` and `hindsight` for a day on a Linux
host and on one fleet host, and read the results with the 5.25.1 and the 6.0
reader.

## GO criteria for tagging 6.0.0

- The fixture tests and the cross-version job pass in CI.
- Decision 1 and the endpoint detection fix are merged.
- The upgrade page and the documentation fixes above are merged.
- The size comparison and the 24-hour run are recorded here, with the
  harness that produced them in the repository.
- The Homebrew formula is at the release being tagged, or the tap's bump
  automation is fixed so the tag produces one PR that is merged.
- The release candidate ran for a day, and in its recordings no gap between
  consecutive rows of a table is longer than two of that table's measured
  intervals, checked by a query with both readers (groups that skip ticks by
  design are listed here and exempt), and no endpoint was refused that
  decision 1 does not explain.

## Out of scope for 6.0.0

- Moving the recorder and the viewer into metriken crates (metriken entry).
  6.0.0 ships them from this repository.
- Segment compaction (dendro `CompactSpec`), and the "20 segments to 1"
  acceptance of #1224's phase 4 that depends on it.
- #1224's optional phase 5, an in-agent archive, which overlaps #1144.
- Reading `producer_epochs`, which 6.0 writes and nothing reads yet
  (`2026-10-05-metadata-after-agent-restart.md`).
