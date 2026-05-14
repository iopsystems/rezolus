# Reviewing the `yv/sql-testing` branch (rezolus side)

This doc orients a reviewer cold. Every concrete claim below is tied
to a `file:line` in the working tree at commit `ea61bfd`; counts come
from `wc -l` / `grep -c` / `git rev-list --count` over those files.
Companion doc: `/work/metriken/REVIEWING.md` (engine side).

The previous version of this file made several claims that turned out
to be stale; this rewrite cites each claim to a verifiable source.

---

## TL;DR

The static (file-mode) browser viewer was migrated from a Rust → WASM
crate running an in-process PromQL evaluator (`crates/viewer/`,
deleted) to a duckdb-wasm-backed setup (`crates/viewer-sql/`, new).
Every dashboard plot now carries both a `promql_query` and a
`sql_query` string; the frontend picks one based on a `BACKEND`
constant.

The picture for the **server-backed viewer (`rezolus view`) and
MCP**:

- **`crates/viewer/`** (old, WASM PromQL engine) was deleted in
  Stage 3 commit `ad1ad9e` — "Stage 3: retire `crates/viewer/` —
  viewer is SQL-only".
- **`rezolus view <parquet>` server-side handler** at
  `src/viewer/mod.rs:1727-1731` and `:1756-1760` instantiates
  `QueryEngine::new(&*tsdb)` and tries to chain
  `.with_dispatch(cfg)` from `dispatch_for_capture(...)`. **This
  code does not currently compile** — `promql::DispatchConfig`,
  `metriken_query::DispatchObserver`, `metriken_query::Diff`, and
  `metriken_query::Mode` no longer exist in `metriken-query` (see
  "Known current build issue" below).
- **MCP** (`src/mcp/`) constructs `QueryEngine::new(...)` via the
  re-export at `src/viewer/mod.rs:63` (`pub use metriken_query::promql;`).
  Compiles cleanly *as a module*, but transitively depends on
  `src/viewer/mod.rs` which doesn't compile, so the binary as a whole
  fails.

The static viewer (`site/viewer/`) is unaffected by the binary build
because it builds via `./crates/viewer-sql/build.sh` (wasm-pack)
independently — see `crates/viewer-sql/build.sh:1-13`. The build
produces `site/viewer-sql/pkg/wasm_viewer_sql.js`, which the static
page loads.

Net diff vs `main` (after review-prep cleanup commit `ea61bfd`):
**+6,968 / −2,266 across 66 files** (`git diff main...yv/sql-testing
--shortstat`). `git rev-list --count main..yv/sql-testing` → **38**
commits.

---

## Stale claims removed from the prior version of this doc

1. **"Shadow-mode dispatch is wired."** The previous TL;DR described
   `rezolus view <parquet>` as running PromQL and DuckDB
   side-by-side via `metriken-query`'s `DispatchConfig`. The code at
   `src/viewer/mod.rs:1640-1700` is structured that way, but the
   types it references **do not exist in `metriken-query`** anymore
   (verified: `grep -rn 'DispatchConfig\|DispatchObserver' /work/metriken/`
   returns nothing; `grep -rn 'pub enum Mode' /work/metriken/` same).
   Likely removed in metriken commit `a25e285` "collapse PromQL
   evaluator to streaming-only". This is the binary's current build
   blocker; see "Known current build issue".

2. **"Those types still exist in metriken-query, but they're
   feature-gated behind `sql`."** The prior FAQ explanation for the
   build failure. Wrong: `CatalogueEntry` is feature-gated;
   `DispatchConfig`/`DispatchObserver`/`Diff`/`Mode` are gone.

3. **"37 commits ahead of main."** Now 38
   (`git rev-list --count main..yv/sql-testing` → 38, current HEAD
   `ea61bfd`).

4. **"net −1981 lines, 11 files" for Stage 3.** Actual:
   `git show --shortstat ad1ad9e` → "11 files changed, 8
   insertions(+), 1968 deletions(-)". Net = −1,960. The 4.7 MB wasm
   blob is the bulk: `git ls-tree -r -l ad1ad9e^ -- site/viewer/pkg/wasm_viewer_bg.wasm`
   → 4,749,893 bytes.

