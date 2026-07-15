# Measurement uncertainty — per-sampler `.rez` archive (sampler grouping + recorder)

- **Opened:** 2026-07-13
- **Status:** IMPLEMENTED & VALIDATED (sub-projects 1+2). Built in 4 stages
  (metriken `module_path` → rezolus `sampler` label → `.rez` format+writer/reader
  → recorder), live-smoke validated: a 5 s recording produced a `.rez` with 25
  correctly-named per-sampler tables; window-advance dedup gave fast samplers 7
  rows (~1 s cadence) and **drivehealth 1 row** (its 60 s cadence) — heterogeneous
  cadence handled structurally; per-metric `window_begin`/`window_width` offset
  columns present; single-`.parquet` back-compat intact. First of four sub-projects
  that make acquisition windows *usable in analysis* (the arc's Phase 2 archive +
  the windows half of Phase 3, pulled forward). Readers (3) + query/rate-error-bars
  (4) follow.
  - **Format revision LANDED (label-set model, Section 2)** — commits
    `f895c057` (rez core: nested recordings + label helpers) and `74afc5d8`
    (`--label` + `build_rez_labels` + wiring). The initial build used a flat
    `<sampler>.parquet` layout with one file-level metadata map. A shakedown
    surfaced that A/B — especially *multi-host* A/B (client+server
    Valkey-vs-Redis) — has arbitrary, varying grouping dimensions no fixed
    hierarchy holds. So `.rez` is now a **bag of label-tagged recordings**
    (`<recording-dir>/<sampler>.parquet` + a manifest of `recordings[]` with a
    label map + per-recording metadata). The per-sampler parquet tables (window
    columns, dedup) are unchanged. Re-validated live (2026-07-15): a 5 s labeled
    recording (`--label arm=baseline`) produced a `.rez` whose tar nests
    `manifest.json` + `rezolus/<sampler>.parquet`, `recordings[0].labels =
    {arm: baseline, host: delta (from systeminfo), source: rezolus}`, 25 tables,
    and window-advance dedup still gives drivehealth 1 row vs fast samplers 7.
  - **Grounded review of the flat build → label-set delta** (verified against
    `src/recorder/rez.rs` and `src/recorder/mod.rs` on 2026-07-14). The per-metric
    parquet layer needs **no change**: `table_to_batch`/`write_table_parquet`/
    `read_table_parquet` (window offset columns, `:buckets`, `metric_type` keying)
    and the whole `TableBuilder`/`RezRecorder::ingest` dedup path are correct as
    built. The revision is confined to the container + label step:
    - `RezManifest` (`rez.rs:15`) is flat: `{ version, metadata: BTreeMap, tables:
      Vec<RezTableIndex> }`. Target nests one level: `{ version, recordings:
      [ { dir, labels: BTreeMap, metadata: BTreeMap, tables: Vec<RezTableIndex> } ] }`.
      `RezTableIndex` (`rez.rs:26`) is unchanged.
    - `write_archive` (`rez.rs:363`) writes `<sampler>.parquet` at the tar root and
      one flat manifest; target writes `<dir>/<sampler>.parquet` and a
      `recordings[0]` entry. `read_archive` (`rez.rs:401`) resolves `idx.file` at
      root; target joins `<dir>/`. `RezArchive` (`rez.rs:343`) stays a decoded
      view but its manifest gains the recording nesting.
    - `RezRecorder::new`/`finalize` (`rez.rs:575`,`650`) take a single `metadata`
      map; target also takes the recording's **label set** (a `BTreeMap`) and a
      `dir` slug, threaded into the manifest's `recordings[0]`.
    - `build_rez_metadata` (`mod.rs:399`) builds today's flat metadata from
      `source`/`--metadata`/`systeminfo`/`descriptions`; target adds a sibling
      `build_rez_labels` producing `{ source, host (from systeminfo hostname),
      + --label k=v }`. A repeatable `--label` clap arg (`mod.rs` arg table) and a
      slug helper are new. The single-endpoint guard (`mod.rs:613`) stays.
    - Tests to migrate: `manifest_tests` (`rez.rs:797`), `archive_tests`
      (`rez.rs:915`), `finalize_tests` (`rez.rs:1002`) assert the flat manifest
      (`archive.manifest.metadata`, `archive.manifest.tables`) — they move to
      `recordings[0]`; a new test covers label-building + `--label`. The
      per-table `table_tests` (`rez.rs:823`) and `recorder_tests` (`rez.rs:670`)
      are untouched.
