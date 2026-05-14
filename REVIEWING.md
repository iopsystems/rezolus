# Reviewing the `yv/sql-testing` branch (rezolus side)

Companion doc: `/work/metriken/REVIEWING.md` (engine side).

This branch migrates the **static (file-mode) browser viewer**
off the in-process Rust→WASM PromQL engine and onto
duckdb-wasm. Server-backed paths are unchanged.

| Path | Engine | Status |
|---|---|---|
| Static viewer (`site/viewer/`, file mode) | duckdb-wasm in JS | **migrated**, SQL-only |
| `rezolus view <parquet>` server-side `/api/v1/query_range` | `metriken_query::promql::QueryEngine` | unchanged |
| `rezolus view http://agent:4241` (live agent) | same | unchanged |
| MCP (`src/mcp/`) | same | unchanged |

The retired in-browser WASM PromQL engine (`crates/viewer/`) was
deleted in commit `ad1ad9e`. The transitional shadow-mode
dispatcher that ran both engines side-by-side was removed in
commit `519c24c`; that role is now played by
`/work/metriken/metriken-query/examples/sql_vs_promql.rs`, run
manually against fixture parquets.

`cargo check --bin rezolus`, `-p dashboard`, and `-p viewer-sql`
pass cleanly. `cargo test --workspace` is **171 passed, 0
failed, 11 ignored**.

Branch shape: **44** commits, **+7,350 / −2,266** across **82**
files (`git diff --shortstat main...yv/sql-testing`,
`git rev-list --count main..yv/sql-testing`).

---

## Architecture

### Static viewer — duckdb-wasm in the browser

```
site/viewer/                                Mithril UI (file-mode entry)
  └─ lib/viewer_api.js:6                    const BACKEND = 'sql'

imports:
  site/viewer-sql/pkg/wasm_viewer_sql.js    built from crates/viewer-sql/
  site/viewer-sql/lib/duckdb-registry.js    926 LOC: JS-side multi-worker
                                            duckdb-wasm pool, parquet
                                            attachment, per-source aliasing,
                                            cgroup index table, result cache
  duckdb-wasm 1.33.1-dev45.0                from jsdelivr (script.js:12)
```

`crates/viewer-sql/Cargo.toml` has **no `metriken-query` dep** —
the static viewer is fully decoupled from the legacy engine.

### Server-backed viewer + MCP — PromQL only, unchanged

`rezolus view <parquet | live-agent-url>` boots `AppState`
(`src/viewer/mod.rs:749`) over a `CaptureRegistry` of `Tsdb`s.
`/api/v1/query_range` (handler `:1661`, `QueryEngine::new`
`:1676`) and `/api/v1/query` (`:1651`) return Prometheus matrix
JSON. MCP (`src/mcp/mod.rs`) uses the same `QueryEngine` via the
re-export at `src/viewer/mod.rs:63`.

### Dashboard crate — two queries per plot

`crates/dashboard/Cargo.toml:13` enables `features = ["legacy"]`
on `metriken-query`; the only usage is `Tsdb`
(`crates/dashboard/src/{data.rs:12, lib.rs:8}`).

Each plot calls `plot_promql_with_sql(opts, promql, sql)`
(definition at `crates/dashboard/src/plot.rs:184`, 280). **130
call sites** across `crates/dashboard/src/dashboard/*.rs`. Both
strings end up in the dashboard JSON the frontend consumes; the
frontend picks one based on a compile-time `BACKEND` constant
per viewer copy (`site/viewer/lib/viewer_api.js:6` = `'sql'`;
`src/viewer/assets/lib/viewer_api.js:7` = `'promql'`). Selection
logic in `src/viewer/assets/lib/data.js:361,379`. When the
server-backed viewer migrates, dual-emission collapses to bare
`plot_sql` (already defined at `plot.rs:169`).

### Why the JS/Rust split inside `viewer-sql`

`duckdb-rs` doesn't build for `wasm32`; the browser DuckDB is
`duckdb-wasm`, a separate library (C++→WASM + JS host API).
Bridging Rust→JS via `wasm-bindgen` would duplicate the JS host
shim that already ships with the library, so duckdb-wasm lives
in JS at the boundary:

- **JS** (`site/viewer-sql/lib/duckdb-registry.js`, 926 LOC):
  worker pool, parquet attachment, schema introspection, per-
  source view aliasing, cgroup index table,
  `__SELECTED_CGROUPS__` substitution, result cache.
