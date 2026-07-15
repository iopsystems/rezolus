# Measurement uncertainty — `.rez` reader ecosystem (viewer / MCP / parquet-tools)

- **Opened:** 2026-07-15
- **Status:** Open — design landed, pre-build. Sub-project (3) of the arc: make
  `.rez` archives *readable* everywhere a `.parquet` is today. Consumes the
  per-sampler `.rez` format (labels + per-sampler tables + per-metric windows)
  built in [per-sampler `.rez` archive](2026-07-13-per-sampler-rez-archive.md).
  Phased A → B → C; each phase ships working software.
- **Arc:** [measurement uncertainty](2026-07-08-measurement-uncertainty.md).
- **Owner:** Brian Martin
- **Repos:** metriken (`~/workspace/metriken`, `next`) for the query-engine
  window-sidecar change; rezolus for `RezReader`, detection/wiring, and the
  `parquet` tools.

This entry is the design spec (absorbs the brainstorm).

## Why

The `.rez` format exists and the recorder writes it, but **nothing reads it**.
Every consumer that opens a `.parquet` today — the viewer (file + upload), the
MCP tools, and the `parquet` subcommands — is oblivious to `.rez`. Until they
read it, the per-sampler tables and per-metric windows are write-only.

A design pass over the consumers (2026-07-15) found the ingest surface has
**two boundaries**, not one:

- **Query workload** (viewer, MCP): everything funnels through
  `metriken_query::ParquetReader::open_with_pool(path, pool) -> impl MetricsSource`
  (`MetricsSource` trait at `metriken-query/src/lib.rs:113`). Viewer hooks:
  `src/viewer/mod.rs:418/427` (file mode), `src/viewer/actions.rs:240/341`
  (upload). MCP: `src/mcp/mod.rs` (CLI commands, `ParquetReader::open`) and
  `src/mcp/server.rs:514` (`get_reader`, cached).
- **Transform workload** (`parquet` tools): arrow-direct
  `ParquetRecordBatchReaderBuilder` (`src/parquet_tools/combine.rs`,
  `src/parquet_tools/mod.rs:378/399`), plus `ParquetReader` for validation only
  in `annotate` (`src/parquet_tools/annotate.rs:177`).

Format is detected today by **magic bytes**, not extension:
`looks_like_ab_tarball()` (`src/viewer/ab_extract.rs:35`) sniffs `PAR1` (bare
parquet) vs `ustar` (tar), then extracts an A/B tarball's `baseline`/`experiment`
parquets. `.rez` is also a `ustar` tar, so detection must distinguish them.

## The core problem the design solves

A `.rez` holds **N per-sampler tables, each on its own timeline** (drivehealth
60 s, BPF 1 s) — and finer still, **each metric carries its own per-row window**
(drive-to-drive stagger within drivehealth). The query boundary
(`ParquetReader`) reads **one wide single-timeline table**. So a `.rez` cannot be
handed to `ParquetReader` directly.

**Decision: a union `MetricsSource`, not a melted store.** `RezReader` composes
**N separate `ParquetReader`s** (one per per-sampler table) behind one
`MetricsSource`. The alternative — melting all tables into one `MemoryStore`
(`metriken-query/src/memory.rs`, which already supports per-series timestamps) —
would make cross-table PromQL "just work" but does cross-timeline alignment
**implicitly and eagerly**. The whole point of `.rez` is that alignment
(interpolate vs decimate) is a **deliberate** choice; keeping the per-sampler
readers as distinct logical boundaries preserves the seam where that choice is
made (sub-project 4). Union it is.

### The window-sidecar snag (why Phase A exists)

`parse_schema` (`metriken-query/src/parquet.rs:1210`) classifies every column by
Arrow type: `UInt64`→counter, `Int64`→gauge, `List<UInt64>`+powers→histogram,
else skip. The `.rez` per-metric window columns are exactly those types with
**no `metric` metadata**:

- `<m>:window_begin` (Int64) → misclassified as a phantom **gauge**
  `"<m>:window_begin"`.
- `<m>:window_width` (UInt64) → misclassified as a phantom **counter**.

