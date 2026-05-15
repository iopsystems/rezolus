# Reviewing `yv/sql-testing` — rezolus side

Companion: `/work/metriken/REVIEWING.md` (engine side).

This branch finishes the migration of **`rezolus view <parquet>`**,
**`rezolus mcp`**, and **Save-as-Report column trim** off the
in-memory `metriken_query::Tsdb` + PromQL pipeline and onto
DuckDB-driven SQL through `metriken_query_sql::DuckDbBackend`. The
WASM static viewer was already on duckdb-wasm; this branch brings
the server-backed viewer and MCP to parity. The live-agent ingest
path and a few PromQL-only odds and ends remain as deliberate
carve-outs; the _Removing Tsdb entirely_ section at the end is the
roadmap for those.

| Path                                                        | Engine                           | Status                                                                |
| ----------------------------------------------------------- | -------------------------------- | --------------------------------------------------------------------- |
| `rezolus view <parquet>` — file / upload / A-B / experiment | `DuckDbBackend` via `SqlCapture` | **migrated**                                                          |
| `rezolus view http://agent:4241` — live agent               | `Tsdb` (ingest only)             | **carve-out**; query handlers return `capture_not_found` in live mode |
| WASM static viewer (`site/viewer/`)                         | duckdb-wasm                      | unchanged (already DuckDB)                                            |
| MCP (`src/mcp/`)                                            | `DuckDbBackend` via `SqlCapture` | **migrated** — `src/mcp/backend.rs` is the shared loader/projector    |
| `rezolus parquet annotate`                                  | `DuckDbBackend`                  | **migrated** — validates KPIs via SQL                                 |
| Save-as-Report column trim                                  | `MetricCatalog` (via `SqlCapture`) | **migrated** for SQL-backed captures; live-mode keeps the PromQL trim path |
| Save-as-Report query embed                                  | `Tsdb` + PromQL `QueryEngine`    | **carve-out** for live-mode embed-only; SQL-backed captures use catalog only |

## Build matrix

- `cargo build --bin rezolus` (default) — full functionality.
  Retains `metriken-query` for the live-agent path,
  `validate_service_extensions`, the dashboard crate's `Tsdb`
  re-export, and Save-as-Report's live-mode PromQL query embed.
- `cargo build --bin rezolus --no-default-features --features sql-only`
  — drops `metriken-query` entirely (`cargo tree -p rezolus --no-default-features --features sql-only | grep 'metriken-query v'`
  is empty; only `metriken-query-sql` appears). Drops only the
  live-agent ingest path. MCP, the file / upload / A-B viewer,
  `parquet annotate`, and Save-as-Report's column-trim path all
  remain.

Both build clean.

---

## Architecture (post-migration)