---

## Architecture (current code)

### Static viewer — duckdb-wasm in the browser

```
site/viewer/                            Mithril UI (file-mode entry)
  ├─ index.html                         loads site/viewer/lib/script.js
  └─ lib/viewer_api.js:6                const BACKEND = 'sql';

  imports:
    site/viewer-sql/pkg/wasm_viewer_sql.js
        ← built by crates/viewer-sql/build.sh from crates/viewer-sql/src/lib.rs (716 LOC)
        ← exports ViewerSql, pure_sql_macros, ...
    site/viewer-sql/lib/duckdb-registry.js  (926 LOC)
        ← JS-side multi-worker DuckDB pool, schema introspection,
          per-source aliasing, cgroup index table
    https://cdn.jsdelivr.net/npm/@duckdb/duckdb-wasm@1.33.1-dev45.0/+esm
        ← imported at site/viewer-sql/lib/script.js:12
```

This path has no `metriken-query` dependency at all
(`crates/viewer-sql/Cargo.toml` has no `metriken-query` in its
`[dependencies]`).

### Server-backed viewer + MCP — PromQL only (currently broken)

```
rezolus view <parquet | live-agent-url>
  → src/viewer/mod.rs::serve(...)
      → AppState::new (mod.rs:762)
          ├─ captures: CaptureRegistry::new(tsdb, ...)
          └─ sql_backend: Arc::new(metriken_query_sql::DuckDbBackend::new())
                          (mod.rs:768) — wired but dead-end, see below

  /api/v1/query_range handlers at mod.rs:1727-1731 and :1756-1760:
      let engine = QueryEngine::new(&*tsdb);
      let engine = match dispatch_for_capture(&state, capture) {
          Some(cfg) => engine.with_dispatch(cfg),    // ← does not compile
          None => engine,
      };
  → response: Prometheus matrix JSON

MCP (src/mcp/)
  → src/mcp/mod.rs:11 — use crate::viewer::promql::{QueryEngine, QueryResult};
  → src/mcp/mod.rs:119,144,177 — QueryEngine::new(tsdb.clone())
  → uses Tsdb only (no dispatch references in src/mcp/)
```

The dispatch/shadow-mode plumbing at `src/viewer/mod.rs:1640-1700`
references `promql::DispatchConfig`, `metriken_query::DispatchObserver`,
`metriken_query::CatalogueEntry`, `metriken_query::Diff`, and
`metriken_query::Mode` — none of these exist in metriken-query today.
See "Known current build issue" for fix options.

### Dashboard crate — both queries per plot

`crates/dashboard/Cargo.toml:13` enables `features = ["legacy"]` on
its `metriken-query` dep. Its only metriken-query usage is
`metriken_query::Tsdb` (`crates/dashboard/src/data.rs:12`,
`crates/dashboard/src/lib.rs:8`).

Each plot calls `plot_promql_with_sql(...)`:

- Group-level shim at `crates/dashboard/src/plot.rs:184`:
  ```rust
  pub fn plot_promql_with_sql(
      &mut self,
      opts: PlotOpts,
      promql_query: String,
      sql_query: String,
  ) {
      self.tail_subgroup_mut()
          .plot_promql_with_sql(opts, promql_query, sql_query);
  }
  ```
- Subgroup variant at `crates/dashboard/src/plot.rs:280`, with the
  doc-comment:
  > "During the Phase D migration window this lets each plot serve
  > both the legacy viewer (which evaluates `promql_query` via the
  > in-memory PromQL engine) AND `viewer-sql` (which evaluates
  > `sql_query` against duckdb-wasm). Once the legacy viewer is
  > retired we convert these calls to bare `plot_sql`."

Both query strings end up in the dashboard JSON the frontend
consumes. The full set of dashboard category files is in
`crates/dashboard/src/dashboard/*.rs` — 15 files including 11 active
category emitters (blockio, cgroups, cpu, gpu, memory, network,
overview, rezolus, scheduler, softirq, syscall) and 4 infrastructure
files (category.rs, mod.rs, query_explorer.rs, service.rs).

