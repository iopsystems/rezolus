# Design: viewer support for "simple capture" parquet

**Date:** 2026-07-03
**Status:** Approved, pre-implementation

## Problem

`rezolus view` today assumes a recording is either a Rezolus-agent capture or a
source with a pre-registered service-extension template. A **simple capture** —
a parquet with perfectly legal metric columns (counter/gauge/histogram + the
`metric_type` and labels in column metadata) that is neither from a Rezolus agent
nor covered by a template — renders badly: the 13 built-in Rezolus sections show
up **empty**, there is no source-appropriate landing page, and the only way to
see the metrics is free-form PromQL in the Query Explorer. Unmapped metrics are
otherwise dropped from the UI.

Goal: make the viewer a useful, self-explanatory tool for any legal metrics
parquet, without a template and without pretending it is a Rezolus recording.

## Two coupled deliverables

1. **Robust per-source detection** of whether Rezolus samplers are present, and
   correct service-name resolution for the default Rezolus agent source.
2. **A generic per-source view** for any source with no samplers and no template:
   a `source: <name>` section holding a selectable table of that source's metrics
   + key metadata, with type-appropriate visualization of the metrics you select.
   Decisions are made per source, so unrecognized sources coexist with Rezolus
   sections in the same file.

## Current architecture (anchors)

- Parquet metadata keys: `src/parquet_metadata.rs` (`source`,
  `per_source_metadata` → `sampler_status`/`service_queries`/`version`/`node`,
  `systeminfo`, `descriptions`).
- Source/template extraction: `src/viewer/metadata.rs`
  `extract_service_extension_metadata()` and `regenerate_dashboards()`.
- Dashboard/nav build: `crates/dashboard/src/dashboard/mod.rs`
  (`SECTION_META` static = the 13 built-ins; `build_dashboard_context()`;
  `generate_section()`). Generators take `&dyn MetricsSource`.
- TSDB catalog surface (already sufficient): `metriken-query`
  `counter_names()`/`gauge_names()`/`histogram_names()` (name **and** type),
  `counter_labels(name)`/`gauge_labels(name)`/`histogram_labels(name)` (series +
  label keys), plus `MetricsSource::label_values()` / `total_series_count()`.
- Metric-type → chart style: `src/viewer/assets/lib/charts/metric_types.js`
  `resolveStyle()` (gauge/counter → line/heatmap/multi; histogram →
  scatter/histogram_heatmap/quantile_heatmap) and `data.js` `applyResultToPlot()`.
- HTTP surface: `src/viewer/routes.rs` (`/api/v1/sections`, `/data/{s}.json`,
  `/api/v1/query_range`, `/api/v1/metadata`, `/api/v1/mode`, …).

**Today's gap:** there is no explicit "is this Rezolus?" check — it is inferred
loosely from `source=="rezolus"` + `sampler_status`/`systeminfo`. Sources that
match nothing contribute no sections; their metrics are dropped from the UI.

## Section A — Detection + naming

Introduce a small, testable classifier producing a per-source `SourceKind`:

```
enum SourceKind { Rezolus, Service, Simple }
fn detect_source_kind(source, metadata, columns) -> SourceKind
```

Layered:
- **Tier 1 — metadata markers.** `Rezolus` if `source == "rezolus"` OR the
  source has `sampler_status`/`systeminfo` under `per_source_metadata`.
  `Service` if a `service_queries` block or a `TemplateRegistry` entry matches
  the source.
- **Tier 2 — self-sampler fingerprint (the giveaway).** If Tier 1 is
  absent/ambiguous, classify as `Rezolus` when the columns contain the Rezolus
  self-telemetry namespace `rezolus_*` — anchored on the **cross-platform**
  self-metrics from the `rezolus/rusage` sampler: `rezolus_cpu_usage`,
  `rezolus_memory_usage_resident_set_size`, `rezolus_rusage`. That sampler is
  registered unconditionally (no `cfg`, uses `getrusage`), so these are present on
  both Linux and macOS recordings. **Do not** anchor on `rezolus_bpf_run_count` /
  `rezolus_bpf_run_time` — those live in the Linux `cpu/*` eBPF samplers and are
  absent on macOS, so they'd miss a legitimate macOS Rezolus capture. This is a
  near-zero-false-positive signal (a foreign capture would never carry a
  `rezolus_`-prefixed metric) and needs almost no maintenance — deliberately
  chosen over enumerating every domain prefix (`cpu_`, `scheduler_`, …), which is
  collision-prone and drifts as samplers are added.
- Otherwise ⇒ **`Simple`**.

**Naming:**
- Default Rezolus agent source resolves its display name as: `node` from
  `per_source_metadata` if present, else the literal `"rezolus"` — never blank,
  never a filename.
- `Simple` source label falls back: explicit `source` metadata value → parquet
  filename stem → `"metrics"`.

**Accepted caveat:** the self-sampler can be disabled in agent config. A
recording with the self-sampler off *and* stripped metadata would fall through to
`Simple`. Tier 1 catches every normal recording, so this edge is accepted rather
than reintroducing broad prefix-matching.

**Placement:** `detect_source_kind` and the `SourceKind` type live in the
**viewer** (`src/viewer/`, alongside `regenerate_dashboards`). The `dashboard`
crate stays passive and dumb — it does not classify anything; see Section B.

## Section B — Dashboard wiring (per-source sections, coexist)

