# Reviewing `yv/sql-testing` — rezolus side

Companion: `/work/metriken/review/review.md` (engine side).

This branch unifies every viewer mode (file / upload / A-B / live
agent), `rezolus mcp`, Save-as-Report column trim, and `parquet
annotate` onto DuckDB-driven SQL through
`metriken_query_sql::DuckDbBackend`, and **deletes the legacy
`metriken-query` crate** (Tsdb + PromQL evaluator + harness; ~13K
LOC in `src/{promql,tsdb,harness}`, ~16.7K LOC counting everything
that left with the crate) on the way out. The pivotal landings:
`LiveSource` (`17f1107` + `1d471cd`) put live captures on the same
DuckDB engine the parquet path uses; the C2-C5 commit sequence on
this branch then migrated `validate_service_extensions` to SQL,
collapsed the `CaptureBackend::Live(Tsdb)` variant into a
`LiveCapture`-backed shim, removed the `live-mode` / `sql-only`
feature seam, and deleted the `metriken-query` crate. **Single
build matrix:** `cargo build --bin rezolus`.

| Path                                                        | Engine                           | Status                                                                |
| ----------------------------------------------------------- | -------------------------------- | --------------------------------------------------------------------- |
| `rezolus view <parquet>` — file / upload / A-B / experiment | `DuckDbBackend` via `SqlCapture` | **migrated** |
| `rezolus view http://agent:4241` — live agent               | `DuckDbBackend` via `LiveSource` (query) + `LiveCapture` (schema) | **migrated** — single DuckDB engine across all viewer modes |
| WASM static viewer (`site/viewer/`)                         | duckdb-wasm                      | unchanged (already DuckDB) |
| MCP (`src/mcp/`)                                            | `DuckDbBackend` via `SqlCapture` | **migrated** — `src/mcp/backend.rs` is the shared loader/projector |
| `rezolus parquet annotate`                                  | `DuckDbBackend`                  | **migrated** — validates KPIs via SQL |
| Save-as-Report column trim                                  | `MetricCatalog` (via `SqlCapture` or `LiveSource::catalog()`) | **migrated** — single SQL-aware resolver |
| `validate_service_extensions`                               | `DuckDbBackend`                  | **migrated** — runs each `kpi.sql` through the same backend the query handlers use |

## Build matrix

`cargo build --bin rezolus` — the only build matrix. No `live-mode`
or `sql-only` features; `metriken-query` is gone from the
dependency tree (`cargo tree -p rezolus | grep 'metriken-query '`
is empty — only `metriken-query-sql` appears).

---

## Architecture (post-migration)

