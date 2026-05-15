# TOMORROW.md — service-extension KPI transcription + leftovers

Captured after the multi-fix session that closed B1–B4 + sparse-metric
binder-error handling + the cgroups injectLabel/SQL bug. See
`REVIEWING.md` for the post-fix state of those.

---

## 1. Transcribe service-extension KPI templates to SQL

### Status today

`config/templates/*.json` carries 10 service templates (cachecannon,
vllm, vllm-decode, vllm-prefill, sglang, sglang-prefill, sglang-router,
llm-perf, valkey, inference-library). Every KPI has a `query`
(PromQL) field and **no** `sql` field. The SQL frontend dispatcher
(`BACKEND='sql'`) reads `plot.sql_query`, sees `null`, and renders a
"query not yet available" placeholder. Result: `/service/cachecannon`
and the equivalent for the other 9 services show 0 plots.

The KPI infrastructure to consume `sql` fields is already in place:
- `crates/dashboard/src/service_extension.rs::Kpi` has
  `pub sql: Option<String>` (commit `fd285bb`).
- `crates/dashboard/src/dashboard/service.rs::generate` calls
  `plot_promql_with_sql{,_full}` when `kpi.sql.is_some()`.
- `crates/dashboard/src/dashboard/service.rs::tests::kpi_sql_none_emits_promql_query_only`
  pins the absence-of-sql → PromQL-only emission (don't accidentally
  break this when adding sql fields).

### What's still missing — server-side prerequisite

`metriken-query-sql/src/views.rs::render_src_sql` currently projects
**only** rezolus-tagged columns into `_src` (the multi-source fix from
this session). Cachecannon's application-source columns
(`0::target_rate`, `0::bytes_rx`, …) are silently dropped from `_src`.
Even hand-written SQL like `SELECT timestamp::DOUBLE/1e9 AS t,
target_rate AS v FROM _src` would binder-error because `target_rate`
isn't a column of `_src`.

So before the KPI templates can carry SQL, **render_src_sql needs a
second projection pass for non-rezolus sources**. Options:

- **Option A (simplest):** also project non-rezolus columns under
  their canonical metric name. For cachecannon this means
  `"0::target_rate" AS target_rate` lands in `_src` alongside the
  rezolus columns. Risk: metric-name collision between rezolus and
  app-source (extremely unlikely — rezolus uses `cpu_usage` /
  `cpu_cycles` style, apps use domain-specific names — but worth a
  test).
- **Option B:** project under prefixed names like
  `target_rate__cachecannon`. KPI SQL would have to reference that
  exact form. More robust against collision; harder to write SQL by
  hand.

Recommended: **Option A**. Add a unit test
(`render_src_sql_multi_source_includes_application_columns`) and a
collision test (synthetic parquet with `cpu_usage` from both rezolus
and an app source) to lock in behaviour.

### The transcription itself — cachecannon as worked example

Template at `config/templates/cachecannon.json`. 14 KPIs, all
operating on the cachecannon-source columns:

| Type | Title | PromQL | SQL shape |
|---|---|---|---|
| gauge | Target Rate | `target_rate{source="cachecannon"}` | `SELECT timestamp::DOUBLE/1e9 AS t, target_rate AS v FROM _src` |
| delta_counter | Request Rate | `sum(irate(requests_sent{source="cachecannon"}[5s]))` | `SELECT timestamp::DOUBLE/1e9 AS t, irate_1s(requests_sent, timestamp) AS v FROM _src` |
| delta_counter | Response Rate | `sum(irate(responses_received{source="cachecannon"}[5s]))` | `SELECT timestamp::DOUBLE/1e9 AS t, irate_1s(responses_received, timestamp) AS v FROM _src` |
| delta_counter | Bytes Received Rate | `sum(irate(bytes_rx{source="cachecannon"}[5s]))` | `SELECT timestamp::DOUBLE/1e9 AS t, irate_1s(bytes_rx, timestamp) AS v FROM _src` |
| delta_counter | Bytes Sent Rate | `sum(irate(bytes_tx{source="cachecannon"}[5s]))` | …same pattern |
| histogram | Response Latency | `response_latency{source="cachecannon"}` | `WITH d AS (SELECT timestamp, h2_delta("response_latency:buckets", LAG("response_latency:buckets") OVER (ORDER BY timestamp)) AS d FROM _src) SELECT timestamp::DOUBLE/1e9 AS t, q::VARCHAR AS quantile, h2_quantile(d, q)::DOUBLE AS v FROM d, (VALUES (0.5),(0.9),(0.99),(0.999),(0.9999)) qs(q) WHERE d IS NOT NULL` |
| histogram | GET Latency | `get_latency{source="cachecannon"}` | same pattern, `get_latency:buckets` |
| histogram | SET Latency | `set_latency{source="cachecannon"}` | same pattern, `set_latency:buckets` |
| delta_counter | Request Error Rate | `sum(irate(request_errors{source="cachecannon"}[5s]))` | `irate_1s(request_errors, timestamp)` |
| delta_counter | Connection Failure Rate | `sum(irate(connections_failed{source="cachecannon"}[5s]))` | `irate_1s(connections_failed, timestamp)` |
| delta_counter | Cache Hit Rate | `sum(irate(cache_hits{source="cachecannon"}[5s]))` | `irate_1s(cache_hits, timestamp)` |
| delta_counter | Cache Miss Rate | `sum(irate(cache_misses{source="cachecannon"}[5s]))` | `irate_1s(cache_misses, timestamp)` |
| delta_counter | GET Rate | `sum(irate(get_count{source="cachecannon"}[5s]))` | `irate_1s(get_count, timestamp)` |
| delta_counter | SET Rate | `sum(irate(set_count{source="cachecannon"}[5s]))` | `irate_1s(set_count, timestamp)` |