- **Arc:** [measurement uncertainty](2026-07-08-measurement-uncertainty.md);
  consumes the per-metric windows built in
  [all-sampler observation windows](2026-07-10-all-sampler-observation-windows.md)
  (metriken `next` + rezolus, hardware-validated).
- **Owner:** Brian Martin
- **Repos:** metriken (`~/workspace/metriken`, `next`) for the `#[metric]`
  `module_path` addition; rezolus for the exposition `sampler` label, the `.rez`
  archive format, and the recorder.

This entry is the design spec (absorbs the brainstorm).

## Why

Every metric now carries an honest per-observation acquisition window through the
agent exposition — but that window is **dropped at every downstream layer**:
parquet has no window columns, the TSDB sample is a bare `(timestamp, value)`,
`rate()` returns a scalar with no uncertainty, and the viewer draws scalar lines.
So the windows are currently unusable for analysis.

Making them usable spans five layers (parquet, TSDB, query result, `rate()`,
viewer) — genuinely several sub-projects. The **root problem** a design pass
surfaced: the recording format forces all metrics onto **one shared timeline**
(the scrape cadence), which is fundamentally mismatched with **heterogeneous
sampler cadences** (drivehealth reads every 60 s; BPF every scrape). Under that
model a slow sampler's value+window are *retained* — repeated across every row —
and the window is a patch carrying the truth despite the wrong model.

**Decision: fix the model at the source with per-sampler tables** (pull the arc's
Phase 2 `.rez` archive forward) rather than patch the single wide file. Each
sampler gets its own table at its own cadence: no retained redundancy, and
**observation-identity is structural** — every row *is* a distinct read, so the
downstream `rate()` needs no window-dedup logic; the per-row window just gives the
error bar.

## Scope (this sub-project = 1 + 2 of 4)

Making windows usable decomposes into four coupled sub-projects; this spec covers
the first two, which are tightly bound (the recorder partitions by the grouping
label):

1. **Sampler grouping** — a universal `sampler` label on every metric.
2. **`.rez` archive + recorder** — the per-sampler-table container and the
   recorder that writes it.

Deferred to their own specs: **(3) reader ecosystem** (viewer file-loading, MCP,
`parquet` tools learn `.rez`); **(4) metriken-query** (multi-table ingest +
cross-timeline alignment + `rate()`/`increase()` **interval-arithmetic** error
bars + correlation ceiling). The viewer rendering (error bands, correlation
indicator) is a later round again.

Recorded design decision for (4), settled during this brainstorm so (1)+(2) are
sized for it: `rate()` error bars use **interval arithmetic** — hard bounds
`rate ∈ [Δv/(e₂−b₁), Δv/(b₂−e₁)]` from the two observations' window edges, no
distributional assumptions. This is why per-sampler tables carry each row's full
`begin`/`end`, not just a width.

## Section 1 — Sampler grouping

The recorder is a **separate process**: it scrapes the agent's msgpack endpoint,
so it has no sampler registry and can only group by what is **in the snapshot
metadata**. So the grouping surfaces as a **`sampler = "<name>"` label** on each
metric's exposition metadata.

Populate it automatically via the **module-path attribution** designed (and
shelved) during the windowing effort — it was unnecessary for windows (each
sampler stamps its own) but is exactly right here, with no torn-safety
entanglement:

- **metriken** (`next`): `#[metric]` records `module_path!()` at the definition
  site; `metriken-core`'s `MetricEntry` stores it + a `module()` accessor. Small,
  additive.
- **rezolus** (agent exposition): map each metric to its sampler by **longest
  module-prefix** against the registered sampler modules, and emit a
  `sampler = "<name>"` metadata label on the exposition entry. Metrics that
  already hand-set `metadata = { sampler = … }` are subsumed (the automatic value
  wins / matches).

A metric with no sampler attribution (should be none for agent metrics; external
metrics carry `source = external`) falls into an `unattributed` table rather than
being dropped.

## Section 2 — the `.rez` archive format (label-set model)

A `.rez` is a **bag of label-tagged recordings**, not a fixed hierarchy. This is
the label-native model observability already lives in, and it's forced by the fact
that the grouping dimensions vary per experiment: a single recording has none, a
2-way A/B has one (`arm`), a client/server A/B has two (`arm` + `role`), a
cross-host fleet has one (`host`), and each of those can also carry multiple
sources. No fixed `<cohort>/<source>/<sampler>` depth survives that; **labels do**.

- The atomic unit is a **recording** = one rezolus endpoint on one host =
  `{labels}` + its per-sampler tables.
- **Labels** are arbitrary key/values: `source` (the endpoint's source),
  `host` (default from `systeminfo`), plus any user-assigned `--label k=v`. A
  client/server Valkey-vs-Redis compare is four recordings tagged
  `{arm: redis|valkey, role: server|client, host: …}`. `baseline`/`experiment`
  cease to be special — they're just `role`/`arm` labels.
- **Grouping is faceting over labels, downstream** (compare `arm`, correlate
  `role=client` vs `role=server` within `arm=redis`, overlay all `host=*`) — never
  baked into folders.

`<recording>.rez` is an **uncompressed tar** (parquet compresses internally):

- **`manifest.json`** — `{ version, recordings: [ { dir, labels: {k:v},
  metadata: { systeminfo, descriptions, sampling_interval_ms, … }, tables:
  [ { sampler, file, columns, rows, cadence_ns } ] } ] }`. Metadata is **per
  recording** (each recording has its own host/systeminfo/descriptions), reusing
  the `per_source_metadata` shape. The manifest is authoritative for labels.
- **per-recording directories** — `<dir>/<sampler>.parquet`, where `<dir>` is a
  filesystem-safe slug of the recording's distinguishing labels (human-readable;
  the manifest carries the real label set). Today's recorder writes exactly **one**
  recording; bundling multiple recordings (the actual A/B / cross-host assembly)
  is the reader/query sub-project.

