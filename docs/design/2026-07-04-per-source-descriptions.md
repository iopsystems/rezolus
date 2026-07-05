# Design: per-source metric descriptions in combined parquets

**Date:** 2026-07-04
**Status:** Approved, pre-implementation
**Branch:** rides on `viewer/simple-capture` (PR #989) — the viewer consumer change
builds on that branch's `metric_catalog` / `/api/v1/metrics` code.

## Problem

Metric descriptions (help text) are stored as a single **file-level** parquet
footer key, `descriptions`, whose value is a flat JSON dict `{ metric_name →
help }`. When a parquet holds **multiple sources** (a combined file from
`parquet combine` or multi-endpoint `record`), the producers **union-merge** all
inputs' description dicts into that one top-level dict, keyed by bare metric name,
**first-writer-wins on collision** ([combine.rs:986‑1007](../../src/parquet_tools/combine.rs)).

Consequence: two sources that define the **same metric name** with different help
text collapse to a single shared description — the source distinction is lost for
descriptions, even though the data columns stay source-distinguished by label.

## Goal

Keep descriptions source-scoped in combined files, without disturbing the
single-source path or requiring migration of existing parquets.

## Decisions (from brainstorming)

- **Minimal scope:** only the combined-file producers change. Single-source
  `record` keeps writing the top-level flat `descriptions` (unambiguous).
- **Read path:** viewer resolves **per-source first, top-level fallback** — no
  migration of existing files needed.

## Schema

Add a nested key under `per_source_metadata.<source>`:

```
per_source_metadata.<source>.descriptions = { metric_name → help }
```

a sibling of the existing `version` / `role` / `sampler_status` /
`service_queries` / `first_sample_ns` / `last_sample_ns`. New constant
`NESTED_DESCRIPTIONS = "descriptions"` in `src/parquet_metadata.rs`.

The top-level `descriptions` key remains the **single-source / legacy** location.

## Producers (only the merge points change)

- **`combine`** (`src/parquet_tools/combine.rs`, the descriptions block at
  ~986‑1007): stop union-merging inputs' descriptions into the top-level dict.
  Instead, nest each input's descriptions under *its own*
  `per_source_metadata.<source>.descriptions` entry (combine already builds a
  per-source metadata object per input — attach descriptions there). No top-level
  `descriptions` written for combined output.
- **Multi-endpoint `record`**: at the point per-endpoint streams merge into one
  parquet, nest per-source rather than union to top-level. (Confirm during
  planning exactly where that merge lives — it may share combine's code path;
  `build_per_source_metadata` in `src/recorder/mod.rs` is the natural place to
  thread the descriptions through.)
- **Single-source `record`**: unchanged — top-level `descriptions`.

## Consumers (per-source-first, top-level fallback)

- **`/api/v1/metrics?source=` handler** (`src/viewer/routes.rs`) and the **WASM
  `metrics()`** (`crates/viewer/src/lib.rs`): resolve the effective descriptions
  map for the requested source — use `per_source_metadata.<source>.descriptions`
  if present, else the legacy top-level `descriptions`. Pass that flat
  `{name→help}` map into `assemble_catalog`.
- **`assemble_catalog` is unchanged** — it still takes a flat descriptions map;
  only the two handlers gain the resolution logic. Extract that resolution into a
  small shared helper so server and WASM stay byte-identical (e.g. a function that
  takes the parsed file-metadata JSON + a source name and returns the effective
  `serde_json::Map`).
- **Flag for planning:** check whether `mcp describe-metrics`
  (`src/mcp/describe_metrics.rs`) reads the top-level `descriptions` on combined
  files; if so, give it the same per-source-first fallback (or note it as a
  follow-up if out of scope).

## Testing

- **combine unit test:** combine two inputs that both define metric `X` with
  *different* help text → assert the output has
  `per_source_metadata.<srcA>.descriptions[X]` and
  `per_source_metadata.<srcB>.descriptions[X]` with the respective texts (both
  preserved, no collision), and no lossy top-level union.
- **resolution helper unit test:** per-source present → returns nested; per-source
  absent → falls back to top-level; neither → empty.
- **Existing single-source behavior:** unchanged (top-level still written and
  resolved via fallback).

## Scope / YAGNI

- No migration of existing parquets; the fallback covers them.
- No per-node descriptions (descriptions are identical across a source's nodes —
  same binary/exporter); they attach at the source level, not the node/instance
  sublevel.
- Combined-file per-source *metric browsing* in the viewer remains the separately
  deferred "Task 5b" work; this change only fixes where descriptions are stored
  and read, which is independent and benefits any consumer.
