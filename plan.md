# Restoration Plan: Restore Features Lost in the PromQL Purge

## Context

Commit `870f045` ("viewer: PromQL purge (P1-P6) — strip dead PromQL surfaces") removed code paths that were broken on this branch — the SQL-only backend couldn't parse PromQL strings the frontend was sending. **However**, several user-facing features that worked on `main` (where the PromQL backend was still alive) silently regressed when C5 deleted the `metriken-query` crate. The purge made the regressions visible by deleting the now-vestigial PromQL plumbing, but didn't restore the features.

This plan restores them with SQL-native equivalents. The engine side already has every needed UDF and the per-metric `grouping_power` lookup — restoration is mostly new SQL emitters, one new endpoint shape, and a tasteful amount of frontend re-wiring.

**Scope:** restore parity with `main` for: bucket heatmaps, quantile spectrum heatmaps, Query Explorer, multi-node selection, KPI rendering for the 90 untranscribed templates, and saved-report backward compatibility.

**Out of scope:** multi-rezolus cross-source aggregation (carve-out 3 in `review/review.md`); this never worked on `main` server-side either.

---

## R5 — Saved-report backward compatibility (land first)

Smallest, lowest-risk change. Land it first so the test bench is clean for the bigger restorations.

**Change:** add `#[serde(alias = ...)]` to accept old wire format on read.

- `/work/rezolus/crates/report-save/src/lib.rs:55` — add `#[serde(alias = "promql_query")]` to `ReportEntry::sql_query`, `#[serde(alias = "promql_query_experiment")]` to `sql_query_experiment`. Canonical name stays `sql_query`; re-saves emit the new shape.
- Tests in `lib.rs::tests` (the `sql_resolve_tests` module): add a fixture-string deserialize test for an old `{"promql_query": "...", "promql_query_experiment": null}` payload; assert the resolved `ReportEntry` carries the alias content in `sql_query`.

**Verification:** `cargo test -p report-save` (was 4/0, expect 5/0 after adding the alias test).

**Commit:** `report-save: accept legacy promql_query field as serde alias`.

---

## R1 + R2 — Bucket heatmap & quantile spectrum (paired)

Both regressions traced to the same root: the deleted `fetchHeatmapForPlot` / `fetchQuantileSpectrumForPlot` sent PromQL `histogram_heatmap(...)` / `histogram_quantiles([...], ...)` strings to a SQL backend that errored. The H2 UDFs (`h2_quantile{s}`, `h2_upper`, `h2_count_in_range`, `h2_delta`) already exist in `/work/metriken/metriken-query-sql/src/udf.rs`. `MetricCatalog::histogram_p_by_metric` (`views.rs:109`) gives us per-metric `grouping_power` for free.

### New endpoint: `/api/v1/heatmap_range`

A dedicated route shape, separate from `/api/v1/query_range`'s Prometheus matrix projection. The dedicated shape is justified because (a) bucket heatmaps need `bucket_bounds` as a first-class field, (b) quantile spectrum needs `color_min_anchor` (peeled-off p0), and (c) projecting through Prometheus matrix would force the frontend to reshape NxM triples back into a dense grid.

- `/work/rezolus/src/viewer/routes.rs:594` — add `heatmap_range` handler alongside `instant_query` / `range_query`. Wires in `src/viewer/routes.rs::app` next to the existing `query_range` route.
- Request: `{ metric: String, kind: "buckets" | "quantile_spectrum", quantiles: Option<Vec<f64>>, capture, node }` (the `node` field hooks R4).
- Response (untagged enum, JSON):
  - `Buckets`: `{ time_data: Vec<f64>, bucket_bounds: Vec<u64>, data: Vec<(u32, u32, u64)>, min_value: f64, max_value: f64 }`
  - `QuantileSpectrum`: `{ time_data: Vec<f64>, data: Vec<Vec<f64>>, series_names: Vec<String>, color_min_anchor: Option<f64> }`