```
src/viewer/
  state.rs            AppState
                        sql_backend: Arc<DuckDbBackend>       (one per process)
                        captures:    Arc<CaptureRegistry>
                        upload_mutex: Mutex<()>               (serializes uploads)
                        new_sql(capture, backend, templates)  (file mode)
                        new(tsdb, templates)                  (live mode, cfg-gated)
                        new_empty(templates)                  (upload-only)
                        with_baseline_data(|&dyn DashboardData| ...)

  sql_capture.rs      SqlCapture { parquet_path, catalog,
                                   kind_by_metric, interval_seconds,
                                   time_range, source, version, filename }
                      impl DashboardData for SqlCapture

  capture_registry.rs CaptureRegistry { baseline, experiment: RwLock<Option<CaptureSlot>> }
                      enum CaptureBackend { Sql(Arc<RwLock<SqlCapture>>),
                                            #[cfg(live-mode)] Live(Arc<RwLock<Tsdb>>) }

  routes.rs           /api/v1/query{,_range} → state.sql_backend.run_sql + arrow_to_prom_matrix
                      Binder errors ("No matching columns" / "not found in FROM clause")
                      → EMPTY_PROM_MATRIX, restoring legacy "unknown metric → empty series" UX.

  actions.rs          ingest_baseline_from_path: SqlCapture::open + atomic swap
                      attach_experiment / detach_experiment: SqlCapture-backed
                      connect_agent / ingest_loop / reset_tsdb: cfg-gated live-mode

crates/prom-matrix/   arrow_to_prom_matrix(&[RecordBatch]) -> String       (native)
                      js_arrow_to_prom_matrix(&JsValue)  -> JsValue        (wasm)
                      Shared pub(crate) emit_prom_matrix_json envelope:
                      the JSON shape can't drift between server and browser.

crates/dashboard/
  data.rs             DashboardData trait — implemented by SqlCapture AND
                      (cfg(live-mode)) Tsdb. Generators read schema through
                      this; query execution is elsewhere.
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

## Carve-outs (still PromQL/Tsdb today)

The remaining carve-outs sit behind the `live-mode` feature
(default-on). `--no-default-features --features sql-only` drops
them. MCP and Save-as-Report column-trim used to be here too and
have since moved off Tsdb — they're called out in the _Recently
landed_ section below and are unconditional in both build
configurations.

### 1. Live-agent query path

`rezolus view http://agent:4241` ingests snapshots into a `Tsdb`
(`actions.rs::ingest_loop`), but `/api/v1/query{,_range}` return
`capture_not_found` for live mode by design — the SQL handlers
need a parquet on disk and there isn't one. The only PromQL
execution that still happens in live mode is
`validate_service_extensions` (KPI availability check on load).
Storage choice for live ingest is the architectural question;
sketched in _Removing Tsdb entirely_.

### 2. Service-extension KPI templates still PromQL-only

The 11 templates in `config/templates/*.json` carry a PromQL
`query` field but no `sql` field. With the frontend on
`BACKEND='sql'`, plot bodies that need `plot.sql_query` see
`null` and render "query not yet available" stubs. The dashboard's
`service.rs::generate` already emits the SQL-aware path when
`Kpi.sql` is `Some` — but populating `sql` is **not** purely a
template-content task. There is a real architectural problem
underneath, surfaced during the May 2026 migration attempt.

**The architectural problem.** Service-extension parquets are
multi-source (e.g., `source: ["cachecannon", "rezolus"]`), and
each column is prefixed in the parquet schema:
`0::bytes_rx` (cachecannon instance 0), `rezolus-client::cpu_usage`
(rezolus node). Column metadata carries the `source` label
(`source=cachecannon`, `source=rezolus`) plus an instance/node
identifier.

The current `_src` view in
`metriken-query-sql/src/views.rs::render_src_sql` projects only
`source=rezolus` columns under canonical names. Service columns
(`source=cachecannon`, `source=vllm`, …) are **unreachable** from
`_src`. A template KPI like
`SELECT sum(irate_1s(bytes_rx, timestamp)) AS v FROM _src` would
bind-error on a real cachecannon parquet, because `bytes_rx` is
not a `_src` column.

The WASM viewer at `site/viewer-sql/lib/duckdb-registry.js` has
already solved this — `buildSourceViews` creates per-source views
**keyed by physical column prefix**: `_src_0` (cachecannon instance
0), `_src_rezolus_client` (rezolus node). But the prefix is an
instance/node identifier, not a source name. The service-extension
templates use `{source="cachecannon"}` PromQL filters — i.e., they
think in source NAMES, not in instance prefixes. There is no clean
1:1 mapping when a parquet has multiple instances of the same
source.

**Three design options** (none of which is "just transcribe SQL"):

1. **Mirror the WASM convention to the server.** Add `buildSourceViews`
   to `metriken-query-sql/src/views.rs` so `_src_<prefix>` views exist
   on the native side too. Templates pick a view via dashboard-emitter
   substitution (new mechanism in `crates/dashboard/src/dashboard/service.rs`
   — currently `kpi.sql` is embedded literally; would need a
   `{{view}}` token + per-render substitution). Cross-crate change in
   metriken-query-sql + dashboard. Parity-wins by matching WASM exactly.