Each per-sampler parquet (unchanged from the earlier draft): rows are that
sampler's **observations** (one per window-advance → its real cadence); columns are
`timestamp`, the metric columns, and **per-metric window columns** (`begin`/`width`
as offsets from the row `timestamp`, null for level-4). No `duration` column — the
per-metric window is the honest per-observation duration. Windows vary per row
honestly (drivehealth's per-drive stagger, the `CpuCounters` sweep), no cross-time
redundancy.

**Output selection** is by the recorder's output-path extension: `record <url>
out.rez` writes the archive; `out.parquet` writes the current single wide file
(back-compat, unchanged). Existing single-`.parquet` files keep reading — readers
accept both.

## Section 3 — Recorder mechanics

A single `record` invocation produces **one recording** (one endpoint on one
host). The recorder builds that recording's **label set** once at startup:

- `source` — the endpoint's source (from the snapshot metadata; `rezolus` for a
  live agent).
- `host` — default from `systeminfo` (hostname), so cross-host assembly works
  without user effort.
- any `--label k=v` the user passes (repeatable) — the escape hatch for `arm`,
  `role`, or any experiment-specific dimension.

These labels are written verbatim into the manifest's single `recordings[0]`
entry. Assembling **multiple** recordings into one `.rez` (the actual A/B /
cross-host bundle) is the reader/query sub-project — the recorder only ever emits
a one-recording archive.

Per snapshot (scraped at the fast interval), partition metrics by the `sampler`
label, then for each sampler decide whether a **new observation** occurred:

- **Observation key = the sampler's representative window** (max `end_ns` among
  its windowed metrics — a sampler brackets one read per refresh, so its metrics
  share ~one window). Key **advanced** → append a row to that sampler's table;
  **unchanged** (slow sampler between reads) → **skip**. This is the entire dedup:
  drivehealth → 1 row/60 s, BPF → 1 row/scrape, for free.
- **Fully level-4 samplers** (all metrics windowless — e.g. `cpu/perf`, all-packed
  groups) have no window to key on; they are read live every scrape, so their
  observation key is the **snapshot timestamp** → one row per poll (their real
  cadence). **Mixed samplers** (windowed + packed, e.g. `cpu/usage`) advance on
  refresh via their windowed metrics and carry the packed columns in the same row.
- **Write path:** per-sampler parquet written to a temp dir as rows accumulate; at
  recording finalize (stop / `--duration` end) assemble `manifest.json` and **tar**
  the tables into `<name>.rez`.

## Section 4 — Round-trip read + testing

Read scope here is the recorder's own **round-trip** (write `.rez` → read back →
tables/rows/windows match). The full reader ecosystem is sub-project (3); old
single-`.parquet` keeps reading via the existing path.

- **Sampler label:** module-prefix → `sampler` mapping (unit test with fixture
  module strings); a snapshot metric carries `sampler=X`; an unattributed metric
  → `unattributed`.
- **Partitioning:** a 2-sampler snapshot → 2 tables with the right column split.
- **Window-advance dedup:** window unchanged across N polls → 1 row; window
  advancing each poll → N rows; a fully-level-4 sampler → per-poll rows; a mixed
  sampler → rows on refresh with packed columns present.
- **`.rez` round-trip:** write → read back → per-sampler tables, row counts, and
  per-metric `(begin,end)` windows match (including per-row window variation).
- **Back-compat:** an existing single-`.parquet` still reads unchanged.

## Fit with the arc / principles

- Consumes the Phase-1b windows directly; the window becomes the **observation
  identity** (dedup) and the **error-bar input** (interval arithmetic in (4)).
- **Principle 10** preserved — no agent-side clock semantics; the recorder keys on
  windows the samplers already recorded.
- The **module-path attribution** returns for its natural use (metric→sampler
  grouping), where it carries no torn-safety cost — a clean reuse of shelved work.
- Sets up cross-host (Phase 4): per-cohort/per-host `.rez` archives compose (a
  cohort is a set of sampler tables); NTP root dispersion joins the correlation
  ceiling later.

## Open questions / spec-time details

- **Recording-dir slug** — the per-recording directory name is a
  filesystem-safe slug of the recording's *distinguishing* labels. With one
  recording there is nothing to distinguish, so a single recording can use a
  fixed dir (e.g. `recording/` or a `source`-based slug); the multi-recording
  assembler (reader sub-project) is what actually needs a collision-free slug
  scheme. The manifest — not the dir name — is authoritative for labels.
- **`--label` CLI surface** — a repeatable `--label k=v` on `record`, parsed to
  the recording's label map. Reserved keys `source`/`host` are auto-populated;
  a user `--label` may override `host` (e.g. a friendly name) but collisions
  should be defined (last-wins vs error) at implementation time.
- **Streaming vs finalize tar** — writing per-sampler parquet incrementally then
  tarring at finalize (chosen) vs a streamable container; finalize is simpler and
  matches `record --duration`.
- **`unattributed` bucket** — confirm no agent metric lands there once module
  attribution is on; external metrics table by `source`.
- **Very-slow / never-advancing samplers** in a short recording — a sampler whose
  window never advances during the recording writes **zero rows** (honest: it was
  never re-read); confirm downstream readers tolerate an empty/absent table.