```
src/viewer/
  state.rs            AppState
                        sql_backend:  Arc<DuckDbBackend>      (one per process)
                        captures:     Arc<CaptureRegistry>
                        live_source:  RwLock<Option<Arc<LiveSource>>>   (live mode)
                        upload_mutex: Mutex<()>               (serializes uploads)
                        new_sql(capture, backend, templates)  (file mode)
                        new_empty(templates)                  (upload-only)
                        with_baseline_data(|&dyn DashboardData| ...)
                      const LIVE_BASELINE_DATA_SOURCE = "live:baseline"
                      Three constructors: `new_sql` (file/upload/A-B),
                      `new_live` (live-agent), `new_empty` (upload-only).
                      No `new(tsdb, …)` — the Tsdb-flavoured init was
                      collapsed in C4.

  sql_capture.rs      SqlCapture { parquet_path, catalog,
                                   kind_by_metric, interval_seconds,
                                   time_range, source, version, filename }
                      impl DashboardData for SqlCapture

  live_capture.rs     LiveCapture { live: Arc<LiveSource>,
                                    schema cache, source, version, filename }
                      impl DashboardData for LiveCapture — the
                      `DashboardData` shim for the live-agent baseline
                      slot. Reads route through to the shared LiveSource;
                      schema-reflection reads use the cached observations.

  live_ingest.rs      Snapshot → LiveSource bridge (~358 LOC).
                      Walks metriken_exposition::Snapshot, strips shape
                      metadata, calls canonical_column_name for shape-
                      identical _src column naming, then LiveSource::append.

  capture_registry.rs CaptureRegistry { baseline, experiment: RwLock<Option<CaptureSlot>> }
                      enum CaptureBackend { Sql(Arc<RwLock<SqlCapture>>),
                                            Live(Arc<RwLock<LiveCapture>>) }
                      LiveCapture wraps Arc<LiveSource> plus a per-metric
                      schema cache so DashboardData reads on live captures
                      see what's been observed so far. Single-feed: no
                      Tsdb anywhere (de77459).

  routes.rs           data_source_for(state, capture) at routes.rs:523
                      resolves the live key ahead of any parquet path, so
                      /api/v1/query{,_range} dispatch is uniform across modes.
                      run_sql is async; the backend call + Arrow projection
                      run under tokio::task::spawn_blocking so 20+ parallel
                      chart fetches don't starve the runtime (7fc2f4d).
                      /api/v1/section_status is a server-driven sidebar
                      gating endpoint (d048379 + f47cbba + 69ff6b5 + 87a8aae).
                      Binder errors ("No matching columns" / "not found in
                      FROM clause") → EMPTY_PROM_MATRIX, restoring legacy
                      "unknown metric → empty series" UX.

  actions.rs          ingest_baseline_from_path: SqlCapture::open + atomic swap
                      attach_experiment / detach_experiment: SqlCapture-backed
                      ingest_loop: single-feeds the LiveCapture via
                      live_ingest::ingest_snapshot. No cfg gates; the
                      Tsdb dual-feed arm dropped in C4 (de77459).

crates/prom-matrix/   arrow_to_prom_matrix(&[RecordBatch]) -> String       (native)
                      js_arrow_to_prom_matrix(&JsValue)  -> JsValue        (wasm)
                      Shared pub(crate) emit_prom_matrix_json envelope:
                      the JSON shape can't drift between server and browser.

crates/dashboard/
  data.rs             DashboardData trait — implemented by SqlCapture
                      (file/upload/A-B; src/viewer/sql_capture.rs:132),
                      LiveCapture (live-agent path; src/viewer/live_capture.rs:116),
                      and EmptyDashboardData (the schema-dump binary +
                      test fixtures; data.rs:65). Generators read schema
                      through this; query execution is elsewhere.
  sql.rs              852 LOC, 21 `pub fn` builder helpers
                      (rate_5m_total, irate_total, hist_percentile_series,
                      cpu_pct_total, cgroup_irate_total, cgroup_irate_by_name,
                      cgroup_ratio_by_name, bucket_heatmap_sql,
                      quantile_spectrum_sql, percentile_kpi_sql,
                      multi_percentile_kpi_sql, …). ~170 plot call sites in
                      dashboard/*.rs route through these. Snapshot tests
                      in `tests/sql_snapshots.rs` (25 snapshots).
  service_extension.rs Kpi.sql: Option<String>  (the sole query body —
                                                  no `query` field exists).
  service.rs          plot_sql{,_full} when kpi.sql is Some; KPIs without
                      SQL render as `_unavailable` placeholder cards. Owns
                      `{{p}}` substitution via `substitute_view_and_p`.
```

`Arc<DuckDbBackend>` lives once on `AppState`; handlers borrow it.
First request for a parquet pays cold-start (open + register
UDFs + macros + `_src` + `_cgroup_index`); subsequent requests hit
a warm slot.

---

## Where to spend attention

1. **The carve-outs below** — they're the active design questions.
   The mechanical move from `Tsdb` to `DuckDbBackend` is straightforward
   and has end-to-end test coverage.
2. **`src/viewer/routes.rs::run_sql`** — the binder-error → empty-
   matrix shim restoring legacy "unknown metric → empty series"
   UX. Concentrated complexity is here.
3. **`metriken-query-sql/src/backend.rs`** — the engine. See the
   companion metriken doc for the concurrency story.
4. **`crates/prom-matrix/`** — the projection layer shared between
   server and WASM. Single envelope formatter blocks JSON drift.
5. **The dashboard crate's `DashboardData` trait** — what makes
   the same dashboard generators drive both backends without
   forking.

---

## Carve-outs

The structural gaps that remain post-deletion. Carve-outs 1 (live-
agent query path) and 2 (validate_service_extensions PromQL
holdout) closed in C2-C5 of this branch. What was carve-out 1 in
prior drafts (the half-finished gauge/counter SQL transcription)
is the launching point for the next purge — see _PromQL purge —
planned (P1-P6)_ below. Carve-outs 3 and 4 are unchanged from
pre-branch.

### 1. Multi-node selection doesn't filter server-side

The top-nav node picker injects `node="..."` only on the PromQL
side; the SQL backend has no equivalent. WASM viewer has the
same gap. On multi-node parquets the server returns aggregated
data regardless of selection. Future work; not unique to this
branch.

### 2. Multi-rezolus aggregation

Two-or-more rezolus sources in one parquet is not yet aggregated
server-side. The `COALESCE + sum` / `h2_combine_lol` projection
shape that the WASM viewer's `_src_rezolus_combined` builds isn't
replicated in `SqlCapture::open`. Single-rezolus + arbitrary
application-source captures (the cachecannon shape) work
end-to-end.

---

## PromQL purge — completed (P1-P6)