Classification happens at the **source** level, in the viewer. The viewer's
`regenerate_dashboards()` classifies each source and hands
`build_dashboard_context()` the already-decided section list (which sources are
Rezolus/Service/Simple, and the `source: <name>` routes to add). The `dashboard`
crate stays **passive and dumb**: it renders the sections it is given and never
inspects metadata to decide what a source is. `SourceKind`'s effect on sections:

- `Rezolus` source → the existing built-in sections, as today.
- `Service` source → its service-extension section, as today.
- `Simple` (unrecognized) source → a dedicated **`source: <name>`** section
  (route `/source/{name}`) containing the metrics table + inline charts
  (Section C), scoped to that source's metrics.

Every source is classified independently, so a combined file that holds Rezolus
metrics *and* one or more unrecognized sources shows the Rezolus built-in
sections **and** a `source: <name>` section per unrecognized source, side by
side. The decision is always per source, never per column: a source we don't
recognize gets its own table section rather than having its metrics dropped
(today's behavior).

**Default landing rule:** land on Overview if any `Rezolus`/`Service` section
exists; otherwise land on the first `source: <name>` section. A pure simple
capture (one unrecognized source) therefore opens straight into its table view,
and the 13 built-in Rezolus sections are gated on a `Rezolus` source being
present (fixing today's empty-section UX for non-Rezolus files).

## Section C — The `source: <name>` browse-to-chart section

Each unrecognized source gets a section titled **`source: <name>`**. Top: a
searchable/sortable table of *that source's* metrics, one row per metric name.
Bottom: type-appropriate charts for the selected rows.

Table columns: `name`, `type` (counter/gauge/histogram), `series` (cardinality),
`labels` (union of label keys), `description` (from `descriptions`). No source
column — the section is already scoped to a single source.

Selection model: multi-select rows; each selected metric renders a chart below,
keeping the existing per-chart style switcher so the user can override.

**Type → default query & style** (reusing `resolveStyle` + the existing
query/chart pipeline verbatim):
- **counter** → `rate(m[<default-window>])`; line (single series) / heatmap
  (multi-series). Default window = the viewer's existing default rate window.
- **gauge** → raw `m`; line / heatmap / multi.
- **histogram** → `histogram_quantiles(...)` percentiles (scatter) with a heatmap
  toggle.

Selecting a row issues the query through the existing `/api/v1/query_range` →
`applyResultToPlot` path. No new chart code, no new query engine. The Metrics
section is distinct from Query Explorer (which stays as the free-form PromQL
power tool).

## Section D — Backend seam

- **Catalog assembler** — lives in the **viewer** (data enumeration, not
  rendering — the `dashboard` crate stays passive). Pure Rust over the existing
  `MetricsSource` trait, taking a **source filter** so it returns only one
  source's metrics (source read
  from per-column `source` metadata, which `parquet combine` already writes; a
  single untagged foreign parquet is treated as one source). Iterate
  `counter_names()`/`gauge_names()`/`histogram_names()` (name + type);
  `*_labels(name)` → `series_count` + union of `label_keys`; `descriptions` map →
  `description`. Produces `Vec<MetricInfo { name, kind, series_count,
  label_keys, description }>`. No `metriken-query` changes.
- **New endpoint** `/api/v1/metrics?source=<name>` → the source-scoped catalog
  JSON, with the same `?capture=baseline|experiment` param as sibling endpoints.
- **Frontend `source: <name>` section** fetches its source's catalog, renders the
  table, and on row-select builds the type-appropriate query and calls the
  existing `/api/v1/query_range`, rendering via existing `applyResultToPlot`.
- **WASM parity** — the assembler is trait-level Rust, so the static-site viewer
  gets it by exposing one matching call in the WASM module; the shared frontend
  assets already carry the HTTP-vs-WASM fetch shim used by other endpoints, so
  the `source: <name>` section works in both server and WASM builds.

## Section E — Testing (TDD)

- **Rust units (first):** `detect_source_kind` across all tiers (metadata
  Rezolus, service template, `rezolus_*` self-sampler fingerprint, simple
  fallback) and name resolution — **including a macOS-shaped recording** whose
  columns carry only the `rezolus/rusage` self-metrics and no `rezolus_bpf_*`, to
  guard against regressing to a Linux-only fingerprint; catalog assembler (types,
  series counts, missing-description case).
- **Pure-JS test** (`node --test tests/*.mjs`): the type → default-query/style
  mapping, extracted as a pure function.
- **Smoke test:** extend `tests/viewer_smoke.sh` with a small non-Rezolus parquet
  fixture → assert `/api/v1/metrics?source=<name>` returns the catalog, the
  `source: <name>` section is the default landing, and no empty Rezolus sections
  render. (Fixture generation is a small sub-task; e.g. record a Prometheus
  endpoint or craft a minimal parquet.)

## Scope / YAGNI

- No template authoring, no persistence of selected-metric layouts (beyond what
  the existing selection/report mechanism already offers).
- One `source: <name>` section per unrecognized source; no cross-source "browse
  everything" table in this pass (a "show all sources" toggle could come later).
- No changes to `metriken-query`; the TSDB already exposes the needed catalog.

## Open items (resolve during planning)

- The canonical list of `rezolus_*` anchor metrics to match on — keep it to the
  **cross-platform** `rezolus/rusage` self-metrics (present on Linux *and*
  macOS), not the Linux-only `rezolus_bpf_*`; centralize as one constant.
- How a source's metric set is enumerated from per-column `source` metadata
  (column subset per source) — combine already writes it; confirm the read path
  for the catalog assembler's source filter.
