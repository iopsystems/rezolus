# Per-source metric descriptions in combined parquets

- **Opened:** 2026-07-04
- **Status:** SHIPPED — merged as part of **PR #989** (`c3babff3`).
- **Prior:** surfaced while building [simple-capture viewer support](2026-07-03-simple-capture-viewer.md).

This entry is the design record for the effort (it absorbs the original brainstorming spec + plan).

## Problem

Metric descriptions (help text) are stored as a single **file-level** parquet
footer key, `descriptions` = flat `{ metric_name → help }`. In a **combined** file
(from `parquet combine` or multi-endpoint `record`), the producer **union-merged**
all inputs' description dicts into that one top-level dict, keyed by bare metric
name, **first-writer-wins on collision** (`src/parquet_tools/combine.rs`). Two
sources defining the same metric name with different help text collapsed to one
shared description — source distinction lost for descriptions, even though the data
columns stay source-distinguished by label.

Where descriptions come from (verified, for the record): a Rezolus agent serves
them at `GET /metrics/descriptions` from each metric's `#[metric(description=...)]`
(`src/agent/exposition/http/mod.rs`); a Prometheus source contributes them from
`# HELP` lines harvested during scrape (`src/recorder/prometheus.rs`). Both land in
the same `descriptions` footer key.

## Decisions

- **Minimal scope:** only the combined-file producer changes. Single-source
  `record` keeps writing the top-level `descriptions` (unambiguous — do not touch).
- **Schema:** nest under `per_source_metadata.<source>.descriptions` (new
  `NESTED_DESCRIPTIONS` const in `src/parquet_metadata.rs`), a sibling of
  `version`/`role`/`sampler_status`.
- **Read path:** viewer resolves **per-source first, top-level fallback** — no
  migration of existing parquets needed.
- **One producer, for free.** Multi-endpoint `record` routes through
  `combine::combine_files` (`src/recorder/mod.rs`), so fixing `combine.rs` fixes
  both combined-file producers; the recorder is untouched.

## What shipped

- **Producer** (`9ff31dc0`): `combine` stops union-merging to the top level and
  nests each input's descriptions under its own
  `per_source_metadata.<source>.descriptions`; already-combined inputs carry their
  nested descriptions through the existing per-source merge. Unit test combines two
  sources with the **same** metric name and **different** help text and asserts
  both survive (37/37 combine tests green).
- **Consumer** (`904cf216`): shared
  `dashboard::metric_catalog::resolve_descriptions(&Value, source) -> Map`
  (per-source-first, top-level fallback, else empty), wired into both the server
  `/api/v1/metrics` handler (`src/viewer/routes.rs`) and the WASM `metrics()`
  (`crates/viewer/src/lib.rs`) — byte-identical resolution. `assemble_catalog`'s
  signature is unchanged.
- `mcp describe-metrics`: **checked, no change** — it uses `crate::common::metric_descriptions()`,
  a build-time static map, not parquet metadata, so it is orthogonal (no regression).
- Docs: `CLAUDE.md` "Parquet File Format" section updated.

## Verification

- Rust units: the combine anti-collision test; the `resolve_descriptions`
  per-source/fallback/empty cases.
- Whole-increment review: **READY** — producer↔consumer seam confirmed (identical
  `per_source_metadata.<source>.descriptions` key path *and* matching source
  identifier on both server and WASM; single-source and legacy files resolve via
  the fallback).

## Notes / limitations

- Descriptions are per-**source**, not per-node — they are identical across a
  source's nodes (same binary/exporter), so they attach at the source level, not
  the node/instance sublevel.
- A source only gets descriptions if its origin supplied them: a Rezolus agent
  always does; a Prometheus exporter only if it emits `# HELP`. (The originating
  observation: a user's `run.parquet` had blank descriptions because the `llm-sim`
  exporter emitted no `# HELP` — nothing to harvest, so no key written.)
