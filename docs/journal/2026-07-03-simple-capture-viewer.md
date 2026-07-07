# Viewer support for "simple capture" parquets

- **Opened:** 2026-07-03
- **Status:** IMPLEMENTED — in review as **PR #989** (iopsystems/rezolus). Closes when #989 merges.
- **Design/plan:** brainstormed → spec → plan → subagent-driven execution (9 tasks, each independently reviewed) → whole-branch review (READY).

This entry absorbs the original design + plan docs (previously
`docs/design/2026-07-03-simple-capture-viewer{,-plan}.md`, removed in favor of
this record).

## Problem

`rezolus view` assumed a recording was either a Rezolus-agent capture or a source
covered by a pre-registered service-extension template. A **simple capture** — a
parquet with perfectly legal metric columns (counter/gauge/histogram + `metric_type`
and labels in column metadata) that is neither from a Rezolus agent nor templated —
rendered badly: the built-in Rezolus sections showed up **empty** and the metrics
were reachable only via free-form PromQL in the Query Explorer (otherwise dropped).

## Goal

Make the viewer useful on any legal metrics parquet, per source, without a template
and without pretending it is a Rezolus recording.

## Key decisions

- **Per-source detection lives in the viewer** (`src/viewer/source_kind.rs`);
  the `dashboard` crate stays a passive renderer (it is handed a decided section
  list, never classifies). `SourceKind` = `Rezolus | Service | Simple`.
- **Layered classifier:** metadata markers first (`source=="rezolus"`,
  `sampler_status`), then a **cross-platform self-sampler fingerprint** — the
  `rezolus/rusage` metrics `rezolus_cpu_usage`, `rezolus_memory_usage_resident_set_size`,
  `rezolus_rusage`. Deliberately **not** `rezolus_bpf_run_*` (Linux/eBPF-only;
  they are absent on macOS and would misclassify macOS recordings — caught in
  review before implementation).
- The **spoofable `systeminfo` signal was dropped** from detection
  (`47ac274e`): `systeminfo` can be attached to any parquet via
  `parquet annotate --systeminfo`, so it is not Rezolus-exclusive. Removing it
  also deleted a buggy combined-file lookup found in review.
- **Per-source sections, coexisting.** Built-in Rezolus sections are gated on a
  Rezolus source being present (backward-compatible: an empty source list = legacy
  behavior, so every existing `build_dashboard_context` call site is unchanged).
  Each unrecognized source gets a `source: <name>` section (route `/source/<name>`).
- **Catalog over the existing TSDB.** `GET /api/v1/metrics?source=` returns
  `{name, metric_type, series_count, label_keys, description}` per metric, assembled
  from `metriken-query`'s `*_names()`/`*_labels()` accessors — **no metriken-query
  changes**. The assembler lives in the shared `dashboard` crate so the server and
  the WASM static site return **byte-identical** JSON.
- **Frontend `MetricBrowser`** — a searchable table; selecting rows renders
  type-appropriate charts (counter→`rate`, gauge→raw, histogram→percentiles)
  through the existing section/query pipeline.

## Acceptance (the GO check, all met)

- Rust units: `detect_source_kind` across all tiers including a macOS-shaped case
  (rusage self-metrics, no `rezolus_bpf_*`); the catalog assembler.
- Pure-JS: `buildDefaultQuery` type→query mapping (`node --test tests/*.mjs`).
- E2E: `tests/viewer_smoke.sh` gained a real non-Rezolus fixture
  (`site/viewer/data/simple_capture.parquet`) asserting the catalog endpoint, a
  `/source/` nav entry, and **no** `/cpu` built-in. Full run green across 9 modes.
- Human-verified in the browser (the user drove `rezolus view run.parquet`).

## Commit map (pre-merge SHAs on PR #989; will change on merge)

Detection + wiring: `8b330cd3` (SourceKind), `51f27b62` (gate built-ins + `source:`
entries), `47ac274e` (drop systeminfo signal). Catalog + endpoint: `c2516a7c`
(assembler + `MetricInfo`), `bd3db756` (`/api/v1/metrics`), `c4073fd7`
(`buildDefaultQuery`), `1b66af30` (`ViewerApi.getMetrics`). Frontend + parity:
`a325393b` (MetricBrowser), `a1565aa8` (stability fix, below), `23885b2d` (move
`metric_catalog` to the `dashboard` crate + WASM `metrics()`). Fixture + smoke:
`59e308ad`, `c9e143b1`.

## Learnings / dead-ends worth keeping

- **The review gate earned its keep on the MetricBrowser.** The first cut
  (`a325393b`) compiled and passed headless checks but was **non-functional**:
  `SectionContent` invoked an inline component factory, so Mithril's tag-identity
  diffing remounted the component every redraw (resetting filter/selection and
  re-firing the fetch), and an in-place spec mutation stopped echarts from
  reconfiguring. Fixed with a per-`sourceName` memoized component + fresh-spread
  spec per render (`a1565aa8`). Neither bug is visible to `node --check`/`cargo
  build` — only to a code-reading reviewer.
- **Bare `Chart` has no title or controls.** A later user report (untitled charts,
  dead Full/Tail toggles) traced to the MetricBrowser rendering a bare `Chart`
  instead of going through `createGroupComponent`, which supplies the title header
  and wires the histogram spectrum controls. Fixed by rendering selected metrics
  through the shared `Group`/section pipeline (`4f80fbae`) — which then surfaced a
  **histogram double-wrap** (`c1b26d14`): `buildDefaultQuery` already wraps a
  histogram as `histogram_quantiles(...)`, and the section pipeline
  (`buildEffectiveQuery`) wraps histograms again → `histogram_quantiles(...,
  histogram_quantiles(...))` → "Metric not found". Fix: pass the **raw** metric
  name for histograms and let the pipeline do the single wrap.
- **Two of the sharpest catches came from the user mid-flight** — the `systeminfo`
  spoofability and the macOS `rezolus_bpf_*` absence — each a real detection defect.

## Deferred (reopen conditions)

- **Combined-file per-source isolation.** A combined Rezolus+foreign file shares
  one merged TSDB, so a foreign source's fingerprint can bleed and it falls back to
  Query-Explorer-only (pre-feature behavior — no crash, no impact on the shipped
  single-source path). Per-source metric routing in combined files needs the
  per-column `source` metadata; revisit when combined simple-captures are needed.
- Minor review follow-ups (redundant clone/read, an `assemble_catalog` loop
  unroll) — non-blocking.

## Follow-on

This effort surfaced that combined parquets also **collapse descriptions** across
sources — split into its own effort: [per-source descriptions](2026-07-04-per-source-descriptions.md).