- Implementation: handler looks up `p` via `state.sql_backend.describe_parquet(data_source)?.histogram_p_by_metric[&metric]`, builds SQL via the new emitters, runs through `run_sql`, projects Arrow batches into the response shape server-side (single pass to compute `min_value`/`max_value` / strip p0).

### New SQL emitters in `crates/dashboard/src/sql.rs`

- `pub fn bucket_heatmap_sql(metric: &str, p: u8, source: Option<&str>) -> String` — emits `SELECT timestamp, h2_delta("metric", LAG("metric") OVER (ORDER BY timestamp)) AS buckets FROM _src[_<source>] ORDER BY timestamp`. The handler then expands the `buckets` LIST column into sparse `(t_idx, b_idx, count)` triples server-side using a CROSS JOIN with `generate_series(0, bucket_count(p))`, dropping zero rows.
- `pub fn quantile_spectrum_sql(metric: &str, quantiles: &[f64], p: u8, source: Option<&str>) -> String` — emits `SELECT timestamp::DOUBLE/1e9 AS t, h2_quantiles("metric", [<quantiles>], p) AS qs FROM _src[_<source>] ORDER BY t`. Handler unpacks the `LIST<UBIGINT>` column-wise into per-quantile parallel arrays; peels off the `p0` row to derive `color_min_anchor`.

Snapshot tests live in `crates/dashboard/tests/sql_snapshots.rs` (the existing harness).

### Frontend restoration