2. **Add source-name-keyed views.** Create `_src_cachecannon`,
   `_src_vllm`, etc., aggregating multiple instances at each
   timestamp (`COALESCE + sum` for scalars, `h2_combine_lol` for
   histograms — the same pattern WASM uses for `_src_rezolus_combined`).
   Templates reference `_src_<source>` directly — no substitution
   layer. Diverges from the WASM convention; means both backends
   carry slightly different view sets. Closer to the PromQL mental
   model the templates were authored in.

3. **Materialise per-source views server-side, scoped per-capture.**
   The view-set lives on the `SqlCapture` (one per loaded parquet)
   rather than in `metriken-query-sql`. Lets the choice between (1)
   and (2) be made per-template later. Adds plumbing through
   `SqlCapture::open` and the `DuckDbBackend` cold-start path.

Whatever route is chosen, the work breaks into pieces:

- **Engine-side:** per-source view materialisation in
  `metriken-query-sql/src/views.rs` (~150-250 LOC + tests),
  parallel to `render_src_sql`. Must agree with the WASM viewer's
  view-naming so dashboard SQL doesn't fork by backend.
- **Dashboard-side:** if option 1, add a substitution mechanism in
  `crates/dashboard/src/dashboard/service.rs:84-97` so the template's
  `kpi.sql` is rewritten at emit time to reference the right view.
- **Templates:** transcribe ~218 KPIs across 11 files (vllm 34,
  vllm-prefill 34, vllm-decode 34, sglang 28, sglang-decode 19,
  sglang-prefill 19, cachecannon 14, llm-perf 13, sglang-router 9,
  inference-library 9, valkey 5; counts are
  `len(json.load(...)["kpis"])` per file). Keep the existing
  `query: PromQL` field so live-mode still renders KPIs.
- **Parity tests:** for each KPI, run the PromQL through the legacy
  `metriken-query::QueryEngine` and the new SQL through `DuckDbBackend`
  against the same fixture, compare numerically (within tolerance).
  Pattern: adapt
  `metriken-query/examples/sql_vs_promql.rs` to walk template KPIs
  rather than catalogue entries. `parquet annotate`'s non-empty check
  alone is insufficient — value drift would be silent.
- **Cross-backend check:** the WASM viewer must render the same
  KPIs correctly through the same SQL bodies. Don't ship a server-side
  view convention WASM can't honour.

**Status (May 2026):** deferred. Adding per-source views is a multi-PR
effort and the view-naming design choice (options 1-3 above) is the
gating decision. The MCP and report-save migrations (now landed —
see _Recently landed_) don't depend on this work — they consume
`_src` directly for rezolus metrics. Templates remain PromQL-only
until the architectural choice is made and the engine piece lands.

### 3. Multi-node selection doesn't filter server-side

The top-nav node picker injects `node="..."` only on the PromQL
side; the SQL backend has no equivalent. WASM viewer has the
same gap. On multi-node parquets the server returns aggregated
data regardless of selection. Future work; not unique to this
branch.

### 4. Multi-rezolus aggregation

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

New `src/mcp/backend.rs` (468 LOC) is the shared helper layer:

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

Legacy `format_recording_info`, `format_metrics_description`,
`calculate_correlation`, `detect_anomalies`,
`extract_matrix_samples`, `extract_time_series`,
`auto_construct_query`, etc. are now `#[cfg(feature = "live-mode")]`
holdouts pending an audit delete.

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

---

## Tests