### Frontend backend selection

The `BACKEND` constant is hard-coded per viewer copy
(non-symlinked):

- `site/viewer/lib/viewer_api.js:6` — `const BACKEND = 'sql';` (static
  viewer)
- `src/viewer/assets/lib/viewer_api.js:7` — `const BACKEND = 'promql';`
  (server-backed copy)
- Both expose it via `backend()` — `site/viewer/lib/viewer_api.js:135`
  and `src/viewer/assets/lib/viewer_api.js:158`.

`buildEffectiveQuery` at `src/viewer/assets/lib/data.js:361` selects
the right query string. The SQL short-circuit is at
`src/viewer/assets/lib/data.js:379`:

```js
if (backend === 'sql') {
    if (plot.sql_query) return plot.sql_query;
    return null;
}
```

KPI placeholder ("(query not yet available — translation pending)") at
`src/viewer/assets/lib/data.js:441` and the chart-side fallback at
`src/viewer/assets/lib/charts/chart.js:382` (`'(query not yet
available)'`).

The PromQL transforms (`rewriteCounterQuery` at `data.js:32`,
`PROMQL_KEYWORDS` at `data.js:59`, `injectLabel` at `data.js:75`,
`substituteCgroupPattern` at `data.js:129`) are live for the
server-backed path (where `BACKEND='promql'`) and dead for the
static-viewer path.

---

## Why the JS / Rust split (in viewer-sql)

DuckDB's official Rust crate (`duckdb-rs`) does not compile to
`wasm32`. The bundled C build it wraps has no upstream wasm target.

The browser-side DuckDB is **duckdb-wasm**, a separate library
(C++ → WASM + JS host API). Calling it from Rust → WASM would
require building a `wasm-bindgen` ↔ `duckdb-wasm` JS bridge.
Instead the architecture puts duckdb-wasm in JS in the first place:

- **JS side** (`site/viewer-sql/lib/duckdb-registry.js`, 926 LOC).
  Owns the duckdb-wasm `AsyncDuckDB` instances, the round-robin
  worker pool, parquet attachment, schema introspection, per-source
  view aliasing, cgroup index table, `__SELECTED_CGROUPS__` SQL
  substitution, result cache, query gating, source picker state.

- **Rust → WASM side** (`crates/viewer-sql/src/lib.rs`, 716 LOC).
  Owns `SqlMetadata` (implements `DashboardData`), Arrow batch →
  Prometheus matrix JSON marshalling (BigInt workaround), time-window
  CTE wrapping, pre-flight column validation. Exports
  `pure_sql_macros()` returning the bytes of `macros.sql`.

The detailed constraints — async-only workers, JS UDFs not viable,
macro registration quirks — live in `crates/viewer-sql/duckdb.md`
(214 LOC).

Macro source-of-truth split:

- Wasm side: `crates/viewer-sql/src/macros.sql` (242 LOC, **30**
  `CREATE MACRO` statements — `grep -c 'CREATE.*MACRO'
  crates/viewer-sql/src/macros.sql` → 30). `include_str!`'d at
  runtime.
- Native side: `/work/metriken/metriken-query-sql/src/macros.rs`
  (314 LOC, **19** `CREATE OR REPLACE MACRO` strings).
- Parity check: `crates/viewer-sql/tests/macros.rs` (123 LOC) loads
  `macros.sql` into a native in-memory DuckDB and asserts behaviour
  against the native macros. Catches drift but does **not** prove
  the inventories are 1:1 — they differ in count (30 vs 19).

---

## Known current build issue

`cargo check --bin rezolus` currently fails. Two distinct things:

1. **BPF prerequisites missing on this dev box.** `build.rs:169`
   panics with "failed to execute `clang`: No such file or
   directory". Not a code bug. Install clang to get past it.

