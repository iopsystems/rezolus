# Reviewing `yv/sql-testing` — rezolus side

Companion: `/work/metriken/REVIEWING.md` (engine side).

This branch finishes the migration of **`rezolus view <parquet>`**
off the in-memory `metriken_query::Tsdb` + PromQL pipeline and onto
DuckDB-driven SQL through `metriken_query_sql::DuckDbBackend`. The
WASM static viewer was already on DuckDB; this branch brings the
server-backed viewer to parity.

| Path | Engine | Status |
|---|---|---|
| `rezolus view <parquet>` — file / upload / A-B / attach-experiment | `DuckDbBackend` via `SqlCapture` | **migrated** |
| `rezolus view http://agent:4241` — live agent | `Tsdb` + PromQL | **carve-out**, behind `live-mode` feature (default-on) |
| WASM static viewer (`site/viewer/`) | duckdb-wasm | unchanged (already DuckDB) |
| MCP (`src/mcp/`) | `Tsdb` + PromQL | **carve-out**, gated by `live-mode` feature |
| `rezolus parquet annotate` | `DuckDbBackend` | **migrated** — validates KPIs via SQL |

`cargo build --bin rezolus` (default) and
`cargo build --bin rezolus --no-default-features --features sql-only`
both clean. `cargo tree --no-default-features --features sql-only`
contains no `metriken-query` entry — only `metriken` (registration
core) and `metriken-exposition` (parquet I/O), both consumed by
recorder / exporter / agent rather than the viewer. Default build
retains `metriken-query` exclusively for the live-mode path.

## Branch shape

81 commits ahead of `origin/main`, **+8,560 / −1,905 across 94
files** (`git diff --shortstat origin/main...HEAD`,
`git rev-list --count origin/main..HEAD`). Includes the 10
review-pass commits documented under "Review pass" below, plus
a second 6-commit pass (also tabled below) that closes the
four pre-merge blockers — `ViewerApi.registry`, multi-source
canonical aliasing, `_cgroup_index` server-side, and the
`h2_combine` cross-backend mismatch — uncovered by the
browser-driven audit captured in "Known regressions / deferred
work".

Migration arc (most recent first; see `git log origin/main..HEAD`
for the full list):

| Commit | Stage | What landed |
|---|---|---|
| `7124c4a` | 9 | `parquet annotate` validates KPIs via SQL through `DuckDbBackend`. |
| `659424e` | fixup | `sql: None` on the two `Kpi` test-helper literals. |
| `2b99c41` | 10 | `live-mode` feature gate: every Tsdb-touching path conditional; SQL-only build drops `metriken-query`. Touches `parquet_tools/mod.rs` to gate `mod annotate` / `mod filter` (combine stays unconditional — recorder needs it). |
| `fd285bb` | 8 | `Kpi.sql: Option<String>` field on the service-extension schema; `service.rs` emits `plot_promql_with_sql{,_full}` when present. **Templates not yet transcribed** — see *Known regressions*. |
| `2224fc3` | 6 | File / upload / A-B / attach-experiment paths flip from `Tsdb::load` to `SqlCapture::open`. `CaptureSlot.backend` is now a `RwLock<CaptureBackend>` (Sql vs Live) so upload can swap it in place. `with_baseline_data(closure)` abstracts the read-guard for callers that just want `DashboardData`. |
| `9972ba7` | 5 | `CaptureBackend` enum (Sql, Live) on `CaptureSlot`. |
| `70d71fa` | 4 | `LazySectionStore::get_or_generate` takes `&dyn DashboardData`. |
| `f9b392b` | 3 | `/api/v1/query{,_range}` accept SQL via `state.sql_backend.run_sql(...)` and project Arrow→matrix JSON. `viewer_api.js` flips `BACKEND='sql'`. |
| `266bba4` | 2 | `SqlCapture` (file-mode capture handle); `DashboardData` impl reads from `MetricCatalog` + cached scalar metadata. |
| `68daebd` | 1 | `crates/prom-matrix/` extracted — shared Arrow→Prometheus matrix projection; native `arrow_to_prom_matrix(&[RecordBatch])` for the server, WASM `js_arrow_to_prom_matrix(JsValue)` moved out of `viewer-sql`. |
| `bfc66fc` | 0 | Re-add `metriken-query-sql` workspace dep; `Arc<DuckDbBackend>` on `AppState`. |