| Command                            | Covers                                                             |
| ---------------------------------- | ------------------------------------------------------------------ |
| `cargo test --bin rezolus`         | Binary including viewer, actions, MCP backend + subcommands.       |
| `cargo test -p dashboard`          | DashboardData impls, plot emitters, sql_snapshots.                 |
| `cargo test -p prom-matrix`        | Arrow → Prometheus matrix projection (incl. NaN/Inf row-dropping). |
| `cargo test -p viewer-sql`         | WASM crate's SHARED_MACROS parity against the native engine.       |
| `cargo test -p metriken-query-sql` | UDFs, backend pool, concurrent invalidate stress.                  |
| `cargo test -p report-save`        | Column-trim resolvers (SQL via `MetricCatalog`; live-mode via `Tsdb`). |
| `cargo test --test mcp_cli`        | End-to-end MCP CLI smoke against `target/debug/rezolus` + `demo.parquet` (auto-skips when fixtures or binary are missing). |
| `node --test tests/*.mjs`          | Frontend pure-JS tests.                                            |
| `bash tests/viewer_smoke.sh`       | End-to-end (upload / file / A-B / proxy). Requires `jq`.           |

Frontend JS has 6 pre-existing failures referencing the retired
in-process WASM PromQL viewer (`crates/viewer/` deleted) and the
PromQL-side `buildEffectiveQuery` injection path that goes
unreached on `BACKEND='sql'`. Both are dead code on the
server-backed viewer; the tests should be retired in a follow-up
cleanup, not fixed.

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

# SQL-only build — only the live-agent ingest path is excluded.
# MCP, file/upload/A-B viewer, parquet annotate, and report-save
# column-trim all build and run in this configuration.
cargo build --bin rezolus --no-default-features --features sql-only
cargo tree -p rezolus --no-default-features --features sql-only | grep 'metriken-query v'   # (empty; only metriken-query-sql appears)
./target/debug/rezolus view site/viewer/data/demo.parquet --listen 127.0.0.1:9091 &
# same /mode /metadata /query_range responses as default build
pkill rezolus

# MCP smoke under sql-only
./target/debug/rezolus mcp query site/viewer/data/demo.parquet 'SELECT count(*) AS n FROM _src'
```

Open <http://127.0.0.1:9091> for the full dashboard rendered via
SQL against DuckDB.

---

## Removing Tsdb entirely

Goal: drop `metriken-query` from the default build's dep tree, and
delete `metriken-query::{promql, tsdb}` (~6,580 LOC) from the
engine side. After this, `metriken-query` either disappears or
becomes a thin re-export shell over `metriken-query-sql`; the
harness's land-or-delete decision (see the metriken doc) is part
of how this lands.

The default build still pulls in `metriken-query` because **four**
call-site classes still need it. Each is independent and can land
in its own PR. (MCP and report-save column-trim were items #1 and
#4 in the previous version of this section; both landed and are
covered in _Recently landed_ above. They are listed here only as
"audit-and-delete" residuals.)

### A. MCP legacy helpers (audit-and-delete)

The MCP migration left behind a handful of PromQL/Tsdb helpers
gated `#[cfg(feature = "live-mode")]` — `format_recording_info`,
`format_metrics_description`, `calculate_correlation`,
`detect_anomalies`, `extract_matrix_samples`,
`extract_time_series`, `auto_construct_query`, plus their callers
under `mcp/anomaly_detection/` and `mcp/correlation.rs`. None are
reachable on the default-build production path; they exist as a
safety net while the SQL-backed equivalents bed in. An audit PR
deletes them and unhooks the `live-mode` cfg from `src/mcp/`.

### B. `crates/report-save/` — live-mode query embed

The HTML report renderer still loads a parquet into a `Tsdb` and
runs the dashboard's queries through `QueryEngine` for the actual
plot-body embed on live-mode captures. (The column-trim half landed
in `a761906` — see _Recently landed_ — so `save_single_parquet_sql`
and `save_combined_ab_tarball_sql` already handle the SQL-backed
caller path.) The remaining work is the query embed: route PromQL
strings through the harness, or rewrite to SQL.

The SQL-rewrite is simpler than MCP — `report-save` doesn't do
analytical computation, just runs each plot's query and lays out
the result. The dashboard crate's SQL-emitting path
(`plot_promql_with_sql`) gives `report-save` the SQL strings to
run; mostly the rewrite is at the trait/Tsdb-vs-SqlCapture
boundary.

### C. `validate_service_extensions` (`src/viewer/metadata.rs`)