2. **Type references that no longer exist in metriken-query.**
   `src/viewer/mod.rs` references:

   | Reference site | Type |
   |---|---|
   | `:1650` | `promql::DispatchConfig` (return type of `dispatch_for_capture`) |
   | `:1663` | `promql::DispatchConfig` (struct literal in `Some(...)`) |
   | `:1676` | `metriken_query::DispatchObserver` (impl for `LoggingDispatchObserver`) |
   | `:1677` | `metriken_query::CatalogueEntry`, `metriken_query::Diff` (in `on_diff` signature) |
   | `:1695` | `metriken_query::Mode` (`on_query`?) |

   None of `DispatchConfig`, `DispatchObserver`, `Diff`, `Mode` exist
   in `/work/metriken/` (verified by grep). `CatalogueEntry` exists
   but is feature-gated behind `sql` and the rezolus binary doesn't
   enable `sql` (`Cargo.toml:78` enables only `ingest, lz4`).

   Fix options:
   - **Remove the dispatch/shadow-mode code path.** Drop
     `dispatch_for_capture` (`:1640-1670`), `LoggingDispatchObserver`
     (`:1675-1700`), and the `with_dispatch` chains at `:1727-1731`
     and `:1756-1760`. Also drop the unused `sql_backend` field at
     `:751` and its initialization at `:768`. Server-backed viewer
     then runs PromQL only, matching `BACKEND='promql'` at
     `src/viewer/assets/lib/viewer_api.js:7`.
   - **Restore the dispatch types on the metriken side.** Reverse
     part of metriken commit `a25e285` to bring back
     `DispatchConfig`/`DispatchObserver`/etc. Heavier.

   Recommended: (1) for this branch, defer the shadow-mode story to
   a follow-up that designs it freshly atop the new metriken
   surface.

3. **Dead-code warning in dashboard.** `cargo check -p dashboard`
   succeeds but warns:
   ```
   warning: constant `RATIO_X1000` is never used
     --> crates/dashboard/src/dashboard/cpu.rs:8:7
   ```
   Cosmetic; safe to remove.

---

## Stage narrative (verified commit hashes)

Each commit hash below was verified with
`git log --oneline -1 <hash>` on `yv/sql-testing`. All 20 resolve.

| Stage | Commit | Subject |
|---|---|---|
| pre-1 (perf) | `eeb02d5` | `viewer-sql: multi-worker query pool (Phase 1 perf)` |
| pre-1 (perf) | `9707a42` | `viewer-sql: combined queries for simple-gauge + irate_total plots (Phase 2 perf)` |
| 1 | `033b8bb` | `viewer-sql: extract lib/duckdb-registry.js — Stage 1 of viewer migration` |
| 2 | `5a7b778` | `Stage 2: wire CaptureRegistry into the Mithril viewer (SQL boot)` |
| 2 (fix) | `29b2359` | `viewer: cache section structure synchronously, defer query fetch` |
| 2b | `eb5553e` | `Stage 2b: cgroup selector → registry-state model` |
| 2c | `b099b97` | `Stage 2c: source picker UI + KPI temporarily-missing placeholders` |
| 2c (perf) | `a38471a` | `viewer: cgroup selector progressive redraw` |
| 2d | `80f29db` | `Stage 2d: regenerate_combined initialises both captures' templates` |
| 3 | `ad1ad9e` | `Stage 3: retire crates/viewer/ — viewer is SQL-only` (net −1,960 across 11 files) |
| post-3 | `af867b5` | `viewer: gate SQL/PromQL query selection on ViewerApi.backend()` |
| post-3 | `f6792ff` | `dashboard: fix cpu_pct_by_id missing sum-by-id aggregation` |
| post-3 | `5dbe881` | `viewer-sql: aggregate across rezolus sources to match PromQL` |
| post-3 | `1bbd6b2` | `viewer-sql: per-source ':src<i>' aliases for avg/max/min emitters` |
| post-3 | `a415fbe` | `viewer-sql: detect rezolus sources via Arrow field metadata` |
| post-3 | `895801b` | `viewer: surface combined-rezolus view in source picker` |
| post-3 | `d30343f` | `viewer-sql: converge macros with native + add test scaffold` |
| post-3 | `4c25c37` | `dashboard: fix four zero-series emitters` |
| post-3 | `5f7a49d` | `dashboard: rate-then-sum for irate_total + irate_sum_by_id helper` |
| post-3 | `889b85f` | `dashboard: reset-aware sql::rate_5m_total — handle counter overflow` |
| review prep | `ea61bfd` | `review prep: add REVIEWING.md, .gitattributes; drop dead echarts symlink` |

