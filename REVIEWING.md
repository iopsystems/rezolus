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

### 2. Service-extension KPI templates: gauges/counters on SQL, histograms still PromQL

Per-source views (`_src_<source>`) now exist on the engine side and
128 of 218 template KPIs ship a `sql` field alongside their PromQL
`query`. Plot bodies that need `plot.sql_query` no longer see `null`
for gauges and counters; the SQL-only frontend renders them through
the SQL pipeline. The remaining 100 KPIs split into histogram
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

The default build still pulls in `metriken-query` because **three**
call-site classes still need it: the live-agent ingest path
(item D below), `validate_service_extensions` on live mode
(item B), and the dashboard crate's cfg-gated `Tsdb`
re-export/`DashboardData` impl (item C). The plan defers all three
behind a live-ingest storage decision; MCP, report-save column trim,
the MCP audit-and-delete, and `parquet annotate` have all landed.

### A. report-save live-mode column trim (deferred — keeps Tsdb in
live mode)

The HTML report renderer's live-mode `save_single_parquet` /
`save_combined_ab_tarball` use a `Tsdb` only to drive the PromQL
column-trim path (`resolve_kept_columns`). report-save **does not run
queries** — it only embeds selection JSON into parquet footer
metadata via `embed_selection_in_parquet`; query execution happens
client-side at view time. Deleting the live-mode PromQL trim would
make live-mode captures fall through to `embed_only` (full parquet
bytes, no trim) — acceptable but a UX regression. Tied to the
live-ingest decision (item D); once that lands and live captures are
SQL-backed, the live-mode branch is dead and the trim path drops out
naturally.

### B. `validate_service_extensions` (`src/viewer/metadata.rs`)

Small. Runs each KPI's PromQL against the live-agent `Tsdb` to
mark KPIs `available=false` when their data is empty. Two
dependencies:

- Carve-out 2 (service-extension SQL templates) must land first
  so KPIs carry SQL strings.
- A SQL backend for the live-agent capture must exist (see D).

After both, this function is a half-page rewrite: run each
`kpi.sql` through `DuckDbBackend`, check non-empty.

### C. Dashboard crate's `Tsdb` re-export + `DashboardData` impl

`crates/dashboard/src/lib.rs:11` re-exports `Tsdb`, and
`data.rs:55` `impl DashboardData for Tsdb` (cfg-gated to
`live-mode`). The dump binary at `crates/dashboard/src/main.rs`
uses `Tsdb::default()` to drive the dashboard generators for
schema dumps.

These exist solely because `Tsdb` is still a live `DashboardData`.
Once items A and B above land and the live-agent path migrates
(see D), the cfg-gated `impl` and the re-export delete with no
remaining consumer. The dump binary needs a synthetic
`DashboardData` (an empty `SqlCapture`-shaped placeholder is the
obvious shape) so the static schema dump survives.

The same applies to the scatter of `Tsdb::default()` in test
fixtures across `dashboard/src/dashboard/{mod,category,service}.rs`
and `viewer/{state,metadata,actions}.rs`. All replace with a
synthetic `DashboardData` once the trait is the only contract.
(`report-save/src/lib.rs` uses `Tsdb::load_from_bytes` rather than
`Tsdb::default()`; same shape, same disposition.)

### D. The live-agent ingest path (storage choice)

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

### What deletes when A-D all land

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

The harness's land-or-delete (metriken-side) collapses to **delete**
once items A-D land: every previous candidate consumer (MCP,
report-save column trim, `validate_service_extensions`, report-save
live-mode trim) is going the SQL-rewrite route, not the harness route.
The build matrix simplifies to a single configuration.