The mapping is mechanical. Each KPI's `sql` field gets one of three
templates above (gauge / irate_1s counter / h2_quantile histogram).
For multi-instance services (e.g. cachecannon with N load
generators), the irate query may need `SUM(v)` over the matching
columns — but for the canonical-alias projection, each metric folds
into one column anyway, so the bare form works.

Once cachecannon.json is done, the same pattern applies to the
other 9 templates. Total work: ~140 KPI entries across 10 files,
each one a 2–10 line SQL string. Probably 1 person-day with care.

### Verification

- `/service/cachecannon` page should render 14 KPI plots with real
  data (not placeholders).
- Compare against PromQL evaluator side-by-side: spin up live-mode
  with the same parquet (or use the WASM viewer's PromQL fallback
  path) and visually check the curves match.
- Add a `kpi_sql_emits_sql_query_when_present` test analogous to the
  existing `kpi_sql_none_emits_promql_query_only`.

---

## 2. Other carve-outs surfaced this session but not yet addressed

### a. Multi-rezolus aggregation in `_src`

`render_src_sql` handles single-rezolus + N application sources. It
does **not** handle parquets with **≥2 rezolus sources** (the
`_src_rezolus_combined` shape the WASM viewer builds). Doing so means
emitting `COALESCE(<src1>::col, 0) + COALESCE(<src2>::col, 0)` for
scalars and `h2_combine_lol([COALESCE(<src1>::col, []::UBIGINT[]),
…])` for histograms, per metric. Pattern source:
`site/viewer-sql/lib/duckdb-registry.js:290-348`.

Not blocking single-recording use; flags up only when someone
combines parquets from multiple rezolus agents.

### b. Multi-node selection on the SQL backend

Frontend `buildEffectiveQuery` injects `node="..."` only on PromQL
plots. With `BACKEND='sql'` the SQL path has no equivalent. For
single-node parquets this is invisible; for multi-node ones the
server returns aggregated data regardless of node-picker UI state.

The fix this session disabled `injectLabel` for SQL queries
(necessary — it produced syntactically-invalid SQL like
`SELECT{node="..."} 0::DOUBLE{node="..."} …`). Re-implementing node
filtering on the SQL side means either:
- An out-of-band server param (`?node=...`) that the server uses to
  wrap `_src` in a per-node view.
- Or per-metric `WHERE node = '<picked>'` clauses, requiring the
  dashboard SQL emitter to know which columns are node-tagged.