Sequenced after the C5 `metriken-query` crate deletion. Six commits
landed on top of the C-series; the codebase has no remaining PromQL
surface anywhere — no helpers, no fallback emitters, no `Kpi.query`
field, no template `query` strings, no `Plot.promql_query`
serialization. The follow-on KPI transcription work then landed:
**209 / 218 KPIs now ship SQL** (commits `9b9165f`, `9daefc6`,
`cd92f18`). The remaining 9 unavailable KPIs all live in
`config/templates/inference-library.json` (no metric definitions
for the placeholder library template) and render as `_unavailable`
placeholder cards via the silent-render path.

### What was removed

  - **Frontend** — PromQL helpers (`rewriteCounterQuery`,
    `injectLabel`, `substituteCgroupPattern`,
    `executePromQLRangeQuery`) gone from `data.js`.
    `viewer_api.js::backend()` retired (always returned `'sql'`).
    Selection JSON wire format threads `sql_query` /
    `sql_query_experiment` (was `promql_query` /
    `promql_query_experiment`).

    `features/explorers.js` and the heatmap / quantile-spectrum
    fetch paths were initially deleted as part of the PromQL purge,
    then **restored as SQL-native rebuilds** before this branch
    ships:
    - `features/explorers.js` (242 LOC, commit `23237b8`) sends raw
      DuckDB SQL through `/api/v1/query_range?strict=true`.
    - `fetchHeatmapForPlot` / `fetchHeatmapsForGroups` /
      `fetchQuantileSpectrumForPlot` (`data.js:366,388,416`) call
      a new `ViewerApi.heatmapRange({...})` helper that hits
      `/api/v1/heatmap_range` (commits `1303170`, `fe35592`,
      `fa75499`); the endpoint resolves the metric + DuckDB SQL
      server-side and returns Arrow → matrix JSON.
  - **Dashboard emitter** (`crates/dashboard/src/plot.rs`) —
    `plot_promql{,_with_sql}{,_full}{,_with_descriptions}` family
    deleted. Replaced by `plot_sql{,_full}{,_with_descriptions}`,
    taking a single SQL string. ~170 call sites in
    `dashboard/*.rs` mechanically converted; KPIs without SQL are
    skipped instead of fallback-emitted. Description-attachment
    scan now walks the SQL string for metric names instead of the
    PromQL one.
  - **Plot struct** — `promql_query` and `promql_query_experiment`
    fields dropped. Plot JSON now carries `sql_query` exclusively.
  - **`Kpi { query, sql }`** — `query: String` field dropped from
    the struct; `sql: Option<String>` becomes the sole query body.
    `Kpi::effective_query` and `ServiceExtension::throughput_query`
    methods deleted (dead — the consumers either discarded the
    return value or relied on PromQL semantics that no longer
    apply). `CategoryKpi::effective_query` deleted for the same
    reason. The `throughput_query` plumbing through
    `DashboardContext` + `overview::generate` is gone.
  - **Template JSON** — `"query":` field stripped from every KPI
    in all 11 `config/templates/*.json` files. Templates ship SQL
    or nothing.
  - **`report-save` wire format** — `ReportEntry::promql_query` /
    `promql_query_experiment` renamed to `sql_query` /
    `sql_query_experiment` to match the new frontend payload
    shape.
  - **`routes.rs` section_status** — dropped the `promql_query`
    fallback branch in plot counting; only `sql_query` is read.
  - **`parquet annotate` / `parquet filter`** — switched from
    `extract_metric_selectors(&kpi.query)` to
    `extract_metric_selectors(kpi.sql.as_deref().unwrap_or(""))`.
    The helper itself is regex-based and works on either dialect;
    only the input source changed.
  - **Frontend section view** — the per-source "Queries" table now
    shows `kpi.sql` instead of `kpi.query`.
  - **Tests** — `tests/data_spectrum_capture.test.mjs` deleted
    (covered the deleted spectrum-fetch path).
    `compare_node_filter.test.mjs` and
    `wasm_viewer_histogram_kpis.test.mjs` (deleted earlier in C4)
    confirmed clean.
  - **Embed demo** — `src/viewer/assets/lib/embed/demo.html`
    rewritten to fetch SQL from `plot.sql_query` and POST to
    `/api/v1/query_range` directly (the `<rezolus-chart>` element
    itself was already SQL-agnostic; only the demo's fetch glue
    needed updating).