Don't squash — the stage-by-stage history is reading order for a
commit-by-commit review.

---

## Where to spend attention (in 1 hour)

1. **`crates/viewer-sql/src/lib.rs`** (716 LOC). The wasm-bindgen
   surface, Arrow → Prometheus matrix marshalling (BigInt workaround
   is subtle), and the `query_range` CTE wrapper.
2. **`crates/dashboard/src/sql.rs`** (627 LOC). SQL emitter helpers
   — most per-plot SQL logic. Pattern shown by `rate_5m_total`
   (`crates/dashboard/src/sql.rs:33`), `concept_total`
   (`:281`), and `irate_sum_by_id` (`:126`).
3. **`site/viewer-sql/lib/duckdb-registry.js`** (926 LOC). JS
   `CaptureRegistry` + worker pool. Public API mirrors the
   now-deleted legacy `WasmCaptureRegistry` surface.
4. Pick one dashboard category (e.g.
   `crates/dashboard/src/dashboard/cpu.rs`, 446 LOC) and read it
   end-to-end. Each `group.plot_promql_with_sql(...)` call shows the
   dual-query pattern in context.
5. **`crates/viewer-sql/src/macros.sql`** (242 LOC, 30 macros). The
   wasm-side SQL macros. Parity scaffold at
   `crates/viewer-sql/tests/macros.rs` (123 LOC):
   > "`macros.sql` is `include_str!`'d at runtime and registered
   > against a browser-side AsyncDuckDB connection. ... These tests
   > load the same SQL into a native DuckDB connection and assert the
   > per-second rate and 5-minute rate primitives behave the same way
   > as their counterparts in
   > /work/metriken/metriken-query-sql/src/macros.rs ..."

Things to skip:

- The PromQL transform helpers in `src/viewer/assets/lib/data.js`
  (`substituteCgroupPattern`, `injectLabel`, `rewriteCounterQuery`,
  `PROMQL_KEYWORDS`, the PromQL branch of `buildEffectiveQuery`).
  Live for the server-backed viewer / MCP path, dead in static-viewer
  mode.
- `crates/dashboard/src/service_extension.rs` (693 LOC). KPI
  definition structures, largely unchanged from `main`.
- `crates/dashboard/src/plot.rs` (879 LOC). Plot metadata and the
  `plot_promql_with_sql` API surface — incremental, not a rewrite.

---

## Mental models for the adversarial reviewer

**1. The engine lives in the browser. The frontend speaks SQL** —
for the static viewer. The server-backed viewer and MCP are still
PromQL.

**2. The Rust crate (`crates/viewer-sql/`) is intentionally thin.**
It doesn't own the database. It owns marshalling. The bulk of the
"viewer migration" diff is the JS registry, not the Rust crate.

**3. Dual-query per plot is a transition artifact.** Each plot's
`(promql_query, sql_query)` pair exists because the server-backed
viewer and MCP still drive PromQL. When those migrate, dual
emission collapses to bare `plot_sql`.

**4. The shadow-mode dispatch story is currently broken**, not
working. The plumbing exists in `src/viewer/mod.rs:1640-1700` but
references gone-from-metriken types. See "Known current build
issue".

---

## FAQ

**Q: Why retire `crates/viewer/`?**

A: It was the static viewer's WASM PromQL engine — ran client-side
over a parquet file. The server's PromQL still runs **native** through
`metriken-query`. Two different code paths; only the first migrated.
The retirement (Stage 3, `ad1ad9e`) deleted 11 files (net −1,960
lines), dominated by the 4.7 MB compiled WASM blob.

**Q: Why two SQL macro libraries?**

A: Native macros (`/work/metriken/metriken-query-sql/src/macros.rs`,
314 LOC, 19 macros, Rust calling `duckdb-rs`) and wasm macros
(`crates/viewer-sql/src/macros.sql`, 242 LOC, 30 macros, pure SQL via
`include_str!`) are two parallel copies. Why two:

