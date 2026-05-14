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

65 commits ahead of `origin/main`, **+7,539 / −1,847 across 92
files** (`git diff --shortstat origin/main...HEAD`,
`git rev-list --count origin/main..HEAD`).

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

## Architecture (post-migration)

```
src/viewer/
  state.rs            AppState
                        + sql_backend: Arc<DuckDbBackend>   (one per process)
                        + captures: Arc<CaptureRegistry>
                        + new(tsdb, templates)              (live mode)
                        + new_sql(capture, backend, templates) (file mode)
                        + baseline_tsdb() -> Option<Arc<RwLock<Tsdb>>>     (live)
                        + baseline_sql()  -> Option<Arc<RwLock<SqlCapture>>> (file)
                        + with_baseline_data(|&dyn DashboardData| ...)

  sql_capture.rs      SqlCapture { parquet_path, catalog, kind_by_metric,
                                   interval_seconds, time_range,
                                   source, version, filename }
                      open(path, &backend) -> SqlCapture
                      empty()                         (upload-only placeholder)
                      impl DashboardData for SqlCapture

  capture_registry.rs CaptureSlot.backend: RwLock<CaptureBackend>
                      enum CaptureBackend { Sql(Arc<RwLock<SqlCapture>>),
                                            #[cfg(live-mode)] Live(Arc<RwLock<Tsdb>>) }
                      new_sql(capture, ...)          (file mode)
                      replace_baseline_with_sql(capture)  (upload swap-in)
                      attach_experiment_sql(capture, ...)
                      get_sql(id) / get(id) (Tsdb)

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
| Rezolus binary | `cargo test --bin rezolus` | **158 / 158 pass** |
| Dashboard inline | `cargo test -p dashboard` (lib) | **34 / 34 pass** |
| Dashboard SQL snapshots | `crates/dashboard/tests/sql_snapshots.rs` | **18 / 18 pass** |
| viewer-sql macro parity | `crates/viewer-sql/tests/macros.rs` | **17 / 17 pass** |
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

These are deliberate carve-outs, called out so reviewers don't go
hunting:

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

- **`SqlCapture::empty()` constructor** (`src/viewer/sql_capture.rs`)
  added for upload-only init. Slightly cleaner than introducing a
  third `CaptureBackend::Empty` variant. The empty capture's
  `DashboardData` impl returns sensible defaults (no time range, no
  metric names) so `/api/v1/sections` and `/api/v1/mode` behave
  correctly pre-upload.

- **`CaptureSlot.backend: RwLock<CaptureBackend>`** instead of plain
  `CaptureBackend`. The lock lets the upload / connect handlers swap
  the backend in place without rebuilding the registry. Readers
  clone the inner `Arc<RwLock<Tsdb>>` or `Arc<RwLock<SqlCapture>>`
  out and release the registry lock before any real work — keeps
  the slot-level lock fast.

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