### What survives intentionally

  - **Comments that explain SQL semantics by comparing to PromQL
    behavior** — `crates/dashboard/src/sql.rs:56,66`,
    `crates/viewer-sql/tests/macros.rs:91`,
    `src/mcp/backend.rs:154-203`, etc. These are educational hooks
    for readers who arrive with PromQL intuition; keep.
  - **Test assertions that `promql_query` is absent from JSON** —
    `crates/dashboard/src/dashboard/{service,category}.rs::tests`
    and `crates/dashboard/src/plot.rs::tests` carry guards that
    fail loudly if the field ever reappears. Useful regression
    catch.
  - **JS helpers named `promqlResultTo*`** in `data.js` — they
    transform Prometheus-matrix-shape JSON results into chart-ready
    data, and the wire format is still Prometheus-matrix shape
    (projected from SQL via the `prom-matrix` crate). Rename to
    `matrixResultTo*` is a follow-up cosmetic pass.

### Verification

After P6, the gates pass:

| Gate                                      | Result                  |
| ----------------------------------------- | ----------------------- |
| `cargo build --bin rezolus`               | clean                   |
| `cargo test --bin rezolus`                | 191 pass / 0 fail       |
| `node --test tests/*.mjs`                 | 122 pass / 0 fail       |
| `grep -rn 'promql\|PromQL' src/ crates/`  | only comments / test guard assertions / `prom-matrix` crate name / `ReportEntry` serde aliases for old saves |
| `cargo tree -p rezolus \| grep promql`    | empty                   |

### Risks (now retired)

  - **Old saved reports** captured before the purge contain plots
    with `promql_query` strings. `ReportEntry` carries
    `#[serde(alias = "promql_query")]` and
    `#[serde(default, alias = "promql_query_experiment")]`
    (commit `c8ee16c`), so old reports deserialize cleanly — the
    field surfaces as `sql_query` on the new struct.
  - **External custom service templates** that ship only `query`
    (PromQL) — KPIs go all-placeholder post-purge. We own all 11
    templates in-tree; downstream custom-template authors should
    transcribe to SQL. CLAUDE.md notes the new requirement.

---

## End-state plot coverage

End-to-end browser audit on `demo.parquet` and
`cachecannon.parquet` across all 12 built-in dashboard pages:

- **All plots bind** (no DuckDB binder errors).
- Sparse-metric plots (a metric not in this parquet) render as
  empty matrices, matching the legacy "unknown metric → empty
  series" UX from the original Tsdb model.
- Service-extension coverage: **209 / 218 KPIs** ship SQL across
  the 11 in-tree templates. The 9 unavailable are all in
  `inference-library.json` (placeholder template with no metric
  bindings) and render as `_unavailable` cards.

Multi-rezolus aggregation (carve-out 2) is the only remaining
_structural_ gap. Everything else is "this parquet doesn't carry
that metric".

---

## Detailed commit notes

Commit-level detail on the major landings on this branch. Read these
when tracing specific changes in `git log`; otherwise the
Architecture and PromQL-purge sections above are the load-bearing
narrative.

### `a06c6ab` — MCP migrated onto `DuckDbBackend`

`src/mcp/` no longer requires `Tsdb` + PromQL. The five subcommands
(`describe-recording`, `describe-metrics`, `detect-anomalies`,
`analyze-correlation`, `query`) and the stdio server run through
`metriken_query_sql::DuckDbBackend` via `SqlCapture`. `mod mcp;` is
unconditional in `src/main.rs`; with the `sql-only` / `live-mode`
feature seams gone (C4), MCP builds in the single default
configuration.

New `src/mcp/backend.rs` (488 LOC) is the shared helper layer:

- `open_capture(path) -> (Arc<DuckDbBackend>, SqlCapture)` —
  parquet open + warm pool, same shape the file-mode viewer uses.
- `batches_to_series(batches) -> Vec<Series>` — Arrow `t/v/labels…`
  projection mirroring the `prom-matrix` contract; NULL / non-finite
  `v` rows drop, matching the viewer's row-dropping rules.
- `counter_sum_rate_sql`, `gauge_sum_sql`, `histogram_quantile_sql`
  — canonical SQL builders for the three metric kinds. They emit
  calls into the macro layer (`irate_1s`, `hist_p`,
  `h2_combine_lol`); `SHARED_MACROS` itself is registered upstream
  by `DuckDbBackend`.

`mcp::resolve_query_to_sql` auto-resolves bare metric names to SQL
by kind; SQL strings pass through unchanged. `mcp query` now takes
DuckDB SQL (breaking CLI change vs PromQL — the M-in-MCP clients
are LLMs, fluent in SQL). Output is the prom-matrix JSON shape.

The legacy `Tsdb`/`QueryEngine` helpers
(`format_recording_info`, `format_metrics_description`,
`calculate_correlation`, `extract_matrix_samples`, `detect_anomalies`,
`extract_time_series`, `auto_construct_query`,
`run_exhaustive_detection`, `format_query_result`/`format_metric`) plus
the unwired `discover_correlations.rs` and `resource_usage.rs` files
have been deleted from `src/mcp/` — `~2,100` LOC down across the
module. `src/mcp/` no longer references `metriken-query` or
`QueryEngine`/`Tsdb` in any build configuration.