- Native version is in Rust because the harness loads it via
  `duckdb-rs::register_all` and runs unit tests against it.
- Wasm version is pure SQL because `duckdb-wasm` doesn't take Rust
  registrations — the browser-side macros must be `CREATE MACRO`
  statements the JS host can hand to the connection.

This is a known hazard. The early-warning system is
`crates/viewer-sql/tests/macros.rs` — a native test that loads
`macros.sql` into an in-memory DuckDB and asserts parity for the
shared primitives. **It does not assert the inventories are
1:1** — the macro counts differ (30 vs 19).

**Q: Why is `duckdb-registry.js` 926 lines?**

A: It owns: worker pool (round-robin checkout), parquet attachment +
schema introspection (column → metric-name index), per-source
aliasing views, cgroup index table construction (one `SELECT
DISTINCT` per cgroup-style column at load time), result cache
(keyed by SQL string), query gating (PromQL vs SQL via
`ViewerApi.backend()`), source picker state, `__SELECTED_CGROUPS__`
substitution. Each concern is roughly 100 LOC; the file is large
because all of it lives in JS by necessity.

**Q: What's the live-agent migration story?**

A: `rezolus view http://localhost:4241` polls the agent's msgpack
endpoint into an in-memory `Tsdb` and serves PromQL via
`/api/v1/query_range`. There is no parquet file on disk to point
DuckDB at. Options:

1. **Server-side SQL.** Replace the in-memory `Tsdb` with a
   continuously appended duckdb relation. Frontend keeps hitting
   `/api/v1/query_range` but with SQL bodies.
2. **Client-side SQL.** Server rotates a parquet file
   (Hindsight-style); frontend pulls parquet bytes and runs
   duckdb-wasm locally. Same code path as file mode. Eliminates
   server PromQL entirely.

Not mutually exclusive. Not in scope for this branch.

**Q: Why is `cargo check --bin rezolus` failing on `Catalogue`,
`DispatchConfig`, `DispatchObserver`, `Diff`, `Mode`?**

A: Different reasons per type:

- **`CatalogueEntry`** and **`Catalogue::embedded()`** exist in
  metriken but are feature-gated behind `sql`
  (`/work/metriken/metriken-query/src/lib.rs:41-42`). The rezolus
  binary's metriken-query dep at `Cargo.toml:78` enables only
  `ingest, lz4` — no `sql`. Adding `sql` would fix the
  `CatalogueEntry`/`Catalogue` references.
- **`DispatchConfig`, `DispatchObserver`, `Diff`, `Mode`** **do not
  exist anywhere in `/work/metriken/`** (verified by grep).
  Adding the `sql` feature will *not* fix these. The shadow-mode
  dispatch types appear to have been removed in metriken commit
  `a25e285` "collapse PromQL evaluator to streaming-only".

The fix is to remove the dispatch code path on the rezolus side
(see "Known current build issue" for the file:line list) or restore
the types on the metriken side. Neither has happened on this branch.

---

## Verification

### Cargo / build

| Command | Status |
|---|---|
| `cargo check -p dashboard` | passes (1 dead-code warning: `RATIO_X1000` at `crates/dashboard/src/dashboard/cpu.rs:8`) |
| `cargo check -p viewer-sql` | passes (1 `unused_mut` warning at `crates/viewer-sql/src/lib.rs:560`) |
| `./crates/viewer-sql/build.sh` | wasm-pack build of the static viewer crate (independent of the binary) |
| `cargo check --bin rezolus` | **fails** — see "Known current build issue" |

### Correctness harness

Lives in metriken at
`/work/metriken/metriken-query/examples/sql_vs_promql.rs` (1,719 LOC).
Requires features `legacy, sql`
(`/work/metriken/metriken-query/Cargo.toml:14-16`):

```toml
[[example]]
name = "sql_vs_promql"
required-features = ["legacy", "sql"]
```

Run from `/work/metriken`:

```
cargo run --release --example sql_vs_promql \
    --features "legacy,sql" \
    -- /work/rezolus/site/viewer/data/demo.parquet
```