- **Rust→WASM** (`crates/viewer-sql/src/lib.rs`, 716 LOC):
  `SqlMetadata` (implements `DashboardData`), Arrow batch →
  Prometheus matrix marshalling (BigInt workaround),
  `query_range` CTE wrap, pre-flight column validation. Exposes
  `pure_sql_macros()` returning `macros.sql` bytes for the JS
  host to register on the connection.

The detailed duckdb-wasm constraints (async-only workers, no JS
UDFs, macro-registration quirks) live in
`crates/viewer-sql/duckdb.md`.

---

## Two macro libraries (known hazard)

| Side | File | Macros | Mechanism |
|---|---|---:|---|
| Native | `/work/metriken/metriken-query-sql/src/macros.rs` | 19 | Rust strings via `register_all` |
| Wasm | `crates/viewer-sql/src/macros.sql` | 30 | Pure SQL via `CREATE MACRO`, `include_str!`'d |

The extra 11 wasm macros re-implement the Rust H2 vscalar UDFs
in pure SQL — duckdb-wasm doesn't accept Rust registrations from
JS. Parity is tested by `crates/viewer-sql/tests/macros.rs` (334
LOC, 17 tests): loads `macros.sql` into a native in-memory
DuckDB and asserts shared primitives match.

**This catches behavioural drift, not signature drift.**
`hist_irate_quantile` / `hist_rate5m_quantile` already differ:
native is `(buckets, q, ts, p)`, wasm is `(buckets, q, ts)`. A
single shared `.sql` file `include_str!`'d from both sides would
close it; deferred to keep this branch focused.

---

## Test coverage

171 Rust tests pass, 11 ignored. Eight frontend `.mjs` test
files under `tests/` runnable via `node --test tests/*.mjs`. New
on this branch:

| Layer | Where | Coverage |
|---|---|---|
| Dashboard SQL emitters | `crates/dashboard/tests/sql_snapshots.rs` + `tests/snapshots/*` | 18 `insta` snapshots, one per public emitter in `crates/dashboard/src/sql.rs`. |
| Wasm/native macro parity | `crates/viewer-sql/tests/macros.rs` | 17 tests asserting wasm `macros.sql` behaviour matches the native macros. Surfaced the `h2_combine` signature divergence (variadic `UBIGINT[]` vs `LIST<LIST<UBIGINT>>`). |
| Plot API surface | `crates/dashboard/src/plot.rs` (inline) | 12 tests on `plot_promql_with_sql` / `plot_sql` round-trip + dual-emission shape. |
| Service extension / KPI | `crates/dashboard/src/service_extension.rs` (inline) | 7 tests on KPI deserialization. |
| Frontend JS | `tests/*.test.mjs` | 8 files: compare math, compare/node filter, heatmap data + resolution, section cache, sections API, selection migration, service routes. |

Not directly unit-tested: `crates/viewer-sql/src/lib.rs` (716
LOC) and `site/viewer-sql/lib/duckdb-registry.js` (926 LOC).
Both are intentionally thin and exercised end-to-end by the
static viewer's puppeteer smoke tests.

---

## Verification: correctness harness

Lives in metriken at
`/work/metriken/metriken-query/examples/sql_vs_promql.rs` (1,719
LOC). Run from `/work/metriken`:

```bash
cargo run --release --example sql_vs_promql --features "legacy,sql" \
  -- /work/rezolus/site/viewer/data/demo.parquet
```

Output: `/tmp/sql_vs_promql_yv/<parquet>/<plot>.json`; top-level
summary at `summary.json`.

**Numerical results from the prior run are stale** by ~30
commits; several known divergences have been fixed (see
below). **Rerun before the PR opens.**

### Known divergence taxonomy (from the stale run)

The buckets the prior harness produced. Counts overstate the
current state — fixes that landed on this branch are cited.

| count | category | meaning / status |
|------:|---|---|
| 985 | SQL view missing the metric | Numeric-encoded parquets (`AB_*`, `*_gemma3`, `cachecannon`) store metric names in Arrow field metadata, not column names. Static viewer renders these mostly empty too. **Likely older fixtures**; confirm before treating as a real issue. |
| 167 | `rate_5m` boundary | Old positional-LAG implementation. The macro is range-based on this branch — `macros.sql:183-185`. **Stale.** |
| 55 | numerical drift (rel ≥ 1e-3) | Multi-source aggregation gap on the two parquets with multiple `rezolus` sources. **Fixed in `5dbe881`, `1bbd6b2`, `a415fbe`, `895801b`.** |
| 26 | series count differs | Same root cause as the 55. |
| 23 | SQL produces series, PromQL doesn't | Inverse of the 985, rarer. |
| 16 | small numerical drift (rel < 1e-3) | Sub-ms timestamp jitter in irate windows. Acceptable noise. |
| 9 | label set mismatch | Multi-source labelling (`source=rezolus` only on PromQL side). |
| 8 | SQL duplicate samples per timestamp | `busy-pct-per-cpu` / `cpu-busy-heatmap` missed a `sum by (id)`. **Fixed in `f6792ff`.** |