- `/work/rezolus/src/viewer/assets/lib/data.js` — restore `fetchHeatmapForPlot`, `fetchHeatmapsForGroups`, `fetchQuantileSpectrumForPlot`. They now POST `{metric, kind, quantiles?, capture, node?}` to `/api/v1/heatmap_range` and return the unwrapped response shape — which is already exactly what `histogram_heatmap.js` and `quantile_heatmap.js` consume. No chart code changes.
- `/work/rezolus/src/viewer/assets/lib/viewer_core.js:488-489` — re-enable the `buildHistogramHeatmapSpec(spec, sectionHeatmapData.get(spec.opts.id), prefixTitle(spec.opts))` branch by populating `heatmapDataCache` via the new fetch path in `app.js::fetchSectionHeatmapData`.
- `/work/rezolus/src/viewer/assets/lib/app.js:426` — `fetchSectionHeatmapData` calls `fetchHeatmapsForGroups(groups, {capture, node})` and stores results in `heatmapDataCache`.
- `/work/rezolus/src/viewer/assets/lib/charts/metric_types.js::buildHistogramQuery` — already deleted; nothing to restore there (the new flow doesn't need query-string wrapping).

### Tests

- Rust: SQL snapshot tests in `crates/dashboard/tests/sql_snapshots.rs` for both new emitters.
- Rust: integration test for the route in `src/viewer/routes.rs::tests` (or a new `crates/dashboard/tests/heatmap_range.rs` if the route is moved to the dashboard crate's binary route registry — verify the rezolus pattern first).
- Frontend: restore a frontend test mirroring the deleted `tests/data_spectrum_capture.test.mjs` shape — feed fixture JSON in, assert `data[0] === time_data`, `data.length === series_names.length + 1`, and `color_min_anchor` is peeled correctly. Suggested filename: `tests/heatmap_capture.test.mjs`.
- Smoke: `bash scripts/viewer_chromium_smoke.sh site/viewer/data/cachecannon.parquet` must render histogram heatmaps and quantile spectra without console errors.

### Risks

- **`min_value` semantics:** heatmap colorscale uses log10 of non-zero counts only. Server-side projection must filter zeros before computing min — otherwise log10(0) = -∞ leaks in.
- **`h2_quantiles` returns `UBIGINT`:** values are integer raw counts/nanoseconds. Cast to `DOUBLE` before serialization to avoid JSON precision drift.
- **Bucket-zero filtering:** for sparse histograms `h2_delta` produces many zero rows; drop them server-side, not on the wire.

### Commit grouping

- **H1**: `dashboard: add bucket_heatmap_sql and quantile_spectrum_sql emitters` (sql.rs + snapshot tests).
- **H2**: `viewer: add /api/v1/heatmap_range endpoint` (routes.rs + tests).
- **H3**: `viewer: restore heatmap and spectrum fetch paths` (data.js + viewer_core.js + app.js + frontend test).

---

## R3 — Query Explorer (SQL-input)

Pure frontend rebuild. Backend already accepts arbitrary SQL via `/api/v1/query_range` (`src/viewer/routes.rs:601`).

**Change:** rebuild `src/viewer/assets/lib/features/explorers.js` from scratch as SQL input.

- Two components, mirroring the pre-purge shape:
  - `QueryExplorer` — textarea + Execute (Ctrl+Enter) + unit selector + history dropdown + error panel + result chart. localStorage key `sql_history` (migrate from `promql_history` on first read: copy verbatim, let user clean up — stale PromQL just errors on submit).
  - `SingleChartView` — pinned chart route `/:section/chart/:chartId`. Editable title/description. Reuse `Chart` component.
- Example queries showcase SQL conventions: `irate_1s("cpu_usage/user/0", timestamp)`, `h2_quantile(latency, 0.99)`, `h2_quantiles(latency, [0.5, 0.9, 0.99])`, `SELECT timestamp, "cpu_usage/user/0" - "cpu_usage/user/1" FROM _src`, an `UNPIVOT COLUMNS('regex')` demo.
- Mount points in `app.js`: route `/query` → `QueryExplorer`, route `/:section/chart/:chartId` → `SingleChartView`.

**Backend tweak:** `/api/v1/query_range` currently translates "No matching columns" binder errors into empty matrices (silent-render UX for the dashboard). The Query Explorer needs raw error visibility — add a `?strict=true` query param that disables the empty-matrix shim. `src/viewer/routes.rs:583-599`.

**Tests:** frontend unit test for history dedupe/trim. Smoke step: navigate to `/query`, run a tiny SELECT, assert non-empty result.

**Commit:** `viewer: restore QueryExplorer and SingleChartView with SQL input`.

---

## R4 — Multi-node selection (server-side filter)

On multi-node parquets, columns are prefixed `<node>::<metric>` and the `node` label lives in Arrow field metadata. Today, the top-nav node picker updates client state but doesn't change which columns queries see.

### Engine side: per-node views

- `/work/metriken/metriken-query-sql/src/views.rs` — add `render_per_node_views_sql(columns: &[ColumnInfo]) -> String`, mirroring the existing `render_per_source_views_sql` pattern (`views.rs:361`). Groups columns by their `node` field-metadata label, emits one `CREATE OR REPLACE TEMP VIEW _src_node_<sanitized_node>` per node, reprojecting columns with the `<node>::` prefix stripped.
- `view_name_for_source` sanitization (`views.rs:445`) — reuse for node names. Alphanumeric + underscore only; non-conforming becomes `_`.
- Plumb through `ensure_views` so per-node views are materialized at parquet load alongside per-source views. DuckDB views are lazy query plans, so the materialization cost is O(nodes × columns) plan-storage, not data copying.
- Add `pub fn nodes(&self) -> Vec<&str>` to `MetricCatalog` so callers can enumerate (read distinct values of `node` from `series_by_metric[*].labels`).

### Backend rewrite

- `/work/rezolus/src/viewer/routes.rs:601` — `range_query` accepts an optional `node: Option<String>` query param.
- Before calling `run_sql`: if `node.is_some()`, validate the node name against `state.sql_backend.describe_parquet(...).nodes()`, then rewrite the SQL string substituting bare `_src` references → `_src_node_<sanitized>`. Use a token-aware regex (`\b_src\b(?!_)`) so existing references like `_src_cachecannon` are untouched. Document the regex limitation (won't handle `_src` inside string literals — unusual but possible).
- Reject unknown nodes with HTTP 400.
- Apply the same `node` param to `/api/v1/heatmap_range` (R1/R2).

### Frontend wiring

- `/work/rezolus/src/viewer/assets/lib/app.js::changeNode` — already updates `selectedNode` and clears caches; no logic change there.
- `/work/rezolus/src/viewer/assets/lib/data.js::queryRange` and `queryRangeForCapture` — accept `{node}` in the options object, forward as `?node=` query param.
- Same threading through `fetchHeatmapForPlot` / `fetchHeatmapsForGroups` / `fetchQuantileSpectrumForPlot` (R1/R2 helpers).
- `viewer_core.js` and per-chart fetch sites: pass `getSelectedNode()` from `data.js` through every fetch call.

### Tests

- Engine: `metriken-query-sql/src/views.rs::tests` — feed a fixture catalog with nodes `["alpha", "beta"]`, assert `render_per_node_views_sql` output and that sanitization rejects malformed node names.
- Backend: route test in `src/viewer/routes.rs::tests` — `?node=alpha` rewrites the query; `?node=alpha; DROP` returns 400.
- Frontend: smoke test against a multi-node fixture (may need a new tiny fixture parquet in `site/viewer/data/`).

### Commits

- **N1**: `metriken-query-sql: materialize _src_node_<X> views and expose MetricCatalog::nodes()`.
- **N2**: `viewer: accept node param on query_range and heatmap_range`.
- **N3**: `viewer: wire node picker through fetch helpers`.

### Risks

- **Regex SQL rewrite** is correct for all dashboard-generated queries but doesn't survive `'_src'` string literals in user-typed Query Explorer SQL. Acceptable for round 1 — Query Explorer users are technical and can spell out `_src_node_<X>` explicitly if needed. Document.
- **Saved reports** captured on multi-node parquets without a node selected replay against the aggregated `_src`. Different from the user's mental model if they were viewing a single node when saving. Acceptable degradation; document.

---

## R6 — KPI SQL transcription (90 untranscribed templates)

Depends on R1+R2 (heatmap and spectrum emitters) and benefits from R5 (clean test history).

### Engine plumbing

- `crates/dashboard/src/dashboard/service.rs::generate` (`service.rs:122`) — at KPI emit, look up `MetricCatalog::histogram_p_by_metric.get(metric_name)` and inject `p` into the SQL via a new `{{p}}` substitution alongside the existing `{{view}}` placeholder. Falls back to `p=3` if the lookup misses (rezolus default).
- Add a public helper `substitute_view_and_p(sql: &str, source: &str, p: u8) -> String` in `crates/dashboard/src/dashboard/service.rs` (next to `substitute_view`).
- The `MetricCatalog` lookup at emit time requires the catalog to be available — `service::generate` already takes a `data: &dyn DashboardData` which exposes the catalog. Confirm and wire.

### New SQL builders in `crates/dashboard/src/sql.rs`

- `pub fn percentile_kpi_sql(metric: &str, q: f64) -> String` — wraps `h2_quantile("{{view}}.\"{metric}\"", {q}, {{p}})`.
- `pub fn multi_percentile_kpi_sql(metric: &str, qs: &[f64]) -> String` — wraps `h2_quantiles(...)`.
- `pub fn unpivot_columns_sql(regex: &str, label: &str) -> String` — for the 6 regex-multi-value selectors. Uses `UNPIVOT` + `COLUMNS('regex')`.
- Reuse `bucket_heatmap_sql` / `quantile_spectrum_sql` from R1/R2 for histogram KPIs with `subtype="buckets"` / `"quantile_heatmap"`.

### Template work in `config/templates/*.json`

Categorized (counts from `python3` audit earlier in the conversation):
1. **~75 histogram percentile KPIs** — mostly latency selectors like `response_latency{source="cachecannon"}`. Mechanically scripted: each template's KPI gets a generated `sql` field invoking `h2_quantile` or `h2_quantiles` against `{{view}}.<metric>`.
2. **~13 compound expressions** (`A / B`, `A - B`) — hand-port each using the layer-A SQL builders. One commit per template family (cachecannon compound, sglang compound, vllm compound).
3. **~6 regex-multi-value selectors** (`finished_reason=~"stop|length"`) — `UNPIVOT` + `COLUMNS('regex')` via the new `unpivot_columns_sql` helper.
4. **9 placeholder KPIs** (no `query` or `sql` after the purge stripped `query`) — either replace with a SQL stub or drop. Coordinate with maintainer per template.

### Tests

- Snapshot tests for emitted SQL strings in `crates/dashboard/tests/sql_snapshots.rs` for each new builder. Use `insta`-style structural assertions (substring matches on `h2_quantile`, the right metric name, the right percentile) rather than full-string snapshots — this keeps the 75-KPI bulk change low-friction.
- Backend integration: `parquet annotate site/viewer/data/cachecannon.parquet` reports 14/14 KPIs binding (was 11/14 — the 3 histograms now bind via the new heatmap/spectrum emitters).
- Same against `sglang_gemma3.parquet` — 13/13 binds (was 6/13).
- Smoke: chromium per-section walk on cachecannon and sglang_gemma3 — no `_unavailable` placeholders in the KPI sections.

### Commits

- **K1**: `dashboard: percentile_kpi_sql + multi_percentile_kpi_sql emitters + service.rs p-substitution`.
- **K2**: `templates: transcribe histogram-percentile KPIs to SQL` (~75 KPIs).
- **K3**: `dashboard: hand-port compound histogram KPIs`.
- **K4**: `dashboard: UNPIVOT + COLUMNS regex for multi-value KPIs`.
- **K5**: `templates: stub or remove unported KPI placeholders`.

### Risks

- Snapshot churn — 75 emitted SQL strings going through `cargo insta review` is noisy. Mitigation: assert substrings + a single golden-file dump, not full-string snapshots per emitter.
- `{{p}}` lookup miss: if a template references a metric absent from the loaded parquet, return-empty rather than panic. The current `parquet annotate` flow already marks such KPIs `available=false`.

---

## Overall sequencing

| Batch | Commits | Description |
| ----- | ------- | ----------- |
| 1     | S1 (R5) | Saved-report alias |
| 2     | H1-H3 (R1+R2) | Heatmap + spectrum endpoint, emitters, frontend |
| 3     | N1-N3 (R4) | Multi-node per-node views + route param + fetch wiring |
| 4     | Q1 (R3) | Query Explorer SQL rebuild |
| 5     | K1-K5 (R6) | KPI transcription (depends on R1/R2) |

R3 is independent of R1/R2/R4 — it can land in parallel with any earlier batch. I'm sequencing it after multi-node so the Explorer's "node picker" interaction is consistent with the rest of the viewer by the time the Explorer ships.

## Verification gates (per batch)

- `cargo build --bin rezolus` — clean
- `cargo test --workspace` — 0 failures (no new ignored)
- `cargo test --workspace` in `/work/metriken` when N1 lands
- `node --test tests/*.mjs` — 111+ pass / 0 fail
- `bash scripts/viewer_chromium_smoke.sh site/viewer/data/cachecannon.parquet`
- `bash scripts/viewer_chromium_smoke.sh site/viewer/data/sglang_gemma3.parquet`
- `bash tests/viewer_smoke.sh` (the existing end-to-end smoke)

## Critical files to modify

- `/work/rezolus/crates/report-save/src/lib.rs` (R5)
- `/work/rezolus/crates/dashboard/src/sql.rs` (R1, R2, R6 emitters)
- `/work/rezolus/crates/dashboard/src/dashboard/service.rs` (R6 substitution)
- `/work/rezolus/src/viewer/routes.rs` (R1+R2 endpoint, R3 strict param, R4 rewrite)
- `/work/rezolus/src/viewer/assets/lib/data.js` (R1+R2 fetch helpers, R4 node param)
- `/work/rezolus/src/viewer/assets/lib/viewer_core.js` (R1+R2 wiring)
- `/work/rezolus/src/viewer/assets/lib/app.js` (R1+R2 heatmapDataCache, R3 routes)
- `/work/rezolus/src/viewer/assets/lib/features/explorers.js` (R3 rebuild)
- `/work/metriken/metriken-query-sql/src/views.rs` (R4 per-node views, R4 `nodes()` accessor)
- `/work/rezolus/config/templates/*.json` (R6 KPI transcriptions)