Small. Runs each KPI's PromQL against the live-agent `Tsdb` to
mark KPIs `available=false` when their data is empty. Two
dependencies:

- Carve-out 2 (service-extension SQL templates) must land first
  so KPIs carry SQL strings.
- A SQL backend for the live-agent capture must exist (see E).

After both, this function is a half-page rewrite: run each
`kpi.sql` through `DuckDbBackend`, check non-empty.

### D. Dashboard crate's `Tsdb` re-export + `DashboardData` impl

`crates/dashboard/src/lib.rs:11` re-exports `Tsdb`, and
`data.rs:55` `impl DashboardData for Tsdb` (cfg-gated to
`live-mode`). The dump binary at `crates/dashboard/src/main.rs`
uses `Tsdb::default()` to drive the dashboard generators for
schema dumps.

These exist solely because `Tsdb` is still a live `DashboardData`.
Once carve-outs A-C above land and the live-agent path migrates
(see E), the cfg-gated `impl` and the re-export delete with no
remaining consumer. The dump binary needs a synthetic
`DashboardData` (an empty `SqlCapture`-shaped placeholder is the
obvious shape) so the static schema dump survives.

The same applies to the scatter of `Tsdb::default()` in test
fixtures across `dashboard/src/dashboard/{mod,category,service}.rs`
and `viewer/{state,metadata,actions}.rs`. All replace with a
synthetic `DashboardData` once the trait is the only contract.
(`report-save/src/lib.rs` uses `Tsdb::load_from_bytes` rather than
`Tsdb::default()`; same shape, same disposition.)

### E. The live-agent ingest path (storage choice)

This is the architectural question. Today
`src/viewer/actions.rs::ingest_loop` polls `/metrics/binary` from
the agent and calls `tsdb.ingest(snapshot)`. To remove `Tsdb`,
snapshots need to land in something DuckDB can query. Three
sketches:

- **Rolling on-disk parquet.** Hindsight already does this for
  post-incident snapshots. Live mode becomes "rezolus view
  <rolling-buffer-path>"; the viewer doesn't need to know it's
  live. Cheap, reuses hindsight's rotation logic, deduplicates a
  whole storage path. Query freshness = rotation period. Probably
  the right move long-term: one ingest, one storage, two
  consumers.
- **In-memory DuckDB table.** `ingest_loop` builds an Arrow
  `RecordBatch` from each snapshot (metriken-exposition's
  `MsgpackToParquet` already has the shape) and `INSERT`s into a
  named table. Lowest latency. Highest plumbing cost — the
  schema needs to grow incrementally as new metrics appear,
  which the current parquet flow handles implicitly.
- **In-memory parquet bytes.** Buffer recent snapshots as parquet
  bytes; query via `read_parquet('blob:...')`. Halfway between
  the other two; loses the on-disk durability of option 1 and
  the freshness of option 2.

If hindsight's rotation logic is reusable (it is), option 1 is the
shortest path. The follow-on simplification — collapsing live
mode and post-incident snapshot into one storage path with two
viewers — is the long-term architectural win.

### What deletes when A-E all land

- `promql-parser` dep goes.
- `metriken-query/src/promql/` (4,716 LOC) goes.
- `metriken-query/src/tsdb/` (1,863 LOC) goes.
- The `legacy` and `ingest` features go.
- `crates/dashboard/`: the `metriken-query` dep, the `Tsdb`
  re-export, and the cfg-gated `DashboardData` impl all go.
- The `#[cfg(feature = "live-mode")]` gates across `src/main.rs`,
  `src/viewer/{state,actions,metadata}.rs`,
  `src/viewer/capture_registry.rs::CaptureBackend::Live`, and
  `src/parquet_tools/mod.rs` go — the carve-out marker
  vanishes.

The harness's land-or-delete (metriken-side) is a sub-decision of
item B above (the report-save live-mode query embed): land it and
B becomes "wire the existing PromQL strings through
`harness::Engine`"; delete it and B becomes "rewrite to SQL". The
build matrix simplifies to a single configuration either way.