Prior-run tolerance: `rel_tol = 1e-9`, `abs_tol = 1e-12`; ±2
boundary samples allowed.

---

## Where to spend attention

1. **`crates/viewer-sql/src/lib.rs`** (716). The wasm-bindgen
   surface, Arrow → Prometheus matrix marshalling (BigInt
   workaround is subtle), and the `query_range` CTE wrapper.
2. **`crates/dashboard/src/sql.rs`** (627). SQL emitter helpers.
   Pattern shown by `rate_5m_total` (`:33`), `concept_total`
   (`:281`), `irate_sum_by_id` (`:126`).
3. **`site/viewer-sql/lib/duckdb-registry.js`** (926). JS
   `CaptureRegistry` + worker pool. Public surface mirrors the
   now-deleted legacy `WasmCaptureRegistry`.
4. One dashboard category end-to-end — e.g.
   `crates/dashboard/src/dashboard/cpu.rs` (445 LOC). Each
   `group.plot_promql_with_sql(...)` shows the dual-query
   pattern in context; the per-CPU plot at `cpu.rs:31` is the
   one that surfaced the `sum by (id)` divergence fixed in
   `f6792ff`.
5. **`crates/viewer-sql/src/macros.sql`** (242, 30 macros) plus
   the parity test at `crates/viewer-sql/tests/macros.rs` (334).

Skip:

- The PromQL transform helpers in `src/viewer/assets/lib/data.js`
  (`rewriteCounterQuery`, `injectLabel`,
  `substituteCgroupPattern`, `PROMQL_KEYWORDS`) — dead in
  static-viewer mode.
- `crates/dashboard/src/service_extension.rs` (693) and
  `crates/dashboard/src/plot.rs` (879) — additive, largely
  unchanged from `main`.

---

## Reading order

44 commits; **don't squash**. Roughly:

- **Pre-migration**: viewer-sql crate scaffold, multi-worker
  pool, combined queries.
- **Stages 1–3**: extract `duckdb-registry.js`, wire
  `CaptureRegistry` into Mithril, source picker, retire
  `crates/viewer/` (`ad1ad9e`, net −1,960 across 11 files —
  dominated by the 4.7 MB compiled WASM blob).
- **Post-Stage 3 fixes**: backend gating (`af867b5`), multi-
  source aggregation (`5dbe881`, `1bbd6b2`, `a415fbe`,
  `895801b`), `cpu_pct_by_id` aggregation (`f6792ff`),
  zero-series emitters (`4c25c37`), counter-overflow reset
  (`889b85f`).
- **Cleanup**: dead shadow-mode plumbing removed (`519c24c`),
  unit tests for the DuckDB layer added, ad-hoc puppeteer probes
  dropped.

---

## Open questions

1. **Live-agent and MCP migration.** Neither uses the SQL
   backend today. The decision determines how long
   `metriken-query`'s `legacy` feature stays alive — and is
   coupled to the metriken-side question of whether to land or
   demote the unused `Engine` layer (see
   `/work/metriken/REVIEWING.md`).
2. **KPI translation.** Plots whose query lives in parquet
   `service_queries` metadata still arrive as raw PromQL
   strings; the viewer shows "(query not yet available —
   translation pending)" (`data.js:441`,
   `charts/chart.js:382`). Fix at `parquet annotate` time.
3. **Server cgroup selector.** Stage 2b made the cgroup selector
   SQL-only; the server-backed viewer no longer has a working
   cgroup page.
4. **Compare-mode combined section.** `regenerate_combined`
   ships the per-capture half; case-(b) "one combined category
   section" still needs multi-capture `init_templates` on
   `viewer-sql`.
5. **Macro library consolidation.** See "Two macro libraries"
   above. A shared `.sql` file closes the signature-drift gap.
6. **Numeric-encoded parquet support.** Teach the SQL emitter to
   read Arrow field metadata, or document the unsupported
   fixture set. 985 of the prior 1,370 divergences live here.