So opening a per-sampler table with today's `ParquetReader` pollutes the metric
list with phantom sidecar "metrics". `parse_schema` must learn that
`:window_begin`/`:window_width` are **sidecars of their base metric, not
metrics**. (Histograms are already fine — `:buckets` carries `metric` +
`grouping_power`/`max_value_power`, and metriken-query already uses the
`:buckets` suffix convention at `memory.rs:353` / `parquet.rs:1224`.)

## Scope (sub-project 3 of 4) and phasing

Three phases, each its own plan producing working, testable software.

### Phase A — metriken-query window-sidecar awareness (foundation)

`parse_schema` recognizes `<m>:window_begin` / `<m>:window_width` as sidecar
columns and **skips them** (returns no `ColDesc`), so they never surface as
phantom metrics. Recognition is by column-name suffix; the base metric `<m>` is
the column name with the suffix stripped. This is additive and the natural seam
where windows enter the query engine — sub-project (4) changes "skip" to "read
into the series' per-sample window" for rate error bars.

- **Test:** a fixture parquet with a counter column plus its `:window_begin` /
  `:window_width` sidecars → `counter_names()` contains the base metric and
  **not** the sidecars; the sidecar columns are absent from all name/label
  listings.
- **Lands in:** metriken `next`. rezolus builds against it via the uncommitted
  `[patch.crates-io]` override (never committed).

### Phase B — single-recording read path

Deliver "record a `.rez`, then view/query it."

- **`RezReader: MetricsSource`** (new, rezolus). Given a `.rez` path: read
  `manifest.json`, extract each `<dir>/<sampler>.parquet` (to a tempdir or via
  the in-tar reader), and open each as a `metriken_query::ParquetReader`
  (buffer-pool shared). Implement `MetricsSource`:
  - **Metadata / naming / labels** (`counter_names`, `gauge_names`,
    `histogram_names`, `*_labels`, `all_names`, label helpers): **union** across
    sub-readers. Metric namespaces are disjoint by construction (partitioned by
    sampler), so the union is conflict-free and a metric maps to exactly one
    sub-reader.
  - **`query` / `query_range` / `columns`**: resolve the referenced metrics (via
    `columns()` on each sub-reader, or the disjoint name→reader map), and if they
    all live in **one** sub-reader, delegate to it. If they span **two or more**
    sub-readers, return a **clear error** naming the samplers
    (`"cross-timeline query spans samplers X, Y — alignment lands in a later
    phase"`). No silent nearest-sample join — that would reintroduce the
    heterogeneous-cadence dishonesty `.rez` exists to prevent.
  - **`time_range` / `time_range_ns`**: union (min start, max end).
    **`interval`**: the minimum (finest) sub-reader interval — it feeds step
    defaults. **`source` / `version` / `file_metadata` / `metadata_get`**: from
    the recording's manifest `metadata` (single recording in Phase B).
    **`filename`**: the `.rez` basename.
- **Detection** (`is_rez`): a byte/tar sniffer — `ustar` magic **and** a
  `manifest.json` member — distinguishing `.rez` from the A/B tarball (whose
  parquets sit at the tar root with no `manifest.json`). Extend the
  `looks_like_ab_tarball` dispatch rather than duplicate it.
- **Wiring:** viewer file mode (`src/viewer/mod.rs:418`), viewer upload
  (`src/viewer/actions.rs:240`), MCP server `get_reader` (`src/mcp/server.rs:514`),
  and the MCP CLI commands (`src/mcp/mod.rs`) each gain a `.rez` branch that
  builds a `RezReader` instead of a bare `ParquetReader`. Because `RezReader`
  *is* a `MetricsSource`, everything downstream (PromQL, dashboards, describe)
  is unchanged.
- **`parquet metadata`** learns to describe a `.rez`: list the recording(s),
  their labels, and per-sampler tables (sampler, rows, cadence) — reading the
  manifest, not the wide-file footer.

### Phase C — multi-recording assembly, faceting, and transforms

Deliver the multi-host A/B payoff (record per host → combine → view faceted).

