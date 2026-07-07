# Selection → Notebook → Report

- **Opened:** 2026-05-10
- **Status:** SHIPPED — merged (see PRs)
This entry covers the arc that turned the viewer's "Selection" pinned-charts
workspace into three distinct artifact types — **Notebook** (live editor),
**Selection** (imported JSON pattern, read-only), and **Report** (parquet-embedded
annotation, read-only) — then made "Save as Report" produce a column-trimmed
parquet recognized by the viewer as a self-contained report. The arc ran across
six PRs over roughly a week (2026-05-10 through 2026-05-16).

**Scope boundary:** the A/B compare machinery, the `parquet combine --ab`
combined-parquet format, and the two-file A/B viewer mode are part of the
compare-mode arc — see `2026-04-21-ab-compare-mode.md`. This entry covers only
the Notebook authoring surface, the Selection/Report sidebar entries, the v3
schema, the column-trim write path, and the Markdown notes/title layer.

---

## Problem

The viewer's "Selection" feature conflated two different things in one
workspace: a **pattern** (which charts to show, with which toggles) and a
**snapshot** (notes tied to specific data observations). This caused several
real problems:

1. **State loss on reload.** `setStorageScope` reassigned the localStorage key
   then immediately called `clearStore()` using the just-set key, wiping pinned
   charts and notes on every page refresh (#882).
2. **Empty histograms.** The chart loader called `executePromQLRangeQuery` with
   the raw stored query, bypassing the `histogram_quantiles(...)` wrapping that
   `buildEffectiveQuery` applies for histogram charts. Pinned histogram cards
   rendered blank (#882).
3. **Missing breadcrumb context.** Pinned chart titles showed only `opts.title`
   (e.g. "Total Flushes") with no indication of section or group (#882).
4. **No compare-mode rendering.** `SelectionView` always rendered through `Chart`
   directly; visiting `/selection` while in compare mode reverted to
   baseline-only even though the toggles and anchors were persisted (#908).
5. **Notes and pattern conflated.** No distinction between a shareable
   pattern (no notes) and a data-annotated report (notes embedded). The
   read/write boundary of the Report view wasn't enforced.
6. **Full-file export.** "Save as Report" re-stamped the source parquet's footer
   and streamed the whole file back. A 100 MB capture stayed 100 MB even when
   the report referenced three charts.
7. **Static-site gap.** The WASM viewer on rezolus.com posted
   `/api/v1/save_with_selection` to a CDN that returned 405 (#924).

---

## Goal

Three artifact types with explicit transitions, each with a clear read/write
boundary:

| Artifact | Sidebar entry | Editable? | Source |
|----------|--------------|-----------|--------|
| **Notebook** | when content present | yes | user |
| **Selection** | when JSON loaded | no (Open in Notebook) | dropped JSON file |
| **Report** | when parquet has annotation | no (Open in Notebook) | embedded in opened parquet |

"Save as Report" should produce a column-trimmed parquet that loads directly
into the Report view without the empty rezolus-section overhead.

---

## Key decisions

- **v3 schema — clean break, no migration.** Pre-v3 payloads (v1 unversioned,
  v2) throw at load time with a clear message. `migrateSelection` in
  `src/viewer/assets/lib/selection/selection_migration.js` (SELECTION_SCHEMA_VERSION = 3)
  accepts only v3. Old localStorage keys (`rezolus_selection_*`) are abandoned
  on first load — acceptable because the feature wasn't yet widely used.

- **Three stores, one payload shape.** `notebookStore`, `reportStore`,
  `loadedSelectionStore` in `selection/selection.js` share the same v3 JSON
  shape. The only runtime difference is which fields are populated (`note` is
  stripped at JSON export time but kept in parquet annotation). This avoids
  a parallel schema per view.

- **Explicit read→write transition (Open in Notebook).** Without a transition,
  the user can't distinguish the saved state from their edits. Selection and
  Report always faithfully reflect their source; Notebook always reflects
  in-progress work. The copy-on-open includes overwrite-confirm if the Notebook
  already has entries.

- **Column trim in the server, column trim mirrored in WASM.** The server's
  `src/viewer/report_save.rs` uses `metriken_query::QueryEngine::columns(query)`
  (0.10.4) to walk the PromQL AST and return the physical parquet columns each
  query touches. `trim_parquet_to_columns` projects those columns (plus
  `timestamp` + `duration`), stamps `KEY_REPORT = "trimmed"` in the footer, and
  rewrites via the existing `rewrite_parquet`. The WASM crate got a parallel
  `crates/viewer/src/report_save.rs` (operates on `Bytes` instead of paths)
  rather than a shared workspace crate — the surface was small enough to accept
  the duplication; can be refactored later if it grows.

- **`KEY_REPORT` marker drives report mode.** `AppState::is_trimmed_report()`
  checks `trimmed_report_marker`; `regenerate_dashboards` short-circuits to an
  empty section list when true. Frontend reads `mode.report` on bootstrap and
  defaults the initial route to `/report`. Older viewers that don't know the key
  just render the trimmed parquet's (mostly empty) section list — ugly but not
  catastrophic.

- **`promql_query_experiment` + `category_members` on pin entries.** Bridge
  KPI charts (compare-mode charts with distinct baseline/experiment queries) need
  to store both queries at pin time. Entries gained optional
  `promql_query_experiment` and `category_members` fields, round-tripped through
  localStorage persist/restore and JSON export, defaulting null on older payloads
  (#910).

- **Markdown notes/preamble/titles with no schema change.** #931 added
  `renderMarkdown` / `renderMarkdownInline` in a new dependency-free `markdown.js`
  (XSS-safe: all chars HTML-escaped before transforms; `javascript:`/`data:` link
  hrefs degrade to plain text). `note`, `tagline`, and `entry.titleOverride` are
  plain strings already in the payload — additive, no version bump, old reports
  render unchanged.

---

## Delivery arc (six PRs)

| PR | Title | Merged | Role |
|----|-------|--------|------|
| **#882** | fix(viewer): selection section bug fixes | 2026-05-10 | Pre-conditions: state-loss, empty histograms, missing context |
| **#893** | test(viewer): end-to-end smoke test + viewer-smoke skill | 2026-05-10 | Safety net: the arc's PRs ran against a live smoke harness from here |
| **#908** | feat(viewer): SelectionView mirrors live compare mode | 2026-05-11 | PR "1 of 3": compare wiring (superseded #894 with merge-conflict resolution) |
| **#909** | feat(viewer): Notebook / Selection / Report rework | 2026-05-11 | PR 2 of 3: v3 schema, rename, LoadedSelection sidebar, read-only Report, save gating |
| **#910** | fix(viewer): compare-mode bridge KPIs render both arms | 2026-05-11 | Bug-fix against #909: bridge queries, asymmetric label extraction |
| **#919** | feat(viewer): Save as Report column-trim + report-mode load | 2026-05-13 | PR 4 of 4: server-side column trim, `KEY_REPORT`, report-mode bootstrap |
| **#923** | feat(wasm-viewer): restore Load Parquet upload affordance | 2026-05-13 | WASM regression from #919 (upload button lost; quick restore) |
| **#924** | feat(wasm-viewer): Save as Report works offline | 2026-05-13 | WASM port of trim + AB tarball repack |
| **#931** | feat(viewer): Markdown notes, preamble & editable titles | 2026-05-16 | No schema change: Markdown render layer over existing plain-string fields |

The "PR 3 of 3" slot (combined-A/B parquet format) was reassigned to the
compare-mode arc; the series renumbered to "PR 4 of 4" at #919.

---

## Learnings / dead-ends

- **The state-loss bug (#882) was a classic split-mutation trap.** The single
  `clearStore` function did two things: reset in-memory state and delete the
  localStorage key. `setStorageScope` needed only the in-memory reset but got
  both. The key was deleted under the new (file-scoped) name, which had nothing
  in localStorage yet, so data appeared to survive — then vanished on reload.
  The fix (`resetStoreState` / `clearStore` split) is the kind of correction that
  only surfaces with live use; the code path looks correct in isolation.

- **`buildEffectiveQuery` wrapping must happen at render time, not storage
  time.** The stored `promql_query` is the raw PromQL the chart was defined
  with. Applying the histogram wrapper at storage time would embed
  `histogram_quantiles(histogram_quantiles(...))` after a round-trip.
  Always store raw; always wrap at fetch time. This same double-wrap trap was
  independently rediscovered in the simple-capture-viewer arc (#989).

- **#908 was a superseded PR (#894) with a merge conflict on the version
  bump.** The original PR (`#894`) had incremented the alpha revision; by the
  time it landed, main had gone through a release cycle to `5.13.1-alpha.0`.
  The fix was to keep main's version and drop the stale bump. The actual viewer
  changes (`app.js`, `selection.js`, `viewer_core.js`) merged cleanly. Takeaway:
  version bumps on long-lived feature branches go stale against release PRs.

- **Bridge-KPI pinning (# 910) surfaced asymmetric label extraction.** Baseline
  charts used `spec.series_names` (a string list that loses multi-dimensional
  labels) while experiment charts read `item.metric` directly. The two sides
  produced disjoint match keys, resulting in "compare: no shared labels between
  captures." Fix: `composeScatterLabel(metric, options)` in `compare_math.js`,
  with an `excludeValues` set that drops dimensions whose values are
  capture-identity names.

- **WASM bundle size jumped 3.6 MB → 4.3 MB** when #924 ported the parquet
  writer + tar crate into the WASM build. The `no_asm` zstd-sys feature is
  target-scoped under resolver 2, so the native binary's zstd kept its x86 asm
  path. Acceptable cost for offline Report saving; noted as a refactor candidate
  if the shared surface grows.

- **#923 (restore Load Parquet upload affordance) was a quick regression
  fix** merged the same day as #919. The upload button was silently dropped
  during the report-mode bootstrap changes. Caught immediately in manual
  verification.

---

## Deferred / reopen

- **Customizable report title + browser tab title.** Users keeping multiple
  report tabs open can't distinguish them. Ask: user-settable title persisted in
  the payload (additive, same approach as `tagline`), and overwrite
  `document.title` to "Report/Notebook[: <title>]" on those routes. Tracked in
  project memory (`project_customizable_report_title.md`). No schema change
  needed; belongs with the existing `titleOverride`/preamble machinery in
  `selection/selection.js`.

- **Row / time trim** (`trim_range_ms`). The frontend already sends the field in
  the save payload; the server ignores it (spec non-goal for PR4). Separate PR
  when someone needs file size reduction by time range.

- **Live-mode trim.** `save_with_selection` in live mode converts msgpack
  snapshots to parquet at save time; the trim path is skipped. Separate concern.

- **`crates/viewer/src/report_save.rs` duplication.** The WASM crate's trim
  implementation is a parallel copy of `src/viewer/report_save.rs` operating on
  `Bytes`. Refactor into a shared workspace crate if the surface grows beyond
  the current ~150 lines.

- **Schema version drift guard.** If reports get shared widely and notes
  authored against one parquet get re-applied against a wrong one, add an
  optional `baseline_checksum` / `experiment_checksum` to the v3 payload and
  render a banner on mismatch (spec open question). Don't refuse to render —
  just warn.