The migration plan that produced this sequence lives at
`/home/yurivish/.claude/plans/draft-a-migration-plan-temporal-turing.md`.

A post-merge review pass added 10 more commits — doc accuracy
fixes, architectural simplifications (Bucket B), and edge-case
fixes with tests (Bucket C):

| Commit | Bucket | What |
|---|---|---|
| `6bfb960` | C8 | viewer wires `DuckDbBackend::invalidate` on detach + baseline-replace. |
| `f264729` | C8 | upstream `metriken-query-sql`: `DuckDbBackend::invalidate(path)` evicts pool entry; covers detach/reattach pool-staleness. |
| `64dc788` | C7 | `AppState::upload_mutex` serializes concurrent uploads — prevents inconsistent post-swap snapshot. Concurrency test included. |
| `e71bc87` | C9 | `prom-matrix` drops NaN/+Inf/-Inf rows as Prometheus gaps (`f64::to_string()` would otherwise produce invalid `"NaN"`/`"inf"` literals). 8 new projection tests. |
| `89d8251` | C6 | `ingest_baseline_from_path` clears `state.live` after the SQL swap — fixes a reachable `.expect("live mode baseline is Tsdb-backed")` panic on `/api/v1/reset` post-upload. Regression test included. |
| `12f555a` | B2 | Drop the inner `RwLock<CaptureBackend>` on `CaptureSlot`. Whole-slot replacement via `RwLock<Option<CaptureSlot>>` already serialised swaps. |
| `2bf7711` | B3 | Drop `baseline_sql()` (one caller, internal). `with_baseline_data` reads the registry directly. |
| `fe6bdc7` | B4 | `CaptureRegistry::new(Option<CaptureBackend>)` unified factory; baseline becomes `Option<CaptureSlot>`; `SqlCapture::empty()` placeholder is gone. |
| `3436f64` | — | Fixup: `sql: None` on a missed `Kpi` test literal in `dashboard::dashboard::category::tests`. |
| `e826092` | B5 | Collapse `save_*_dispatch` cfg-pairs into single functions with internal `#[cfg]` blocks. |
| `66292ff` | A | Doc accuracy: branch shape + JS test references. |

A second 6-commit pass — driven by a chromium-headless audit of
both `demo.parquet` and `cachecannon.parquet` against the
server-backed viewer — closes the four pre-merge blockers, the
sparse-metric binder-error class, and the `irate_lag(HUGEINT,
…)` issue, then ships the docs:

| Commit | What |
|---|---|
| `5f948f4` | `viewer/assets`: server-side adapter stubs (`registry()`, `setSelectedCgroups`, `getSelectedCgroups`) so the page doesn't TypeError on load (B1); skips `injectLabel` on SQL bodies; substitutes `__SELECTED_CGROUPS__` before `executeQuery`; switches section title + sidebar to `(N)` total / `(…)` while loading instead of flickering `(0) → (N)`. |
| `7f8da05` | `viewer/routes`: translates DuckDB `No matching columns` / `not found in FROM clause` binder errors into an empty Prometheus matrix, restoring the legacy `Tsdb`+PromQL "unknown metric → empty series" behaviour on parquets that lack a particular metric (carve-out #5). |
| `e5b66b7` | `dashboard`: switches `hist_percentile_series_combined` to emit `h2_combine_lol([*COLUMNS(...)])` (the shared cross-backend macro from upstream `metriken-query-sql/src/shared_macros.sql`), and casts `SUM(UBIGINT)` → `UBIGINT` at the four `irate_lag(…)` call sites in `sql.rs` + `cpu.rs` so DuckDB's HUGEINT promotion doesn't break the UDF (carve-out #6). |
| `3543351` | `viewer-sql`: retires the wasm-only `MACRO h2_combine(lol)` now that the shared `h2_combine_lol` exists; renames the parity test from `h2_combine_sums_elementwise_widest_wins` to the new name. |
| `0b4ecc0` | tests: `all_nan_series_collapses_to_empty_matrix` (prom-matrix) and `kpi_sql_none_emits_promql_query_only` (dashboard) pin the carve-outs the prior pass left implicit. |
| `c852cb7` | docs: this REVIEWING.md refresh + new `TOMORROW.md` capturing the deferred work (service-extension KPI transcription with cachecannon as a worked example, multi-rezolus aggregation, MCP migration). |

## Architecture (post-migration)

```
src/viewer/
  state.rs            AppState
                        + sql_backend: Arc<DuckDbBackend>   (one per process)
                        + captures: Arc<CaptureRegistry>
                        + upload_mutex: Mutex<()>          (serializes uploads)
                        + new(tsdb, templates)             (live mode)
                        + new_empty(templates)             (upload-only mode)
                        + new_sql(capture, backend, templates) (file mode)
                        + baseline_tsdb() -> Option<Arc<RwLock<Tsdb>>>  (live, cfg-gated)
                        + with_baseline_data(|&dyn DashboardData| ...)  (backend-agnostic)

  sql_capture.rs      SqlCapture { parquet_path, catalog, kind_by_metric,
                                   interval_seconds, time_range,
                                   source, version, filename }
                      open(path, &backend) -> SqlCapture
                      impl DashboardData for SqlCapture

  capture_registry.rs CaptureRegistry {
                        baseline: RwLock<Option<CaptureSlot>>,
                        experiment: RwLock<Option<CaptureSlot>>,
                      }
                      CaptureSlot.backend: CaptureBackend  (plain enum, no inner lock)
                      enum CaptureBackend { Sql(Arc<RwLock<SqlCapture>>),
                                            #[cfg(live-mode)] Live(Arc<RwLock<Tsdb>>) }
                      new(Option<CaptureBackend>)         (unified factory)
                      replace_baseline_with_sql(capture)  (upload / re-upload)
                      attach_experiment_sql(capture, ...)
                      get_sql(id) / get(id) (Tsdb, cfg-gated)

  routes.rs           /api/v1/query{,_range} -> state.sql_backend.run_sql(sql, path)
                                              -> prom_matrix::arrow_to_prom_matrix(...)
                      /api/v1/metadata reads from whichever backend the slot holds
                      /api/v1/reset, /api/v1/connect gated to live-mode feature

  actions.rs          ingest_baseline_from_path: SqlCapture::open + swap
                      attach_experiment: SqlCapture-backed
                      connect_agent, ingest_loop, reset_tsdb: gated live-mode

crates/prom-matrix/   arrow_to_prom_matrix(&[RecordBatch]) -> String   (native)
                      js_arrow_to_prom_matrix(&JsValue) -> Result<String, JsValue>  (wasm)
                      EMPTY_PROM_MATRIX const
                      pub(crate) emit_prom_matrix_json   (shared envelope, no drift)

crates/dashboard/
  data.rs             DashboardData trait (unchanged)
                      impl DashboardData for Tsdb gated #[cfg(feature = "live-mode")]
  service_extension.rs Kpi.sql: Option<String>  (NEW)
  dashboard/service.rs uses plot_promql_with_sql{,_full} when kpi.sql is Some
```

`metriken_query_sql::DuckDbBackend` (from
`/work/metriken/metriken-query-sql/`) provides `Sync + Send` per-source
connection-pool semantics; `Arc<DuckDbBackend>` is cloned into every
handler. First request for a given parquet pays pool cold-start;
subsequent requests hit the warm pool.

## Tests

| Layer | Where | Status |
|---|---|---|
| Rezolus binary | `cargo test --bin rezolus` | **160 / 160 pass** (was 158; +2 from the review pass — live→SQL swap regression test and concurrent-upload consistency test) |
| Dashboard inline | `cargo test -p dashboard` (lib) | **35 / 35 pass** (was 34; +1 from this pass — pins the `Kpi.sql=None` → plot has `promql_query` only contract used by the SQL frontend's "not-yet-migrated" placeholder branch) |
| Dashboard SQL snapshots | `crates/dashboard/tests/sql_snapshots.rs` | **18 / 18 pass** (snapshot for `hist_percentile_series_combined` updated to pin the new `h2_combine_lol([*COLUMNS(...)])` emission) |
| viewer-sql macro parity | `crates/viewer-sql/tests/macros.rs` | **17 / 17 pass** (the previously-named `h2_combine_sums_elementwise_widest_wins` test was renamed to `h2_combine_lol_sums_elementwise_widest_wins` and updated to call the shared macro under its new name) |
| prom-matrix projection | `crates/prom-matrix/tests/projection.rs` | **9 / 9 pass** (was 8; +1 from this pass — pins the "all-NaN series collapses to empty matrix" case alongside the existing mixed-NaN/Inf row-drop test) |
| metriken-query-sql pool invalidate | upstream `metriken-query-sql/tests/pool_invalidate.rs` | **4 / 4 pass** (was 3; +1 concurrent-stress test pins the `Arc<ConnState>` keep-alive contract when invalidate races with in-flight queries) |
| Frontend JS | `node --test tests/*.mjs` | **76 / 82 pass** (6 failures — see below) |
| End-to-end smoke | `tests/viewer_smoke.sh` | **Requires `jq`** (not in dev image); verified manually with `curl` |

**JS failures (6):**

- `tests/wasm_viewer_histogram_kpis.test.mjs` (×1) — references
  `site/viewer/pkg/wasm_viewer.js`, the retired in-process WASM
  PromQL engine (`crates/viewer/` deleted in `ad1ad9e`). Test file
  should be removed in a follow-up cleanup.
- `tests/compare_node_filter.test.mjs` (×5, all 5 tests in the file) —
  exercises `buildEffectiveQuery`'s PromQL-side node-label and
  instance-label injection. The 5 failing names are
  *"node label is injected on the cross-capture (experiment) path
  for non-service routes"*,
  *"node label is injected on the baseline (non-cross-capture) path too"*,
  *"node label is NOT injected for /service/\* routes..."*,
  *"instance label is injected on baseline path but skipped on cross-capture path"*,
  *"with no selected node, queries pass through unchanged on either path"*.
  After `BACKEND='sql'` (`f9b392b`) the function short-circuits to the
  SQL branch and never reaches the injection path; the tests assert
  PromQL-shape rewriting that's now dead code on the server-backed
  viewer until the SQL side gains a node-filter equivalent.

## Known regressions / deferred work

An end-to-end audit (per-plot SQL execution on `demo.parquet`
and `cachecannon.parquet`) surfaced four pre-merge blockers and
several smaller follow-ups, all **fixed** on this branch by a
follow-up implementation pass. **End-state: 254 / 254 plots
bind on both parquets across all 12 built-in dashboard pages.**
Many plots return empty matrices on small parquets that don't
carry every metric (e.g. demo has no GPU), but no plot
binder-errors — which is what the legacy `Tsdb`+PromQL path
delivered. The only remaining limitation is multi-rezolus
aggregation (≥2 rezolus sources in one parquet), which was not
present in the migration's original scope.

**B1. ✅ Fixed — `ViewerApi.registry is not a function`.**
The server-side adapter at `src/viewer/assets/lib/viewer_api.js`
was missing the `registry()` method that the source-picker code
in `src/viewer/assets/lib/app.js:433,461,471` (added in commit
`b099b97` "Stage 2c: source picker UI") calls. Without it the
JS threw TypeError on first load and mithril halted before any
chart rendered. Fix: a `registry()` stub returning `null`; the
three call sites all already null-coalesce via `?.has?.(...)`
patterns. Source-picker UI is a no-op on the server backend
until full multi-source support lands — which is the deferred
B2 work below.

**B2. ✅ Fixed — multi-source parquets now bind the standard
sections.** `metriken-query-sql/src/views.rs::render_src_sql`
now detects `<prefix>::` columns and, when present, picks the
rezolus-tagged columns (Arrow field metadata `source=rezolus`)
and projects them under canonical names that match dashboard
regexes. A new `canonical_alias` helper mirrors the wasm
viewer's `canonicalAlias` (`duckdb-registry.js:124`): it strips
the `<prefix>::` prefix, passes already-canonical column names
through, and rebuilds numeric-encoded columns (e.g.
`rezolus-client::10x0` with metadata `metric=cpu_cycles, id=0`)
into `cpu_cycles/0`. Value labels sort with non-numeric first
and numeric IDs last to match the named-column convention.
Empirics with the full post-fix stack (B2 + carve-outs #5/#6):
**254 of 254 plots bind on both demo and cachecannon** across
all 12 built-in pages. Plots whose target metric isn't in the
parquet return an empty Prometheus matrix instead of binder-
erroring (carve-out #5), so the result mirrors the legacy
`Tsdb`+PromQL "unknown metric → empty series" UX. **Multi-
rezolus** captures (≥2 sources tagged `rezolus`) are not yet
aggregated server-side — those would need the `COALESCE + sum`
/ `h2_combine` projection shape that wasm's
`_src_rezolus_combined` builds. Single-rezolus + arbitrary
application-source captures (the cachecannon shape) work
end-to-end.

**B3. ✅ Fixed — `/cgroups` section now binds on the server
backend.** `_cgroup_index` is now created server-side by
`metriken-query-sql/src/views.rs::create_cgroup_index`, called
from `build_slot_connection` (`backend.rs:339`) immediately
after `_src`. The shape (`metric`, `column_name`, `name`, `id`,
`labels MAP(VARCHAR, VARCHAR)`) mirrors the wasm viewer's
`buildCgroupIndex` so the dashboard's JOIN syntax binds
identically on both. On multi-source parquets `column_name`
goes through `canonical_alias` so the JOIN keys actually match
`_src` columns (which are also canonical-aliased). The render
SQL is precomputed at pool cold-start (one `read_introspection`)
and re-used on lazy slot rebuilds. Three unit tests pin the
contract: empty parquet, synthetic cgroup column with split
name/id/labels, and apostrophe-bearing cgroup names. After
selecting a cgroup on cachecannon, **27 of 48 plots populate**
(Total/User/System CPU Cores, Migrations, IPC, TLB Flushes, all
16 Syscall variants aggregate-side, plus a few individual-side
where the `/` cgroup carries per-state breakdown); the
remaining 21 are sparse-metric (no `cgroup_cpu_throttled*`
recorded in cachecannon, NULL-name rows lack per-`op` labels)
and render as empty rather than as errors.

**B4. ✅ Fixed — `h2_combine` cross-backend signature
mismatch.** Resolved by introducing
`h2_combine_lol(lol)` as a shared pure-SQL macro in
`metriken-query-sql/src/shared_macros.sql`. The dashboard's
`hist_percentile_series_combined` emits the new name, the WASM
viewer's single-arg `MACRO h2_combine(lol)` is retired, and
the native variadic UDF stays as-is for fast direct-column
callers. The duckdb-rs panic hazard at `udf.rs:515-525` (the
reason the variadic UDF exists) is sidestepped entirely
because the new shape is pure SQL. Verified on demo: `/syscall`
"Overall Latency" returns 5 quantile series × 301 samples,
parity with the WASM viewer.

---

### 5. ✅ Fixed — sparse-metric binder errors

DuckDB's `COLUMNS('regex')` is a **binder-time** error when no
column matches. On parquets lacking a metric (demo has no GPU,
no `cpu_tsc`/`aperf`/`mperf`, incomplete softirq subtypes,
etc.), the dashboard emits the plot's SQL anyway. To restore
the legacy `Tsdb`+PromQL behaviour (unknown metric → empty
series), the server's `run_sql` handler at
`src/viewer/routes.rs::run_sql` now translates two binder-error
classes — `No matching columns` and `not found in FROM clause`
— into an `EMPTY_PROM_MATRIX` response. The frontend sees
"no data" and renders an empty chart, which matches what
PromQL would have produced. All other SQL errors continue to
surface as `sql_error` so real bugs aren't swallowed.

### 6. ✅ Fixed — `irate_lag(HUGEINT, ...)` binder errors

DuckDB promotes `SUM(UBIGINT)` to `HUGEINT` to avoid u64
overflow, but the native `irate_lag` UDF
(`metriken-query-sql/src/udf.rs`) is typed `(UBIGINT,
UBIGINT, BIGINT) -> DOUBLE`. The dashboard's cgroup_irate_by_name
and cgroup_ratio_by_name emitters in
`crates/dashboard/src/sql.rs`, plus the inline
`Misses (Per-CPU)` and `MPKI (Per-CPU)` queries in
`crates/dashboard/src/dashboard/cpu.rs`, all had
`SUM(v) AS s` immediately followed by `irate_lag(s, ...)`.
Fix: `CAST(SUM(v) AS UBIGINT)` at each of the four call
sites. Per-CPU and per-cgroup counter sums fit u64 with
extreme headroom; the cast is well-bounded and the comment
at each site documents the overflow rationale.

---

These are deliberate carve-outs, called out so reviewers don't
go hunting:

1. **Service-extension KPIs render as "temporarily unavailable"
   placeholders** on the migrated server viewer (and the WASM
   viewer when loading multi-source captures). Cause: `BACKEND='sql'`
   asks the frontend to send `plot.sql_query`, but the 10 service
   templates in `config/templates/*.json` still ship `query` (PromQL)
   only — `Kpi.sql` is `None` for every template. The frontend
   reads null and renders a "query not yet available" stub.
   Fix path: add a server-side per-source view layer (mirror
   `site/viewer-sql/lib/duckdb-registry.js::buildSourceViews` in
   `SqlCapture::open`), then transcribe each template's KPIs to
   SQL against the aliased view. Templates ship in
   `config/templates/{cachecannon,vllm,vllm-decode,vllm-prefill,sglang,sglang-prefill,sglang-router,llm-perf,valkey,inference-library}.json`.

2. **Multi-node selection** (the top-nav node picker) doesn't filter
   server-side queries. Frontend `buildEffectiveQuery` only injects
   `node="..."` on PromQL plots; the SQL backend has no equivalent.
   On multi-node parquets the server returns aggregated data
   regardless of selection. WASM viewer same. Future work.

3. **MCP migration deferred** entirely. `src/mcp/` continues using
   `Tsdb::load` + `QueryEngine`; the `live-mode` feature gates the
   whole module out of the SQL-only build (`mod mcp` is gated in
   `src/main.rs`). `mcp query` accepts PromQL strings, not SQL.
   Follow-up branch should migrate the 5+ `Tsdb::load` call sites
   and the anomaly-detection / correlation algorithms.

4. **`Save as Report` column trim** disabled when the baseline is
   SQL-backed (`src/viewer/actions.rs::save_with_selection` forces
   `trim_columns = false`). Original PromQL-driven column resolution
   in `report-save::resolve_kept_columns` requires a Tsdb. The
   embed-only path still runs and saves the parquet with full columns
   + the selection JSON in the footer. Fix path: walk
   `SqlCapture::catalog().series_by_metric` instead of running
   PromQL.

5. **Stage 9 worktree** at `/work/rezolus/.claude/worktrees/agent-aff3f0c14218e1cff`
   is locked by the Claude session that produced commit `2516840`
   (cherry-picked as `7124c4a`). Remove manually with
   `git worktree remove --force <path>` once unlocked.

## Decisions worth flagging at review

- **Upload-only init has no placeholder capture.** The registry
  models "no baseline yet" with `baseline: RwLock<Option<CaptureSlot>>`
  rather than a sentinel `SqlCapture::empty()`. The earlier placeholder
  + its `DashboardData` impl were removed by `fe6bdc7` (B4) once the
  registry's `Option` made them redundant. Pre-upload, `/api/v1/mode`
  returns `loaded:false` and section handlers short-circuit on `None`.

- **`CaptureSlot.backend` is a plain enum**, not `RwLock<CaptureBackend>`.
  Swaps go through the outer `RwLock<Option<CaptureSlot>>` — the whole
  slot is replaced atomically. The inner lock removed by `12f555a` (B2)
  was redundant: any code path that needed to mutate the backend was
  already holding the outer write lock through `replace_baseline_with_sql`
  / `attach_experiment_sql`.

- **`parquet_tools/mod.rs`** was edited by the live-mode-gating commit
  even though the plan said to leave `parquet_tools/` alone. Reason:
  `combine` is consumed unconditionally by recorder and A-B
  extraction in the viewer, so we can't gate the entire module —
  only `mod annotate` / `mod filter` are conditional. Their submodule
  *contents* are untouched.

- **`bytes = "1"`** added as a direct dep in `crates/report-save/`
  and the rezolus binary because `metriken-query`'s
  `pub use bytes::Bytes` re-export becomes unavailable in
  sql-only. The `bytes` crate is small and was already a transitive
  dep.

- **`metriken-exposition` stays unconditional** (parquet writing for
  recorder / hindsight / exporter / agent / save-as-report
  embedding). Only `metriken-query` is feature-gated.

## Verification recipe

```bash
# Default build + file mode
cargo build --bin rezolus
./target/debug/rezolus view site/viewer/data/demo.parquet --listen 127.0.0.1:9091 &
sleep 2

curl -s http://127.0.0.1:9091/api/v1/mode
# {"category":null,"combined_ab":false,"compare_mode":false,"live":false,"loaded":true,"report":false,"url_loading":"disabled"}

curl -s http://127.0.0.1:9091/api/v1/metadata
# {"status":"success","data":{"fileChecksum":"7909b4b1...","filename":"demo.parquet","maxTime":1768956939000,"minTime":1768956638000}}

curl -s "http://127.0.0.1:9091/api/v1/query_range" \
  --data-urlencode 'query=SELECT timestamp/1e9 AS t, "cpu_usage/user/0"::DOUBLE AS v FROM _src ORDER BY t LIMIT 3' \
  --data-urlencode 'start=0' --data-urlencode 'end=99999999999' --data-urlencode 'step=1' -G
# {"status":"success","data":{"resultType":"matrix","result":[{"metric":{},"values":[[1768956638,"35504000000"],...]}]}}
pkill rezolus

# SQL-only build (live mode + MCP excluded)
cargo build --bin rezolus --no-default-features --features sql-only
cargo tree -p rezolus --no-default-features --features sql-only | grep metriken-query
# (empty — metriken-query gone)
./target/debug/rezolus view site/viewer/data/demo.parquet --listen 127.0.0.1:9091 &
# same `mode` / `metadata` / `query_range` responses as default build
pkill rezolus
```

Open the browser at <http://127.0.0.1:9091> for the full dashboard
rendered via SQL queries against DuckDB.