- **`parquet combine`** assembles single-recording `.rez` files into one
  **multi-recording** `.rez` (each input becomes a `recordings[]` entry; labels
  preserved / augmentable via flags, mirroring today's `--ab` naming). This is
  the label-set model's assembly path — the multi-host, multi-arm archive.
  - **`dir` uniqueness is combine's responsibility.** `read_archive` keys each
    table by `<dir>/<sampler>.parquet` and holds the extracted parquet bytes in a
    map, so two recordings that slug to the **same `dir`** (e.g. two `rezolus`
    sources, or `"a/b"` and `"a-b"` both → `"a-b"`) would collide: the tar writes
    two identical paths and the reader silently overwrites the first, then fails
    `missing table file`. `combine` must therefore assign a **collision-free
    `dir`** per recording (disambiguate by the distinguishing labels, appending a
    counter as a last resort) and error rather than emit a colliding archive. The
    recorder never hits this (it writes exactly one recording); the writer/reader
    already support N recordings and are covered by
    `archive_round_trips_multiple_recordings` (`src/recorder/rez.rs`), which
    round-trips two distinct-`dir` recordings sharing a sampler basename.
- **`RezReader` reads multiple recordings**: it already carries a `Vec` of
  recordings-worth of sub-readers; Phase C exposes them for **label faceting**
  (compare `arm`, overlay `host=*`), reusing the viewer's existing A/B-compare
  machinery (`src/viewer/ab_extract.rs`, compare mode) generalized from two fixed
  captures to N label-tagged recordings.
- **`parquet filter`** slims a `.rez` — drop per-sampler tables / columns not
  needed by KPIs, per recording, rewriting the manifest.
- **`parquet annotate`** attaches `ServiceExtension` KPIs to a chosen recording's
  metadata (`service_queries`), per recording.

## Round-trip / testing

- **Phase A:** unit test in metriken-query (fixture parquet with sidecars →
  no phantom metrics).
- **Phase B:** unit — `RezReader` union over ≥2 fixture tables (names/labels are
  the union; a single-table query delegates; a cross-table query errors with
  both sampler names). Integration — build a real `.rez` (the recorder path;
  a live 5 s capture like the format smoke) and assert: `all_names()` excludes
  `:window_*` sidecars, a per-sampler `query_range` returns data, and the viewer
  loads it. Detection — `is_rez` true for a `.rez`, false for a bare parquet and
  for an A/B tarball.
- **Phase C:** `combine` two single-recording `.rez` → a 2-recording `.rez`
  whose manifest lists both with their labels; `RezReader` reads both; the viewer
  facets by label; `filter` drops a table and the manifest reflects it;
  `annotate` adds a KPI to one recording.

## Fit with the arc / principles

- Consumes the `.rez` windows: Phase A is the **entry point of windows into the
  query engine** — sub-project (4) grows the rate/`increase()` interval-arithmetic
  error bars and correlation ceiling from the same `parse_schema` seam.
- The **union boundary** keeps cross-timeline alignment an explicit, deliberate
  operation (interpolate vs decimate), never an implicit ingest side effect —
  consistent with the format's reason to exist.
- **Principle 10** (no agent-side clock semantics) is untouched — this is all
  read-side; the recorder already stamped the windows the reader surfaces.
- Phase C realizes the label-set model's purpose (multi-host / multi-arm A/B as a
  bag of label-tagged recordings), setting up the cross-host correlation work
  later in the arc.

## Open questions / spec-time details

- **In-tar vs tempdir extraction** — whether `RezReader` extracts each table to a
  tempdir (simple; `ParquetReader::open` takes a path) or reads table bytes from
  the tar and opens a bytes-backed `ParquetReader`. Tempdir is the low-risk
  default (matches how the A/B tarball path already extracts); revisit if temp
  I/O is a problem for upload mode.
- **`interval()` for heterogeneous cadence** — min (finest) chosen; confirm no
  consumer assumes a single true interval (viewer step defaults should tolerate
  per-panel cadence).
- **Cross-table error surfacing in the viewer** — a `MetricsSource` error from a
  faceted/derived panel must render as a legible message, not a blank chart;
  confirm the viewer's query-error path (Phase B).
- **`combine` label augmentation** — exact flag surface for assigning/overriding
  labels when assembling recordings (mirror `--ab baseline=… experiment=…` vs a
  general `--label`), settled in the Phase C plan.
- **Multi-recording `interval`/`time_range`** — across recordings that may not
  share a clock (different hosts); Phase C confirms union semantics and defers
  true cross-host clock reconciliation (NTP root dispersion) to the arc's later
  correlation-ceiling work.