Output lands in `/tmp/sql_vs_promql_yv/<parquet>/<plot>.json` (per
the example source). Summary at `summary.json`.

**Reviewer note: numerical results below are *not* re-verified at
this commit.** They were taken from `/tmp/sql_vs_promql_yv/summary.json`
generated against a prior metriken HEAD. The branch has advanced
since; rerun the harness before the PR is opened, then refresh this
table:

| parquet         | identical | tolerant | divergent |
|-----------------|----------:|---------:|----------:|
| demo            |       179 |        1 |        22 |
| vllm            |       181 |        2 |        19 |
| cachecannon     |        87 |        0 |       115 |
| AB_base         |        66 |        0 |       136 |
| AB_base_pin     |        70 |        0 |       132 |
| AB_level        |        70 |        0 |       132 |
| AB_level_pin    |        66 |        0 |       136 |
| sglang_gemma3   |        34 |        0 |       168 |
| vllm_gemma3     |        25 |        0 |       177 |
| disagg-sglang   |        34 |        0 |       168 |
| sglang-nixl-16c |        37 |        0 |       165 |
| **total**       |   **849** |    **3** |  **1370** |

Each parquet contributes 250 plots; 48 are cgroup-only and skipped by
the harness, so each row sums to 202 plots (202 × 11 = 2,222 pairs;
plus 528 skipped = 2,750).

---

## Known divergence taxonomy (stale; rerun the harness)

The buckets below describe shapes the prior harness run produced.
Several were *fixed in subsequent commits* on this branch (cited
below); the divergence counts therefore overstate the current state.

| count | category | what it means |
|------:|---|---|
| 985 | SQL view missing the metric | Numeric-encoded parquets (`AB_*`, `*_gemma3`, `cachecannon`) store metric names in Arrow field metadata, not column names. Dashboard SQL references columns by metric name; the SQL emitter does not yet read Arrow metadata at view-build time on every code path. Architectural cliff — the static viewer renders these mostly empty too. Likely older test fixtures; worth confirming with the team. |
| 167 | `rate_5m` boundary | Reflects the older positional-LAG implementation. The macro is **already** range-based on this branch at `crates/viewer-sql/src/macros.sql:183-185` (and on the native side at `metriken-query-sql/src/macros.rs`). **Count is stale.** |
| 55 | Larger numerical drift (rel ≥ 1e-3) | All on `disagg-sglang.parquet` (40) or `sglang-nixl-16c.parquet` (15) — the two parquets with multiple `rezolus` sources. Multi-source aggregation gap; PromQL summed across all sources, SQL queried one. **Fix landed in commits `5dbe881`, `1bbd6b2`, `a415fbe`, `895801b` — re-run to confirm.** |
| 26 | Series count differs | Same root cause as the 55. |
| 23 | SQL produces series, PromQL doesn't | Inverse of category 1, rarer. |
| 16 | Small numerical drift (rel < 1e-3) | irate window-math jitter from sub-millisecond timestamp drift. Acceptable noise. |
| 9 | Label set mismatch | Multi-source labelling differences (e.g. `source=rezolus` only on PromQL side). |
| 8 | SQL duplicate samples per timestamp | `busy-pct-per-cpu` and `cpu-busy-heatmap` previously emitted 2× rows per `(timestamp, cpu_id)` because the SQL form missed a `sum by (id)`. **Fix landed in commit `f6792ff` "dashboard: fix cpu_pct_by_id missing sum-by-id aggregation"** — re-run to confirm. |

Tolerance settings: `rel_tol = 1e-9`, `abs_tol = 1e-12`; ±2 boundary
samples allowed for grid-evaluation artefacts (per the prior summary
JSON; check `summary.json` after a fresh run).

### The multi-source root cause (for the 55+26 numbers)

- Legacy PromQL evaluator's `Tsdb` stores all sources' series
  together and aggregates across them on `sum(...)`, `sum by (id)`,
  `histogram_quantiles(...)`.
- Pre-fix dashboard SQL picked **one source view** (named in the
  parquet filename, fallback to the first rezolus source) and queried
  only that view.