22 in-process MCP tests pin the contract (open / extract /
SQL-builder / auto-resolve / detect / correlate / `execute_query`
shape including the empty-matrix fallback).

### `a761906` — Save-as-Report SQL-aware column trim

The trim path no longer needs a `Tsdb`. New
`resolve_kept_columns_sql(payload, catalog, side)` in
`crates/report-save/src/lib.rs:140` resolves the keep-set from a
`MetricCatalog` instead:

1. **Word-boundary metric-name match.** For every metric in the
   catalog, check whether the metric name appears as a word in the
   query text. If yes, all of that metric's physical columns are
   kept. Catches the PromQL `metric_name{labels}` shape and SQL
   metric references.
2. **Direct quoted-physical match.** For every physical column,
   check whether its quoted form (`"col"`) appears in the SQL.
   Catches direct-column references the metric-name pass misses.

Word-boundary matching prevents `cpu` from accidentally matching
`cpu_cycles`. `timestamp` and `duration` are always kept.
Over-keeping is preferred over under-keeping — the goal of trim is
footer size, not correctness.

`src/viewer/actions.rs::save_single_dispatch` (`actions.rs:799`)
and `save_combined_ab_dispatch` (`:826`) route SQL-backed baselines
(and SQL-backed experiments for A/B) through
`save_single_parquet_sql` / `save_combined_ab_tarball_sql`. Live
mode bypasses these dispatchers entirely — `save_with_selection`
(`actions.rs:662`) short-circuits to `snapshots_to_parquet` when no
parquet path is attached. No Tsdb branch survives, and
`report-save` has no feature flags (its `Cargo.toml` declares one
runtime dep on `metriken-query-sql`, no optionals).

The four `report-save` entry points
(`save_single_parquet_embed_only`, `save_single_parquet_sql`,
`save_combined_ab_tarball_embed_only`,
`save_combined_ab_tarball_sql`) all thread the
`events: Vec<Event>` field from `ReportPayload` through to the
parquet footer via a `KEY_EVENTS` KeyValue (added in main merge
`9b628c4`). `events_payload_json` returns `None` for empty events
(byte-identical output to pre-events captures); both trim and embed
paths `retain(|kv| kv.key != KEY_EVENTS)` before pushing so re-saves
don't duplicate. The 5 in-crate `#[test]`s cover the resolver
(metric-name expansion, direct-column match, word-boundary safety,
`trim_columns=false` bypass) plus the legacy `promql_query` serde
alias.

### `6054fe2` — Chromium per-section smoke + two silent-render fixes

`scripts/viewer_chromium_smoke.sh` (227 LOC bash + embedded Python
CDP driver) is a headless-Chromium harness that walks every section
in `/api/v1/sections` against a running `rezolus view <parquet>`
and asserts each section either rendered a real chart, reserved an
`_unavailable` placeholder, or displayed a `.section-notes` no-data
callout. Captures per-section screenshots + console errors + HTTP
4xx/5xx responses. Run with:

```bash
bash scripts/viewer_chromium_smoke.sh site/viewer/data/cachecannon.parquet
```

Requires `chromium`, `jq`, `python3`, and the python `websockets`
package (`pip install --user websockets`). The script picks the
more recently built debug/release binary automatically.

Adding it surfaced two latent silent-render bugs the API-only
`tests/viewer_smoke.sh` could not see because both produced 200 OK
responses with empty rendered output. Both landed around May 7–8
in the SQL-migration sprint and reinforced each other:

1. **`data.js::processDashboardData` stripped `_unavailable` KPIs.**
   The no-data filter loop at `src/viewer/assets/lib/data.js` ~L489
   checked `plotHasData(plot)` but not `plot._unavailable`. KPIs
   flagged unavailable upstream (`af867b5` "viewer: gate SQL/PromQL
   query selection") got dropped here before reaching the
   `chart-unavailable` placeholder render in `charts/chart.js`. The
   filter now mirrors `viewer_core.js::plotHasData`, keeping
   `_unavailable` plots through to the placeholder. Effect on a
   pre-`91ea72e` parquet (PromQL-only KPI templates embedded):
   service sections render placeholder slots instead of blank pages.

2. **`loadSection` cached the section payload before
   `data.metadata` was initialized.** `storeSectionResponse` makes
   a shallow copy. `processDashboardData` (introduced by `29b2359`
   "viewer: cache section structure synchronously, defer query
   fetch") then ran `data.metadata = data.metadata || {}` and wrote
   `unavailable_charts` there — but the cached copy's `metadata`
   stayed `undefined`, so the "Charts with no data" notes never
   rendered. `loadSection` (`src/viewer/assets/lib/app.js` ~L288)
   now initializes `data.metadata = {}` *before* caching so both
   objects share the metadata reference. Effect: sampler sections
   with no matching metrics render the explanatory list of missing
   charts instead of "(0)" + a void.

### `9f66ce1` — MCP CLI end-to-end tests

`tests/mcp_cli.rs` (276 LOC) spawns `target/debug/rezolus` as a
child process and exercises every MCP subcommand against
`site/viewer/data/demo.parquet`. Catches regressions in the thin
CLI shim (arg parsing → dispatch → print → exit) that the
in-process tests don't reach. The "DuckDB is actually being called"
question turns on `cli_query_runs_duckdb_sql`, which passes
`SELECT count(*) AS n FROM _src` and asserts on the real row count
of demo.parquet (302).

Covers `describe-recording`, `describe-metrics`, `query` (happy
path, `SHARED_MACROS` macros, malformed SQL),
`detect-anomalies` (bare-metric auto-resolution → SQL → analysis),
`analyze-correlation` (two bare metrics, max correlation
strength), and that `query --help` mentions DuckDB rather than
PromQL (CLI contract).

Auto-skips when the demo fixture is missing or when the binary
isn't built.

### `7fc2f4d` — viewer: spawn_blocking the SQL handler

`/api/v1/query{,_range}` used to run `state.sql_backend.run_sql`
inline on the tokio worker thread. With 20+ parallel chart fetches
on first page-load, the synchronous DuckDB call (warm-pool checkout
+ query execution + Arrow projection) starved the runtime, leaving
unrelated handlers (`/api/v1/sections`, `/api/v1/metadata`) waiting
behind the SQL queue.

`run_sql` is now `async`. The backend call + `arrow_to_prom_matrix`
projection both run under `tokio::task::spawn_blocking`. A
`JoinError` (task panic) maps to the same `sql_error` shape the
binder-error shim uses, so the frontend sees a uniform error
envelope. The DuckDB pool's per-slot Mutex remains the residual
serialization point — but it's now visible as queueing inside the
backend, not as runtime starvation outside it.

Measured: a 560-parallel-query smoke against `vllm.parquet` on a
5-core box dropped from multi-second worst-case wall time to ~700 ms
worst-case and ~360 ms mean.

### Sidebar section gating — `d048379` + `f47cbba` + `69ff6b5` + `87a8aae`

Pre-this-bundle, every section in the sidebar appeared live as soon
as the dashboard JSON loaded — even sections whose plots had no
matching metrics in the recording (which would render blank or as
`_unavailable` cards). Users had to click through to discover empty
sections.

`d048379` adds `/api/v1/section_status`, a server-driven endpoint
that ports `processDashboardData`'s plot-keep rules to the backend:
for each section, the server enumerates plots and reports
`withData` (a strict subset that returned non-empty rows) plus
total plot counts. `f47cbba` consumes the response client-side in a
`sectionStatus` map that persists past the 3-entry response-cache
eviction, gating sidebar entries: sections with `withData == 0`
render in gray. `69ff6b5` moves the `section_status` fetch under
the splash screen so the first paint reflects the gating; the
splash takes ~200 ms longer but there's no gray-flash. `87a8aae`
adds a cgroup edge-case carve-out: cgroup plots are deferred (only
materialised once a cgroup is selected), so they shouldn't count
toward `withData` when the parquet has no cgroups at all — the
status endpoint probes `_cgroup_index` row count up front.

### `17f1107` + `1d471cd` + `494b4fc` + `f5482ff` — live mode migrates to DuckDB (LiveSource)

The single largest landing on the branch post-doc-baseline. Closes
the live-agent query carve-out (formerly carve-out 1, formerly item
D of "Removing Tsdb entirely").

**Engine side (metriken).** `metriken_query_sql::LiveSource`
(`live.rs`, ~800 LOC) is an in-memory DuckDB table whose `_src`
grows column-by-column as new metrics appear. Single shared
`Mutex<Connection>` (DuckDB is `!Sync`); the parquet path's per-
slot pool model doesn't apply because each pool slot is an
independent in-memory DB — fine for the immutable parquet case,
wrong for a shared mutable table. Schema growth (`ALTER TABLE
_src ADD COLUMN`) plus single-row INSERT happens under one mutex
acquisition. `_cgroup_index` rebuilds via the same
`render_cgroup_index_sql` renderer the parquet path uses; `_src_<source>`
is a pass-through `SELECT *` view that auto-picks up new columns.

`DuckDbBackend` gains `create_live_source(data_source, source_name,
sampling_interval_ms) -> Arc<LiveSource>` and a `live_sources` map
parallel to the parquet `connections` map. `run_sql` /
`describe_parquet` / `invalidate` all check live sources first
before falling through to the parquet pool. Same API surface; the
backend dispatches.

`494b4fc` exposes `canonical_column_name` as a public free
function — mirrors the parquet path's `canonical_alias` rule so
external bridges (rezolus's `live_ingest.rs`) build `_src` column
names byte-identical to what the parquet path would produce.
Without this, rezolus agents emitting numeric label values (`"49"`,
`"24x0"`) ended up with `_src` columns named `49, 50, 51, ...` and
dashboard SQL targeting `^cpu_usage(/[^:]+)?$` matched nothing.

`1e5a2a2` (slot-acquisition try_lock fast-path) landed alongside,
unrelated to live but improves parquet-path concurrency. Slot
checkout now scans all pool slots non-blockingly via `try_lock`
starting at the round-robin pick; falls back to blocking only when
every slot is busy. Eliminates the "12 peers queued behind a slow
slot" pathology that was the residual after `spawn_blocking`.

**Consumer side (rezolus).** `1d471cd` adds
`live_source: RwLock<Option<Arc<LiveSource>>>` to `AppState` and
the constant `LIVE_BASELINE_DATA_SOURCE = "live:baseline"` (picked
to not collide with any parquet path). `init_live_mode` and
`/api/v1/connect` register a fresh source on `state.sql_backend`
and pass the `Arc<LiveSource>` to `ingest_loop` for dual-feed.
`data_source_for(state, capture)` resolves baseline to the live key
ahead of any parquet path, so `/api/v1/query{,_range}` dispatch is
uniform.

`src/viewer/live_ingest.rs` (~343 LOC) is the new bridge: walks
`metriken_exposition::Snapshot` (Counter / Gauge / Histogram),
strips shape-keys (`metric_type`, `unit`, `grouping_power`,
`max_value_power`) from labels, builds `LiveColumn` descriptors
canonicalised via `canonical_column_name`, and issues one
`LiveSource::append` per snapshot. Post-C4 the Tsdb dual-feed is
gone, so `ingest_snapshot` consumes the snapshot directly — no
clone, no `std::mem::take` dance.

`f5482ff` adds chromium smoke `--live <agent-url> [--ingest-wait N]`
mode for end-to-end coverage.

**Test coverage added.** L1: 10 tests in
`metriken-query-sql/tests/live.rs` (round-trip, time-range bounds,
schema growth, NULL semantics, cgroup_index rebuild, timestamp snap,
per-source view, concurrent read+write, bad-SQL surfacing). L2: 5 tests in
`metriken-query-sql/src/live.rs::tests` (cross-engine parity —
replay parquet rows into a LiveSource, assert byte-identical Arrow
output for SELECT/COUNT/MIN/MAX/SUM/irate_1s/h2_*). L3: 5 tests in
`src/viewer/routes.rs::live_route_tests` (data_source_for dispatch,
metadata time-range advances as snapshots accumulate). L4: 2 tests
in `src/viewer/live_ingest::tests` (snapshot round-trip, cgroup
label propagation).

`validate_service_extensions` (formerly the last PromQL holdout) migrated
to SQL in C3 (`6805ab4`) — runs each KPI's `sql` field through the same
`DuckDbBackend`. KPIs without `sql` (9/218 templates, all in
`inference-library.json`) keep their default `available = true` and
render as `_unavailable` placeholder cards via the silent-render
path (`6054fe2`).

---

## Tests

| Command                            | Covers                                                             |
| ---------------------------------- | ------------------------------------------------------------------ |
| `cargo test --bin rezolus`         | Binary including viewer, actions, MCP backend + subcommands.       |
| `cargo test -p dashboard`          | DashboardData impls, plot emitters, sql_snapshots.                 |
| `cargo test -p prom-matrix`        | Arrow → Prometheus matrix projection (incl. NaN/Inf row-dropping). |
| `cargo test -p viewer-sql`         | WASM crate's SHARED_MACROS parity against the native engine.       |
| `cargo test -p metriken-query-sql` | UDFs, backend pool, LiveSource parquet↔live parity. **Run from `/work/metriken/`** — the crate lives in the sibling repo, not in the rezolus workspace. |
| `cargo test -p report-save`        | Column-trim resolvers (SQL via `MetricCatalog`).                   |
| `cargo test --test mcp_cli`        | End-to-end MCP CLI smoke against `target/debug/rezolus` + `demo.parquet` (auto-skips when fixtures or binary are missing). |
| `node --test tests/*.mjs`          | Frontend pure-JS tests.                                            |
| `bash tests/viewer_smoke.sh`       | End-to-end (upload / file / A-B / proxy). Requires `jq`.           |
| `bash scripts/viewer_chromium_smoke.sh <parquet>` | Headless-Chromium per-section smoke. Drives `rezolus view <parquet>` and navigates to every section in `/api/v1/sections`, then asserts each one either rendered a chart (`.chart-wrapper` with svg/canvas), reserved an `_unavailable` placeholder (`.chart-unavailable`), or displayed a "Charts with no data / Unavailable KPIs" notes section (`.section-notes`). Captures per-section screenshots, console errors, failed network requests, and HTTP 4xx/5xx responses. Surfaces the *silent* failure mode the API-only `viewer_smoke.sh` cannot — section returns 200 but renders nothing. Requires `chromium`, `jq`, `python3`, `python websockets` (`pip install --user websockets`). |
| `bash scripts/viewer_chromium_smoke.sh --live <agent-url> [--ingest-wait N]` | Same per-section render check, against a running agent. Waits N seconds after viewer startup (default 5) so `_src` accumulates rows before sections are exercised. Post-LiveSource (`1d471cd` + `f5482ff`) live-mode renders the same dashboards as file mode. |

Frontend JS: `node --test tests/*.mjs` reports 122 pass / 0 fail.
`cargo test --bin rezolus` reports 191 pass / 0 fail. The
pre-existing failures from `compare_node_filter.test.mjs` (5) and
`wasm_viewer_histogram_kpis.test.mjs` (1) were dropped in C4 along
with the rest of the Tsdb plumbing.

---

## Verification recipe

```bash
# Default build + file mode
cargo build --bin rezolus
./target/debug/rezolus view site/viewer/data/demo.parquet --listen 127.0.0.1:9091 &
sleep 2
curl -s http://127.0.0.1:9091/api/v1/mode
curl -s http://127.0.0.1:9091/api/v1/metadata
curl -s "http://127.0.0.1:9091/api/v1/query_range" \
  --data-urlencode 'query=SELECT timestamp/1e9 AS t, "cpu_usage/user/0"::DOUBLE AS v FROM _src ORDER BY t LIMIT 3' \
  --data-urlencode 'start=0' --data-urlencode 'end=99999999999' --data-urlencode 'step=1' -G
pkill rezolus

# MCP smoke
./target/debug/rezolus mcp query site/viewer/data/demo.parquet 'SELECT count(*) AS n FROM _src'

# Live-mode smoke — requires an agent on :4241
sudo ./target/release/rezolus config/agent.toml &
REZOLUS_NO_OPEN=1 ./target/debug/rezolus view http://localhost:4241 --listen 127.0.0.1:9091 &
sleep 5   # let _src accumulate
curl -s "http://127.0.0.1:9091/api/v1/query_range" \
  --data-urlencode 'query=SELECT timestamp/1e9 AS t, "cpu_usage/user/0"::DOUBLE AS v FROM _src ORDER BY t LIMIT 3' \
  --data-urlencode 'start=0' --data-urlencode 'end=99999999999' --data-urlencode 'step=1' -G
# Should return matrix JSON (not capture_not_found).
pkill rezolus
```

Open <http://127.0.0.1:9091> for the full dashboard rendered via
SQL against DuckDB.

---

## Tsdb removed — historical roadmap

The C2-C5 deletion sequence as a reference for `git log` archaeology.

End state:
- `cargo tree -p rezolus | grep 'metriken-query '` → empty.
- `metriken-query` crate deleted from `/work/metriken/`.
- No `Tsdb`, no `QueryEngine`, no `metriken_query::*` import in
  rezolus.
- Single build: `cargo build --bin rezolus`. No `live-mode` /
  `sql-only` features.

Sequence:
- **C2 — Phase 0 prune** (`0ab61e0`). `parquet annotate` / `filter`
  un-gated; `report-save`'s Tsdb-flavoured trim path deleted; live
  captures short-circuit `save_with_selection` to
  `snapshots_to_parquet`; unreachable `attach_experiment` Tsdb
  variant removed.
- **C3 — `validate_service_extensions` SQL migration** (`6805ab4`).
  KPI availability check now runs `kpi.sql` through `DuckDbBackend`.
- **C4 — drop Tsdb in rezolus** (`de77459`).
  `CaptureBackend::Live(Tsdb)` → `Live(LiveCapture)`; `ingest_loop`
  single-feed; new `EmptyDashboardData` powers schema-dump + tests;
  83 `cfg(feature = "live-mode")` gates removed; both features
  deleted; 6 dead JS tests dropped.
- **C5 — delete `metriken-query`** (`6f072b5` / `b5f5574`).
  Removed `metriken-query/src/{promql, tsdb, harness}` (~13k LOC);
  `queries.toml`, three examples, Cargo.toml, workspace member
  entry — ~16.7k LOC total. `promql-parser` drops out of the dep
  graph.

