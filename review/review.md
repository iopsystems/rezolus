# Reviewing `yv/sql-testing` — rezolus side

Companion: `/work/metriken/review/review.md` (engine side).

This branch unifies every viewer mode (file / upload / A-B / live
agent), `rezolus mcp`, Save-as-Report column trim, and `parquet
annotate` onto DuckDB-driven SQL through
`metriken_query_sql::DuckDbBackend`, and **deletes the legacy
`metriken-query` crate** (Tsdb + PromQL evaluator + harness, ~10,914
LOC on the engine side) on the way out. The pivotal landings:
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
                        new(tsdb, templates)                  (live mode, cfg-gated; collapsed in C4)
                        new_empty(templates)                  (upload-only)
                        with_baseline_data(|&dyn DashboardData| ...)
                      const LIVE_BASELINE_DATA_SOURCE = "live:baseline"

  sql_capture.rs      SqlCapture { parquet_path, catalog,
                                   kind_by_metric, interval_seconds,
                                   time_range, source, version, filename }
                      impl DashboardData for SqlCapture

  live_ingest.rs      Snapshot → LiveSource bridge (~343 LOC).
                      Walks metriken_exposition::Snapshot, strips shape
                      metadata, calls canonical_column_name for shape-
                      identical _src column naming, then LiveSource::append.

  capture_registry.rs CaptureRegistry { baseline, experiment: RwLock<Option<CaptureSlot>> }
                      enum CaptureBackend { Sql(Arc<RwLock<SqlCapture>>),
                                            #[cfg(live-mode)] Live(Arc<RwLock<Tsdb>>) }
                      The Live variant is residue for non-query metadata
                      reads; it collapses in C4 once a placeholder
                      DashboardData replaces Tsdb in that role.

  routes.rs           data_source_for(state, capture) resolves the live
                      key ahead of any parquet path, so /api/v1/query{,_range}
                      dispatch is uniform across modes.
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
                      connect_agent / ingest_loop / reset_tsdb: cfg-gated live-mode
                      ingest_loop currently dual-feeds Tsdb + LiveSource;
                      Tsdb side drops in C4.

crates/prom-matrix/   arrow_to_prom_matrix(&[RecordBatch]) -> String       (native)
                      js_arrow_to_prom_matrix(&JsValue)  -> JsValue        (wasm)
                      Shared pub(crate) emit_prom_matrix_json envelope:
                      the JSON shape can't drift between server and browser.

crates/dashboard/
  data.rs             DashboardData trait — implemented by SqlCapture AND
                      (cfg(live-mode)) Tsdb. Generators read schema through
                      this; query execution is elsewhere.
  sql.rs              16 SQL builder helpers (rate_5m_total, irate_total,
                      hist_percentile_series, cpu_pct_total, cgroup_irate_total,
                      …) that the per-section dashboard generators in
                      dashboard/*.rs call to produce each plot's `sql` argument.
                      ~130 plot call sites route through these helpers.
  service_extension.rs Kpi.sql: Option<String>  (added; templates carry None
                                                  for now — see carve-outs)
  service.rs          plot_promql_with_sql{,_full} when kpi.sql is Some;
                      plot_promql{,_full} otherwise.
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
holdout) closed in C2-C5 of this branch; carve-outs 3 and 4 are
unchanged from pre-branch.

### 1. Service-extension KPI templates: gauges/counters on SQL, histograms still PromQL-shaped

Per-source views (`_src_<source>`) now exist on the engine side and
128 of 218 template KPIs ship a `sql` field alongside their PromQL
`query`. Plot bodies that need `plot.sql_query` no longer see `null`
for gauges and counters; the SQL-only frontend renders them through
the SQL pipeline. The remaining 90 KPIs split into histogram
percentile fan-outs (skipped pending engine-side `grouping_power`
plumbing into the substitution layer), compound expressions
(`A - B`, `A / B` — non-trivial to translate), regex-multi-value
selectors (`finished_reason=~"stop|length"` — needs an aggregate
column expression), and 9 placeholder KPIs with no `query` field.

**The architecture (option 2 from the previous version).** Each
parquet column carries a `source` label in its field metadata.
`metriken-query-sql/src/views.rs::render_per_source_views_sql`
groups columns by that label value (not by `<prefix>::`) and emits
`CREATE OR REPLACE TEMP VIEW _src_<source>` per source. Single-
instance sources get a straight projection; multi-instance sources
of the same name aggregate at each timestamp (`COALESCE + sum` for
scalars, `h2_combine_lol` for histograms — the same shape
`_src_rezolus_combined` uses on the wasm side for multi-rezolus).

View names follow the wasm `viewNameForSource` rule (non-
`[a-zA-Z0-9_]` chars become `_`) so `vllm-prefill` resolves to
`_src_vllm_prefill` on both backends. The shared `view_name_for_source`
helper is exposed at `metriken_query_sql::view_name_for_source`.

**Template authoring.** KPI `sql` carries the placeholder `{{view}}`
where the per-source view would otherwise be named. The dashboard
emitter (`crates/dashboard/src/dashboard/service.rs::substitute_view`)
resolves it to `_src_<service_name>` at emit time, using the
template file's `service_name` field; `parquet annotate` performs
the same substitution before running the SQL through `DuckDbBackend`
to validate KPI data presence. The placeholder is also exposed via
the public `dashboard::substitute_view` helper so consumers don't
inline the sanitisation rule.

**Transcription via script.** `/tmp/transcribe_kpis.py` (committed
state lives in templates, not as a recurring tool) walks every
KPI's PromQL and emits SQL for three patterns:

  - `metric{labels}` → `SELECT t, CAST("<col>" AS DOUBLE) AS v FROM {{view}} ORDER BY t`
  - `sum(metric{labels})` → same shape (sums collapse to a single
    matching column once `source` is implied by the view).
  - `sum(irate(metric{labels}[Ns]))` → `SELECT t, irate_1s("<col>", timestamp) AS v FROM {{view}} ORDER BY t`

`<col>` is built via the same `canonical_alias` rule the engine
uses: metric name + non-source value-label values appended as
`/<v>`, non-numeric first then numeric. Histograms, regex selectors,
and compound expressions don't translate — `kpi.sql` stays absent
and the legacy PromQL path renders them.

**Verification end-to-end.** `parquet annotate` against
`site/viewer/data/cachecannon.parquet` reports 11/14 KPIs bind
through the SQL pipeline (the 3 misses are histograms — sql=None);
against `sglang_gemma3.parquet` with the `llm-perf` template,
6/13 KPIs bind (the misses are 5 histograms + the regex Error Rate
KPI + a sparse-metric absent from the recording). No false
positives: every KPI marked `available=true` produced data through
the new `_src_<source>` view.

**What still lands as deferred Phase 2 follow-ups:**

  - Histogram percentile transcription needs the engine to plumb
    each histogram metric's `grouping_power` into the substitution
    layer (or a per-metric `hist_p_p(buckets, ts, q, p)` macro
    invocation). 33 KPIs across the templates.
  - Compound expressions (ratio, subtraction) — write each by hand
    using the layer-A SQL emitters. ~13 KPIs.
  - Regex-multi-value selectors — UNPIVOT + `COLUMNS('regex')` or
    explicit SUM across hand-listed columns. ~6 KPIs.
  - A parity walker adapting `metriken-query/examples/sql_vs_promql.rs`
    to compare PromQL vs SQL output per (KPI, fixture). Currently the
    only check is `parquet annotate`'s non-empty data assertion.

**Status (current commit):** 128/218 KPIs (59%) carry SQL via the
auto-transcription. The remaining 90 stay PromQL-only — the
dashboard emitter falls back to the PromQL path when `kpi.sql`
is `None`, so live-mode and the legacy PromQL viewer continue
to render them.

### 2. Multi-node selection doesn't filter server-side

The top-nav node picker injects `node="..."` only on the PromQL
side; the SQL backend has no equivalent. WASM viewer has the
same gap. On multi-node parquets the server returns aggregated
data regardless of selection. Future work; not unique to this
branch.

### 3. Multi-rezolus aggregation

Two-or-more rezolus sources in one parquet is not yet aggregated
server-side. The `COALESCE + sum` / `h2_combine_lol` projection
shape that the WASM viewer's `_src_rezolus_combined` builds isn't
replicated in `SqlCapture::open`. Single-rezolus + arbitrary
application-source captures (the cachecannon shape) work
end-to-end.

---

## End-state plot coverage

End-to-end browser audit on `demo.parquet` and
`cachecannon.parquet` across all 12 built-in dashboard pages:

- **254 / 254 plots bind** (no DuckDB binder errors).
- Sparse-metric plots (a metric not in this parquet) render as
  empty matrices, matching the legacy "unknown metric → empty
  series" UX from the original Tsdb model.
- Cgroup section on cachecannon: 27 / 48 plots populate after
  selecting a cgroup. The 21 empties are sparse-metric (no
  `cgroup_cpu_throttled*` recorded; NULL-name rows lack per-`op`
  labels) — not binder errors.

The cross-source aggregation (carve-out 4) is the only remaining
_structural_ gap. Everything else is "this parquet doesn't carry
that metric".

---

## Recently landed (post-doc commits not in the original review)

Three commits landed after the previous narrative refresh. Each is
the practical resolution of an item formerly in _Removing Tsdb
entirely_.

### `a06c6ab` — MCP migrated onto `DuckDbBackend`

`src/mcp/` no longer requires `Tsdb` + PromQL. The five subcommands
(`describe-recording`, `describe-metrics`, `detect-anomalies`,
`analyze-correlation`, `query`) and the stdio server run through
`metriken_query_sql::DuckDbBackend` via `SqlCapture`. `mod mcp;` is
unconditional in `src/main.rs`; MCP builds in `--features sql-only`.

New `src/mcp/backend.rs` (488 LOC) is the shared helper layer:

- `open_capture(path) -> (Arc<DuckDbBackend>, SqlCapture)` —
  parquet open + warm pool, same shape the file-mode viewer uses.
- `batches_to_series(batches) -> Vec<Series>` — Arrow `t/v/labels…`
  projection mirroring the `prom-matrix` contract; NULL / non-finite
  `v` rows drop, matching the viewer's row-dropping rules.
- `counter_sum_rate_sql`, `gauge_sum_sql`, `histogram_quantile_sql`
  — canonical SQL builders for the three metric kinds, using
  `SHARED_MACROS` (`irate_1s`, `hist_p`, `h2_combine_lol`).

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

Was carve-out 4 in the previous doc. The trim path no longer needs
a `Tsdb`. New `resolve_kept_columns_sql(payload, catalog, side)` in
`crates/report-save/src/lib.rs` resolves the keep-set from a
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

`src/viewer/actions.rs::save_single_dispatch` and
`save_combined_ab_dispatch` route SQL-backed baselines (and
SQL-backed experiments for A/B) through
`save_single_parquet_sql` / `save_combined_ab_tarball_sql`. The
legacy Tsdb branch stays for live-mode. `metriken-query/legacy` is
declared on report-save's `live-mode` feature only, so
`cargo build -p report-save` works standalone with the SQL trim
path.

4 new tests pin the resolver's metric-name expansion, direct-column
matching, word-boundary safety, and `trim_columns=false` bypass.

After the merge with `main` (commit `9b628c4`), the four save
entrypoints (`save_single_parquet`, `save_single_parquet_sql`,
`save_combined_ab_tarball`, `save_combined_ab_tarball_sql`) all
thread the `events: Vec<Event>` field from `ReportPayload` through
to the parquet footer via a `KEY_EVENTS` KeyValue. Closed the
combined-AB `None` placeholder main's events feature left behind.
`events_payload_json` returns `None` for empty events (byte-identical
output to pre-events captures); both trim and embed paths
`retain(|kv| kv.key != KEY_EVENTS)` before pushing so re-saves don't
duplicate. 5 added/updated tests cover the round-trip on both engines.

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
`LiveSource::append` per snapshot. The Snapshot is cloned for the
live path so the legacy `tsdb.ingest` continues to consume the
original — Snapshot accessors take `&mut self` and `std::mem::take`
the inner Vecs. Clone of ~tens of KB at 1 Hz is negligible; the
clone drops alongside the Tsdb feed in C4 of this branch.

`f5482ff` adds chromium smoke `--live <agent-url> [--ingest-wait N]`
mode for end-to-end coverage.

**Test coverage added.** L1: 10 tests in
`metriken-query-sql/tests/live.rs` (round-trip, schema growth,
NULL semantics, cgroup_index rebuild, timestamp snap, per-source
view, concurrent read+write, bad-SQL surfacing). L2: 5 tests in
`metriken-query-sql/src/live.rs::tests` (cross-engine parity —
replay parquet rows into a LiveSource, assert byte-identical Arrow
output for SELECT/COUNT/MIN/MAX/SUM/irate_1s/h2_*). L3: 5 tests in
`src/viewer/routes.rs::live_route_tests` (data_source_for dispatch,
metadata time-range advances as snapshots accumulate). L4: 2 tests
in `src/viewer/live_ingest::tests` (snapshot round-trip, cgroup
label propagation).

What's NOT migrated: `validate_service_extensions`. That's C3 of
this branch.

---

## Tests

| Command                            | Covers                                                             |
| ---------------------------------- | ------------------------------------------------------------------ |
| `cargo test --bin rezolus`         | Binary including viewer, actions, MCP backend + subcommands.       |
| `cargo test -p dashboard`          | DashboardData impls, plot emitters, sql_snapshots.                 |
| `cargo test -p prom-matrix`        | Arrow → Prometheus matrix projection (incl. NaN/Inf row-dropping). |
| `cargo test -p viewer-sql`         | WASM crate's SHARED_MACROS parity against the native engine.       |
| `cargo test -p metriken-query-sql` | UDFs, backend pool, LiveSource parquet↔live parity. **Run from `/work/metriken/`** — the crate lives in the sibling repo, not in the rezolus workspace. |
| `cargo test -p report-save`        | Column-trim resolvers (SQL via `MetricCatalog`; live-mode via `Tsdb` — live-mode path drops in C2). |
| `cargo test --test mcp_cli`        | End-to-end MCP CLI smoke against `target/debug/rezolus` + `demo.parquet` (auto-skips when fixtures or binary are missing). |
| `node --test tests/*.mjs`          | Frontend pure-JS tests.                                            |
| `bash tests/viewer_smoke.sh`       | End-to-end (upload / file / A-B / proxy). Requires `jq`.           |
| `bash scripts/viewer_chromium_smoke.sh <parquet>` | Headless-Chromium per-section smoke. Drives `rezolus view <parquet>` and navigates to every section in `/api/v1/sections`, then asserts each one either rendered a chart (`.chart-wrapper` with svg/canvas), reserved an `_unavailable` placeholder (`.chart-unavailable`), or displayed a "Charts with no data / Unavailable KPIs" notes section (`.section-notes`). Captures per-section screenshots, console errors, failed network requests, and HTTP 4xx/5xx responses. Surfaces the *silent* failure mode the API-only `viewer_smoke.sh` cannot — section returns 200 but renders nothing. Requires `chromium`, `jq`, `python3`, `python websockets` (`pip install --user websockets`). |
| `bash scripts/viewer_chromium_smoke.sh --live <agent-url> [--ingest-wait N]` | Same per-section render check, against a running agent. Waits N seconds after viewer startup (default 5) so `_src` accumulates rows before sections are exercised. Post-LiveSource (`1d471cd` + `f5482ff`) live-mode renders the same dashboards as file mode. |

Frontend JS has 6 pre-existing failures: 5 in
`compare_node_filter.test.mjs` (PromQL-side `buildEffectiveQuery`
paths unreached on `BACKEND='sql'`) + 1 in
`wasm_viewer_histogram_kpis.test.mjs` (ENOENT on the retired
pre-2026 `site/viewer/pkg/wasm_viewer.js` artifact path). Both
files target code paths that no longer exist on the server-backed
viewer; they're slated for deletion in C4 of this branch alongside
the rest of the Tsdb plumbing.

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

This section was the pre-deletion roadmap. The deletion landed in
C2-C5 of this branch; what follows is a retrospective summary of
how the migration unfolded so a reviewer can read the commit
history with context.

End state:
- `cargo tree -p rezolus | grep 'metriken-query '` → empty.
- `metriken-query` crate deleted from `/work/metriken/`.
- No `Tsdb`, no `QueryEngine`, no `metriken_query::*` import
  anywhere in rezolus.
- Single build: `cargo build --bin rezolus`. No `live-mode` /
  `sql-only` features.

C2-C5 sequence:
- **C2 — Phase 0 prune.** `parquet annotate` / `filter` un-gated
  (already SQL-driven). `report-save` Tsdb-flavoured trim path
  deleted (live captures route through `LiveSource::catalog()` +
  `save_*_sql` instead). `attach_experiment` (Tsdb variant) deleted
  as unreachable.
- **C3 — `validate_service_extensions` SQL migration.** Rewrote
  the KPI availability check to run `kpi.sql` through
  `DuckDbBackend`. KPIs without SQL (90/218 templates) keep their
  default `available = true` and render as `_unavailable`
  placeholder cards via `6054fe2`'s silent-render path — same UX
  shape as PromQL-empty rendering.
- **C4 — drop Tsdb in rezolus.** `CaptureBackend::Live(Tsdb)` →
  `CaptureBackend::Live(LiveCapture)` (new struct wrapping
  `Arc<LiveSource>` + per-metric schema cache). `ingest_loop`
  takes one handle instead of two; dropped the dual-feed.
  `crates/dashboard`'s `Tsdb` re-export and `impl DashboardData
  for Tsdb` gone; new `EmptyDashboardData` placeholder powers the
  schema-dump binary and test fixtures. All 83 `cfg(feature =
  "live-mode")` gates removed; both `live-mode` and `sql-only`
  features deleted. Dropped 6 dead JS tests
  (`compare_node_filter` × 5 + `wasm_viewer_histogram_kpis` × 1).
- **C5 — delete `metriken-query`.** Removed
  `metriken-query/src/{promql, tsdb, harness}` (~10,914 LOC),
  `queries.toml`, the three harness-feature examples, the crate's
  Cargo.toml, and the workspace member entry. `promql-parser`
  drops out of the dep graph.