| parquet         | rezolus sources | count of cases | ratios observed |
|-----------------|----------------:|---------------:|-----------------|
| disagg-sglang   |               2 |             40 | promql ≈ 2× sql |
| sglang-nixl-16c |               3 |             15 | promql ≈ 3× sql |

Fix decision was **(a) aggregate across all rezolus sources in SQL**
to match PromQL — landed across commits `5dbe881`, `1bbd6b2`,
`a415fbe`, `895801b`.

### `rate_5m` is already range-based

`crates/viewer-sql/src/macros.sql:183-185`:

```sql
CREATE OR REPLACE MACRO rate_5m(c, ts) AS
    (c - first_value(c) OVER (ORDER BY ts RANGE BETWEEN 300000000000 PRECEDING AND CURRENT ROW))
    / NULLIF((ts - first_value(ts) OVER (ORDER BY ts RANGE BETWEEN 300000000000 PRECEDING AND CURRENT ROW))::DOUBLE / 1e9, 0);
```

### `cpu-busy-heatmap` / `busy-pct-per-cpu` are aggregated

`crates/dashboard/src/dashboard/cpu.rs:32-38`:

```rust
busy.plot_promql_with_sql(
    PlotOpts::counter("Busy % (Per-CPU)", "busy-pct-per-cpu", Unit::Percentage)
        .percentage_range(),
    "sum by (id) (irate(cpu_usage[5m])) / 1000000000".to_string(),
    sql::cpu_pct_by_id("^cpu_usage/[a-z]+/[0-9]+$", "/([0-9]+)$"),
);
```

The helper body in `crates/dashboard/src/sql.rs:216-237` ends with
`SUM(rate_ns) / 1e9 AS v ... GROUP BY ...`, which is the `sum by (id)`
aggregation the divergence row was missing.

---

## Open questions / out-of-scope follow-ups

These don't block landing this branch but shape the path forward:

1. **Live-agent and MCP migration.** Neither calls the SQL backend
   today. Decision affects how long `metriken-query`'s `legacy`
   feature stays alive.
2. **KPI translation.** Plots whose query lives in parquet
   `service_queries` metadata still arrive as raw PromQL strings.
   Viewer shows "(query not yet available — translation pending)"
   (`src/viewer/assets/lib/data.js:441`,
   `src/viewer/assets/lib/charts/chart.js:382`). Fix at `parquet
   annotate` time.
3. **Server cgroup selector revival.** The Stage 2b rewrite made
   the cgroup selector SQL-only. The server-backed viewer no longer
   has a working cgroup page.
4. **Compare-mode "category combined section".**
   `regenerate_combined` ships the per-capture half; the case-(b)
   "one combined category section" still requires multi-capture
   `init_templates` API on `viewer-sql`.
5. **Macro library consolidation.** Two copies of the SQL macros
   (`crates/viewer-sql/src/macros.sql` 242 LOC vs
   `/work/metriken/metriken-query-sql/src/macros.rs` 314 LOC, 30 vs
   19 macros). The parity test at `crates/viewer-sql/tests/macros.rs`
   catches drift for shared primitives but doesn't enforce
   inventory parity.
6. **Numeric-encoded parquet support.** Either teach the SQL emitter
   to read Arrow field metadata, or document the unsupported fixture
   set. 985 of the prior 1,370 divergences live here.
7. **Refresh the harness numbers** before opening the PR.
8. **Fix the rezolus binary build.** Remove the dispatch/shadow-mode
   plumbing in `src/viewer/mod.rs:1640-1700` (and the unused
   `sql_backend` field at `:751`/`:768`) — see "Known current build
   issue" for the option matrix.

---

## Pointers

- Companion document (engine side): `/work/metriken/REVIEWING.md`
- Correctness harness: `/work/metriken/metriken-query/examples/sql_vs_promql.rs`
- Harness output (last run): `/tmp/sql_vs_promql_yv/` — **stale**
- Cross-branch output: `/tmp/promql_yv/`, `/tmp/promql_main/` — **stale**
- Parity test scaffold: `crates/viewer-sql/tests/macros.rs`
- CLAUDE.md: project-level architecture overview (build commands,
  operating modes, sampler architecture)