Probably a follow-on after the per-source view layer (#1 prerequisite)
since they share the "wrap `_src` in a subset view" mechanism.

### c. MCP migration

`src/mcp/` still on `Tsdb` + PromQL behind the `live-mode` feature
gate. The 5+ `Tsdb::load` call sites and the anomaly-detection /
correlation algorithms need their own SQL ports. Out of scope for
the viewer migration but on the broader migration roadmap.

### d. `Save as Report` column trim

`src/viewer/actions.rs::save_with_selection` forces `trim_columns =
false` for SQL-backed captures. The embed-only path still saves the
parquet with full columns + selection JSON in the footer.
`report-save::resolve_kept_columns` needs a SQL-side analog that
walks `SqlCapture::catalog().series_by_metric` instead of running
PromQL.

---

## 3. Found-this-session bug worth pinning with a regression test

`src/viewer/assets/lib/app.js` cgroup-section wrapper applies
`injectLabel(query, 'node', node)` to *every* `executeQuery` call.
With `BACKEND='sql'`, this mangles SQL like
`SELECT 0::DOUBLE AS t, name AS name, COUNT(*)::DOUBLE AS v FROM _cgroup_index …`
into
`SELECT{node="…"} 0::DOUBLE{node="…"} AS t, name{node="…"} AS name{node="…"} …` —
nonsense SQL that DuckDB binder-errors on, which the server's
"`not found in FROM clause` → empty matrix" fallback then silently
turns into "No cgroup data found" on the UI.

Fixed this session by skipping `injectLabel` when `query` begins
with `WITH` / `SELECT`. Worth a JS-side regression test in
`tests/*.mjs` once the broader injectLabel surface gets revisited.

---

## 4. Sparse-metric binder-error fallback — design tradeoff to revisit

`src/viewer/routes.rs::run_sql` translates two binder-error classes
(`No matching columns`, `not found in FROM clause`) into
`EMPTY_PROM_MATRIX`. This restored PromQL's "unknown metric →
empty series" behaviour, but it also silently swallows the cgroup
injectLabel bug above (and would swallow any future SQL-mangling
upstream of run_sql). Consider:

- Logging at `warn` level when the binder-error → empty translation
  fires, so unexpected occurrences are visible in the server log.
- Or narrowing the regex to also match the column name in the error
  message (so e.g. `not found in FROM clause: Some(name)` returns
  empty but `not found in FROM clause` with a value-keyword token
  surfaces as a real error).

---

## 4b. Three more bugs uncovered after the initial fix wave

All fixed this session, but worth pinning with regression tests:

- **`ViewerApi.setSelectedCgroups is not a function`** — same shape
  as B1. Server-side adapter at
  `src/viewer/assets/lib/viewer_api.js` was missing
  `setSelectedCgroups` (and `getSelectedCgroups`); the static viewer
  adapter has them for the duckdb-wasm registry. The cgroup_selector
  calls them when the user moves items between Aggregate/Individual.
  Fixed by adding no-op stubs; the actual selection state is now
  threaded through `setActiveCgroupPattern` (§4c).
- **`_cgroup_index.column_name` mismatch with `_src`** —
  `metriken-query-sql/src/views.rs::render_cgroup_index_sql`
  inserted `c.physical` (raw prefixed names like
  `rezolus-client::41x0`) for `column_name`, but on multi-source
  parquets `_src` projects columns under canonical aliases
  (`cgroup_cpu_usage/user/0`). The dashboard's
  `idx.column_name = u.col` JOIN never matched → 0 cgroup plots
  populated even after a valid selection. Fixed by passing the
  multi-source columns through `canonical_alias()` when building
  `_cgroup_index`, plus skipping non-rezolus rows (their `_src`
  projection is absent) and excluding the wasm-style
  infrastructure label keys from the `labels` MAP.
- **`__SELECTED_CGROUPS__` substitution missing on server build**
  — the cgroup_selector at
  `src/viewer/assets/lib/cgroup_selector.js:209` calls
  `executeQuery(plot.sql_query)` with the placeholder literal still
  in the SQL. On the WASM viewer the duckdb-wasm registry
  substitutes; on the server build the placeholder reached DuckDB
  and binder-errored (then swallowed by §4's fallback, surfacing as
  empty charts). Fixed by:
  - extending the cgroups-section `executePromQLRangeQuery`
    wrapper in `app.js:629` to call `substituteCgroupPattern`
    before send;
  - wiring `section_views.js`'s `setSelectedCgroups` callback to
    convert the picked names → SQL IN-list literal
    (`('name1','name2')`) and push it through
    `setActiveCgroupPattern`.
  Regression test idea: an end-to-end JS test that selects `/` on
  `cachecannon.parquet` and asserts ≥1 plot's `data` array becomes
  non-empty.

## 4c. Loading-indicator UI tweak

Section title showed `Foo (0)` immediately on navigation, then
updated to `Foo (N)` after each plot's data lands. The (0) phase
incorrectly read as "no charts in this section" rather than "still
loading". Now shows `Foo (…)` while `withData == 0 && total > 0`,
switches to `Foo (N)` after the first plot's data arrives. Truly
empty sections (no plots in the dashboard at all) still show
`(0)`. Minor UX polish — not a correctness fix.

---

## 5. Test additions that fell out of this session

Already landed:
- `h2_combine_lol_matches_variadic_udf_for_two_lists` (cross-backend
  parity)
- `h2_combine_lol_with_empty_outer_returns_empty_list`
- 3 `_cgroup_index` builder tests
- 4 `canonical_alias` / `render_src_sql` tests for multi-source

Worth considering:
- A frontend JS test that asserts `injectLabel` is NOT applied to
  SQL-shaped queries.
- An end-to-end test that confirms `/cgroups` renders the "1
  available" UI on `cachecannon.parquet` (would catch the bug fixed
  in §3 if it regresses).
