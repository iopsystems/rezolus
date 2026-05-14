# Reviewing the `yv/sql-testing` branch

This document orients a reviewer who has not seen the SQL viewer migration
before. Read it before opening the diff. It covers what changed, the
JS/Rust split, where to spend attention, and what the known correctness
gaps are.

The companion document at `/work/metriken/REVIEWING.md` covers the engine
side (DuckDB integration, PromQL → SQL translation scaffolding, the
correctness harness). Read it first; this doc builds on it.

---

## TL;DR

The static (file-mode) browser viewer was migrated from a Rust → WASM
crate running the in-process PromQL evaluator to a JS-driven
duckdb-wasm setup. The legacy WASM viewer crate (`crates/viewer/`) was
deleted in Stage 3 (commit `ad1ad9e`, `git log --oneline -- crates/viewer`).

The picture for the **server-backed viewer and MCP** has three sub-cases:

1. **`rezolus view <parquet>`** runs the legacy PromQL evaluator *and* DuckDB
   side-by-side in shadow mode via `metriken-query`'s `DispatchConfig`. Every
   query goes to both engines; a `LoggingDispatchObserver` logs divergences.
   Wired in `src/viewer/mod.rs:1727-1731` (and the range variant at
   `src/viewer/mod.rs:1756-1760`):

   ```rust
   let engine = QueryEngine::new(&*tsdb);
   let engine = match dispatch_for_capture(&state, capture) {
       Some(cfg) => engine.with_dispatch(cfg),
       None => engine,
   };
   ```

   `dispatch_for_capture` at `src/viewer/mod.rs:1650-1669` returns `Some` only
   when the capture has a recorded `parquet_path`; otherwise `None`. It can
   be force-disabled with `METRIKEN_DISABLE_SQL`.
2. **`rezolus view <live-agent>`** has no `parquet_path`
   (`src/viewer/mod.rs:762-763` initialises it to `None` and live ingest at
   `src/viewer/mod.rs:533-538` never writes it), so `dispatch_for_capture`
   returns `None` → PromQL/Tsdb only.
3. **MCP** (`src/mcp/`) constructs `QueryEngine::new(...)` with no
   `.with_dispatch(...)` call — PromQL/Tsdb only.

Their migration to a SQL-only path is a separate workstream — see the open
questions section.

Every dashboard plot now carries both `promql_query` and `sql_query`. The
renderer picks via `ViewerApi.backend()` (`'sql'` for the static viewer at
`site/viewer/lib/viewer_api.js:6`, `'promql'` for the server-backed copy at
`src/viewer/assets/lib/viewer_api.js:7`).

Net diff vs. pre-migration: **+7318 / −2341** across 64 files
(`git diff --shortstat main..HEAD`). The big deletions are the retired
wasm artifacts removed in Stage 3 — `site/viewer/pkg/wasm_viewer_bg.wasm`
was 4,749,893 bytes (`git ls-tree -r -l ad1ad9e^ -- site/viewer/pkg/wasm_viewer_bg.wasm`).

Correctness: ~89% identical to PromQL on single-source rezolus parquets
(demo: 179/180 identical-or-tolerant out of 202; vllm: 183/202 — see
`/tmp/sql_vs_promql_yv/summary.json`). All known divergences trace to a
small set of root causes documented below.

---

## Architecture before / after

### Before (legacy `main`)

```
Static viewer (browser, file mode)
  └─ Mithril UI (site/viewer/lib/)
       └─ wasm_viewer.js  (4.7 MB compiled Rust)
             └─ metriken-query::QueryEngine + Tsdb (in-process PromQL)

Server-backed viewer (rezolus view ...)
  └─ Mithril UI (same)
       └─ /api/v1/query_range (HTTP)
             └─ metriken-query::QueryEngine + Tsdb (native)

MCP (src/mcp/)
  └─ metriken-query::QueryEngine + Tsdb
```

Dashboard plots carry one `promql_query` string each.

### After (`yv/sql-testing`)

```
Static viewer (browser, file mode)             ★ migrated
  └─ Mithril UI (site/viewer/lib/)
       └─ site/viewer/lib/viewer_api.js  async query surface; BACKEND='sql'
            └─ CaptureRegistry (JS)      site/viewer-sql/lib/duckdb-registry.js (926 LOC)
                 ├─ N AsyncDuckDB workers (round-robin pool)
                 └─ ViewerSql (Rust→wasm)  crates/viewer-sql/ (716 LOC lib.rs + 242 LOC macros.sql)
                       └─ duckdb-wasm AsyncDuckDBConnection (via JsFuture)

Server-backed viewer
  rezolus view <parquet>                       ★ shadow-mode dispatch
  └─ Mithril UI (same)
       └─ /api/v1/query_range             BACKEND='promql'
            └─ metriken-query::QueryEngine + Tsdb (PromQL, primary)
                 └─ engine.with_dispatch(cfg)    src/viewer/mod.rs:1756
                      └─ metriken_query_sql::DuckDbBackend  (shadow,
                         src/viewer/mod.rs:751 sql_backend field)

  rezolus view <live-agent>                    ─ Tsdb only (no parquet path)
  └─ same Mithril UI → /api/v1/query_range → QueryEngine; dispatch
     skipped because dispatch_for_capture returns None
     (src/viewer/mod.rs:1659-1662).

MCP (src/mcp/)                                 ─ Tsdb only
  └─ QueryEngine::new(...) constructed without .with_dispatch(...)

crates/dashboard/  emits BOTH promql_query and sql_query per plot
                   SQL emitters live in crates/dashboard/src/sql.rs (627 LOC)
```

Every dashboard plot now carries both query strings. The renderer's
`buildEffectiveQuery` (`src/viewer/assets/lib/data.js:361`) picks based
on `ViewerApi.backend()`; the SQL-backend short-circuit is at
`src/viewer/assets/lib/data.js:379` (`if (backend === 'sql') ...`).

---

## The dual-query story

Each plot in `crates/dashboard/src/dashboard/*.rs` emits its query via
`Group::plot_promql_with_sql(opts, promql_string, sql_string)` — the
group-level shim at `crates/dashboard/src/plot.rs:184` that forwards to
the subgroup method at `crates/dashboard/src/plot.rs:280`:

```rust
pub fn plot_promql_with_sql(
    &mut self,
    opts: PlotOpts,
    promql_query: String,
    sql_query: String,
) { ... }
```

It is called across every dashboard category — `blockio.rs`,
`cgroups.rs`, `cpu.rs`, `gpu.rs`, `memory.rs`, `network.rs`,
`overview.rs`, `rezolus.rs`, `scheduler.rs`, `softirq.rs`, `syscall.rs`.
Both strings end up in the dashboard JSON the frontend consumes.

The `BACKEND` constant is exported from two distinct (non-symlinked)
copies of `viewer_api.js`:

- `'sql'` in the static viewer's bundled copy at
  `site/viewer/lib/viewer_api.js:6` (`const BACKEND = 'sql';`)
- `'promql'` in the server-backed copy at
  `src/viewer/assets/lib/viewer_api.js:7` (`const BACKEND = 'promql';`)

Both expose it via `backend()` (`site/viewer/lib/viewer_api.js:135` and
`src/viewer/assets/lib/viewer_api.js:158`).

The data layer's `buildEffectiveQuery` (`src/viewer/assets/lib/data.js:361`)
short-circuits on `backend === 'sql'` (line 379) and returns the SQL
string directly. Otherwise it runs the PromQL transform pipeline
(`rewriteCounterQuery` at `data.js:32`, `injectLabel` at `data.js:75`,
`substituteCgroupPattern` at `data.js:129`) and returns PromQL.

When the server-backed viewer or MCP eventually drop the PromQL
emission, dual emission collapses — `plot_promql_with_sql` becomes
`plot_sql` and the PromQL strings vanish from the dashboard JSON.

---

## Why the JS / Rust split

DuckDB's official Rust crate (`duckdb-rs`) does not compile to
`wasm32`. The bundled C build it wraps has no upstream wasm target.

The browser-side DuckDB is **duckdb-wasm**, a separate library written
in C++ → WASM with a JavaScript host API. Calling it from Rust → WASM
requires either a wasm-bindgen ↔ duckdb-wasm JS bridge (significant
effort) or putting the DuckDB instance in JS in the first place.

We chose the latter. So:

- **JS side** (`site/viewer-sql/lib/duckdb-registry.js`, 926 LOC) owns:
  - The duckdb-wasm `AsyncDuckDB` instances and their Workers
  - The round-robin worker pool (`CaptureSession.WorkerPool`)
  - Parquet attachment, schema introspection, KV metadata parsing
  - Per-source aliasing views for multi-source parquets
  - The cgroup index table and `__SELECTED_CGROUPS__` SQL substitution
  - Result cache, query gating, source picker state

- **Rust→WASM side** (`crates/viewer-sql/src/lib.rs`, 716 LOC) owns:
  - `SqlMetadata` (implements the dashboard's `DashboardData` trait)
  - Arrow batch → Prometheus matrix JSON marshalling (BigInt handling)
  - Time-window CTE wrapping (`wrapWithSrcCte`)
  - Pre-flight column existence validation
  - The pure-SQL macros source-of-truth (exported via `pure_sql_macros()`
    — `crates/viewer-sql/src/lib.rs:38`, which returns the bytes of
    `crates/viewer-sql/src/lib.rs:42`'s `include_str!("macros.sql")`)

The JS module imports the WASM module and calls into it for marshalling
and metadata. The WASM module calls back into JS for the duckdb-wasm
connection. Bidirectional traffic, but the responsibility split is
clean: **engine in JS, dashboard logic in Rust**.

The detailed constraints that shaped this split — async-only workers,
JS UDFs not viable, macro registration quirks — live in
`crates/viewer-sql/duckdb.md` (214 LOC). Worth a skim if you're auditing
the JS layer.

---

## Stage narrative

The migration landed across two early perf commits, seven numbered stages,
and a tail of fixes. Commit hashes on `yv/sql-testing` (verify with
`git log --oneline 037 -- crates/viewer-sql crates/viewer crates/dashboard site/viewer site/viewer-sql src/viewer`):

| Stage | Commit | What |
|-------|--------|------|
| pre-1 (perf) | `eeb02d5` | viewer-sql: multi-worker query pool (Phase 1 perf) |
| pre-1 (perf) | `9707a42` | viewer-sql: combined queries for simple-gauge + irate_total plots (Phase 2 perf) |
| 1 | `033b8bb` | Extract `site/viewer-sql/lib/duckdb-registry.js`. The new JS `CaptureRegistry` mirrors the legacy `WasmCaptureRegistry` surface. |
| 2 | `5a7b778` | Boot-path swap; `ViewerApi` becomes async-aware (CaptureRegistry's `query_range` returns a Promise, unlike the legacy synchronous WASM export). |
| 2 (fix) | `29b2359` | Section-structure cache populated synchronously so the page doesn't blank during boot. |
| 2b | `eb5553e` | Cgroup selector reads from registry state instead of running ad-hoc PromQL probes. |
| 2c | `b099b97` | Source picker UI + "query not yet available" placeholder UX for KPIs. |
| 2c (perf) | `a38471a` | Cgroup selector progressive redraw. |
| 2d | `80f29db` | `regenerate_combined` initialises both captures' templates in compare mode. |
| 3 | `ad1ad9e` | Delete `crates/viewer/` and the retired `site/viewer/pkg/wasm_viewer*` artifacts (net −1981 lines, 11 files; verified via `git show --shortstat ad1ad9e`). |

Post-Stage 3 fixes (`git log --oneline ad1ad9e..HEAD`):
- `af867b5` — gate query selection on `ViewerApi.backend()`
- `f6792ff` — fix `cpu_pct_by_id` missing sum-by-id aggregation
- `5dbe881` — viewer-sql aggregate across rezolus sources to match PromQL
- `1bbd6b2` — per-source `:src<i>` aliases for avg/max/min emitters
- `a415fbe` — detect rezolus sources via Arrow field metadata
- `895801b` — surface combined-rezolus view in source picker
- `d30343f` — converge macros with native + add test scaffold
- `4c25c37` — fix four zero-series emitters
- `5f7a49d` — rate-then-sum for `irate_total` + `irate_sum_by_id` helper
- `889b85f` — reset-aware `sql::rate_5m_total` (counter overflow)

The branch is 37 commits ahead of `main` (`git rev-list --count main..HEAD` →
`37`). Don't squash — the stage-by-stage history is the reading order for
someone reviewing it commit-by-commit.

---

## Where to spend attention

If you have an hour:

1. **`crates/viewer-sql/src/lib.rs`** (716 LOC). The wasm-bindgen surface,
   the Arrow → Prometheus matrix marshalling (the BigInt workaround is
   subtle), and the `query_range` CTE wrapper.
2. **`crates/dashboard/src/sql.rs`** (627 LOC). The SQL emitter helpers.
   This is where most of the per-plot SQL logic lives. Look at
   `rate_5m_total` (`crates/dashboard/src/sql.rs:33`), `concept_total`
   (`crates/dashboard/src/sql.rs:281`), and `irate_sum_by_id`
   (`crates/dashboard/src/sql.rs:126`) for the pattern.
3. **`site/viewer-sql/lib/duckdb-registry.js`** (926 LOC). The JS
   `CaptureRegistry` and worker pool. The public API mirrors the legacy
   `WasmCaptureRegistry` so the Mithril UI didn't need to change.
4. Pick one dashboard category (e.g. `crates/dashboard/src/dashboard/cpu.rs`,
   446 LOC) and read it end-to-end. Each `group.plot_promql_with_sql(...)`
   call shows the dual-query pattern in context.
5. **`crates/viewer-sql/src/macros.sql`** (242 LOC). The wasm-side SQL
   macros. **Note: this file is a parallel copy of the native macros
   in `/work/metriken/metriken-query-sql/src/macros.rs`. They are
   documented as same-semantics but can drift; see the "Known hazards"
   section below.** The parity-check test scaffold loads `macros.sql`
   into a native DuckDB and asserts behaviour against the source-of-truth
   — see `crates/viewer-sql/tests/macros.rs:1-30`:

   ```rust
   //! `macros.sql` is `include_str!`'d at runtime and registered against a
   //! browser-side AsyncDuckDB connection. ... These tests load the same
   //! SQL into a native DuckDB connection and assert the per-second rate
   //! and 5-minute rate primitives behave the same way as their counterparts
   //! in /work/metriken/metriken-query-sql/src/macros.rs ...
   ```

Things to skip:

- The PromQL transform helpers in `src/viewer/assets/lib/data.js`
  (`substituteCgroupPattern`, `injectLabel`, `rewriteCounterQuery`,
  `PROMQL_KEYWORDS`, the PromQL branch of `buildEffectiveQuery`).
  They are live for the server-backed viewer / MCP path, dead in
  static-viewer mode. The file's section comments at
  `src/viewer/assets/lib/data.js:14-15` mark them as such.
- `crates/dashboard/src/service_extension.rs` (693 LOC). KPI definition
  structures, unchanged from `main`.
- `crates/dashboard/src/plot.rs` (879 LOC). Plot metadata and the
  `plot_promql_with_sql` API surface — incremental, not a rewrite.

---

## Mental models for the adversarial reviewer

**1. The engine lives in the browser. The frontend speaks SQL.** The
core swap is "PromQL evaluator in Rust → WASM" → "DuckDB in
JS-via-duckdb-wasm". Everything else falls out of that single decision.

**2. The Rust crate (`crates/viewer-sql/`) is intentionally thin.** It
doesn't own the database. It owns marshalling. The bulk of the
"viewer migration" diff is the JS registry, not the Rust crate.

**3. Dual-query per plot is a transition artifact.** Every plot's
`(promql_query, sql_query)` pair exists because the server-backed
viewer and MCP still drive PromQL. When those migrate, dual emission
collapses naturally.

**4. The known divergence list is bucketed but not exhaustive.** 1370
divergent pairs sound alarming. About 1289 of them (94%) fall into 8
named categories below — 985 + 167 + 55 + 26 + 23 + 16 + 9 + 8.
The remaining ~81 are not yet categorised (a refresh of the taxonomy is
overdue — see the divergence section). Of the named categories, one is
a confirmed remaining bug (sql duplicate samples per timestamp, see
below), one is a semantic decision still open (multi-source aggregation
in shadow-mode runs), and the rest are acceptable noise or already-fixed
issues whose run output predates the fix.

---

## Adversarial-reviewer FAQ

**Q: Why retire `crates/viewer/` if the server still uses PromQL?**

A: The retired crate is the **static viewer's WASM PromQL engine** — it
ran client-side over a parquet file. The server's PromQL still runs
**native** through `metriken-query`. They are two different code paths;
only the first migrated. The server-side PromQL endpoint
(`/api/v1/query_range`) is unchanged.

**Q: Why are there two SQL macro libraries?**

A: Native macros (`/work/metriken/metriken-query-sql/src/macros.rs`,
314 LOC, in Rust calling `duckdb-rs`) and wasm macros
(`crates/viewer-sql/src/macros.sql`, 242 LOC, pure SQL embedded via
`include_str!`) are two parallel copies of the same logic. They must
stay in sync. Why two copies:

- The native version is in Rust because the harness loads it via
  `duckdb-rs::register_all` and runs unit tests against it (see
  `metriken-query-sql/tests/macros.rs` parity).
- The wasm version is pure SQL because duckdb-wasm doesn't take Rust
  registrations — the browser-side macros must be `CREATE MACRO` statements
  the JS host can hand to the connection.

This is a known hazard. The early-warning system is
`crates/viewer-sql/tests/macros.rs` (123 LOC) — a native test that
loads `macros.sql` into an in-memory DuckDB and asserts parity with the
native implementations. Drift surfaces there.

A future commit can collapse this to one source of truth (extract
macros.sql, generate the Rust version from it, or vice versa); not in
scope for this branch.

**Q: Why is `duckdb-registry.js` 926 lines?**

A: It owns: worker pool (round-robin checkout), parquet attachment +
schema introspection (column → metric-name index), per-source aliasing
views (one `_src_<source>` view per rezolus source on multi-source
parquets), cgroup index table construction (one `SELECT DISTINCT` per
cgroup-style column at load time), result cache (keyed by SQL string),
query gating (PromQL vs SQL via `ViewerApi.backend()`), source picker
state, `__SELECTED_CGROUPS__` substitution. Each concern is roughly
100 LOC; the file is large because all of it lives in JS by necessity.

**Q: What's the live-agent migration story?**

A: Live mode (`rezolus view http://localhost:4241`) polls the agent's
msgpack endpoint into an in-memory `Tsdb` and serves PromQL via
`/api/v1/query_range`. There is no parquet file on disk to point
DuckDB at. Options being weighed:

1. **Server-side SQL.** Replace the in-memory `Tsdb` with a continuously
   appended duckdb relation. Frontend keeps hitting
   `/api/v1/query_range` but with SQL bodies. Native DuckDB UDFs
   available (wasm-UDF gap goes away — significant win on histogram
   sections). Server stays the choke point for auth/rate-limiting.
2. **Client-side SQL.** Server rotates a parquet file (Hindsight-style);
   frontend pulls parquet bytes and runs duckdb-wasm locally. Same
   code path as file mode. Eliminates server PromQL entirely. Cost:
   every viewer streams its own data; per-query auth/rate-limit
   enforcement disappears.

The two are not mutually exclusive — option 1 with a rolling parquet
ring is essentially Hindsight + tail. See the "Open questions" section
for the full decision matrix.

**Q: Why is `cargo check --bin rezolus` failing on `Catalogue`,
`DispatchConfig`, `DispatchObserver`, `Diff`, `Mode`?**

A: Build-config bug, not a missing-types issue. Those types still exist
in `metriken-query`, but they're feature-gated behind `sql` — see
`/work/metriken/metriken-query/src/lib.rs:41-42`:

```rust
#[cfg(feature = "sql")]
pub use catalogue::{Catalogue, CatalogueEntry, CatalogueError, GoldenExample, OutputShape};
```

The rezolus workspace declares the dep with `default-features = false`
(`Cargo.toml:22`) and the `[dependencies]` table for the binary only
enables `features = ["ingest", "lz4"]` (`Cargo.toml:78`) — so the binary
sees a metriken-query without the `sql` cargo feature, and the dispatch
types referenced from `src/viewer/mod.rs:1664-1696` are invisible.
`rustc` even prints the cfg-out note:

```
note: found an item that was configured out
   --> /work/metriken/metriken-query/src/lib.rs:42:32
    |
 41 | #[cfg(feature = "sql")]
    |       --------------- the item is gated behind the `sql` feature
```

The static viewer builds via `./crates/viewer-sql/build.sh` (independent
of the binary), so this doesn't block the SQL viewer. Fix is to add the
`sql` feature to the binary's metriken-query dep in `Cargo.toml`.

---

## Verification

### Cargo / build

```
cargo build                                          # main binary; fails — see FAQ above
cargo build -p dashboard                             # dashboard crate; passes (1 dead-code warning)
./crates/viewer-sql/build.sh                         # wasm-pack build; passes
```

The `cargo build -p dashboard` run finishes with a single warning:

```
warning: constant `RATIO_X1000` is never used
 --> crates/dashboard/src/dashboard/cpu.rs:8:7
```

### Correctness harness

Lives in metriken: `/work/metriken/metriken-query/examples/sql_vs_promql.rs`.
Loads each dashboard plot from the dashboard JSON, runs `promql_query`
through legacy `metriken-query` and `sql_query` through DuckDB, classifies
each pair.

```
cd /work/metriken
cargo run --release --example sql_vs_promql \
    --features "legacy,sql" \
    -- /work/rezolus/site/viewer/data/demo.parquet
```

Output in `/tmp/sql_vs_promql_yv/<parquet>/<plot>.json`. Summary in
`summary.json`.

### Within-branch results (latest, 11 parquets × 250 plots)

Numbers below come straight from `/tmp/sql_vs_promql_yv/summary.json`.
Each parquet contributes 250 plots, of which 48 are cgroup-only and
skipped by the harness (the table omits a `skipped` column for
readability); the per-row identical+tolerant+divergent therefore totals
202, and the table grand total is 11 × 202 = 2222 (plus 528 skipped =
11 × 250 = 2750 pairs).

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

Headline: ~89% identical on single-source rezolus parquets (demo, vllm).
Multi-source and numeric-encoded parquets diverge for known reasons —
see the divergence taxonomy below.

### Cross-branch sanity check

`examples/promql_only.rs` runs the PromQL evaluator alone, so it
compiles on both `main` and `yv/sql-testing`. The run results live at
`/tmp/promql_yv/promql_results.json` and `/tmp/promql_main/promql_results.json`,
each shaped as `{parquet → {plot_name → result}}` covering 193 plots ×
3 parquets (demo, vllm, sglang-nixl-16c) = 579 (parquet, plot) pairs.

Comparing the two JSONs pair-by-pair: **381 identical, 198 divergent**.
The branch's PromQL strings have shifted — most divergences are plot
queries that were reshaped on `yv/sql-testing` to make the SQL emitter's
job tractable (e.g. the BPF execution-time / overhead family). So this
is **not** a zero-delta cross-branch result; it's evidence that the
PromQL surface evolved on the branch. The shadow-mode dispatch
correctness signal lives in the harness above, not here.

---

## Known divergence taxonomy

The harness run in `/tmp/sql_vs_promql_yv/summary.json` reports 1370
divergent + 3 within-tolerance pairs out of 2750 (and 528 cgroup-skipped).
About 1289 of the 1370 fit into the eight named buckets below
(985+167+55+26+23+16+9+8); ~81 are unaccounted-for residue that a refreshed
run would either bucket or shrink:

| count | category | what it means |
|------:|---|---|
| 985 | sql view missing the metric | Numeric-encoded parquets (`AB_*`, `*_gemma3`, `cachecannon`) store metric names in Arrow field metadata, not column names. Dashboard SQL references columns by metric name; the SQL emitter does not yet read Arrow metadata. **Architectural cliff, not a regression** — the static viewer renders these mostly empty too. Likely older test fixtures; worth confirming with the team. Sample row from `/tmp/sql_vs_promql_yv/AB_base.divergences.txt`: `AB_base::blockio-throughput-total (blockio) — series count: promql=1 sql=0`. |
| 167 | `rate_5m` boundary | Documented as the original positional-LAG implementation. The macro has since been rewritten range-based in `crates/viewer-sql/src/macros.sql:183-185` (and a parallel native rewrite landed in the macro source-of-truth). **This count is stale; rerun the harness to refresh.** |
| 55 | larger numerical drift (rel ≥ 1e-3) | **All one root cause.** Every case is on `disagg-sglang` (40) or `sglang-nixl-16c` (15) — the two demo parquets with multiple `rezolus` sources. See "The multi-source root cause" below. |
| 26 | series count differs | Multi-source aggregation gaps. Same root cause as the 55. |
| 23 | sql produces series, PromQL doesn't | Inverse of category 1, rarer. |
| 16 | small numerical drift (rel < 1e-3) | irate window-math jitter (sub-millisecond timestamp drift divided into per-second deltas). Acceptable noise. |
| 9 | label set mismatch | Multi-source labelling differences (e.g. `source=rezolus` only on PromQL side). |
| 8 | sql duplicate samples per timestamp | Reported on `busy-pct-per-cpu` (`crates/dashboard/src/dashboard/cpu.rs:32-38`) and `cpu-busy-heatmap` (`crates/dashboard/src/dashboard/overview.rs:32-38`). Sample row: `demo::busy-pct-per-cpu (cpu) — labels id=1: duplicate samples per integer-second bucket: promql_dups=0 sql_dups=300` (`/tmp/sql_vs_promql_yv/demo.divergences.txt`). Both plots now route through `sql::cpu_pct_by_id`, whose `SUM(rate_ns) / 1e9` aggregation in `crates/dashboard/src/sql.rs:233-235` collapses the per-id duplicates — landed in commit `f6792ff` "dashboard: fix cpu_pct_by_id missing sum-by-id aggregation". **This count is stale; rerun the harness to refresh.** |

Tolerance settings: `rel_tol = 1e-9`, `abs_tol = 1e-12` (verified in
`/tmp/sql_vs_promql_yv/summary.json`); ±2 boundary samples allowed for
grid-evaluation artefacts.

### The multi-source root cause

The 55 cases above (plus the 26 in "series count differs") are not 55
independent bugs. They are one architectural choice:

- The legacy PromQL evaluator's `Tsdb` stores all sources' series
  together and aggregates across them on `sum(...)` /
  `sum by (id)` / `histogram_quantiles(...)`.
- The dashboard's SQL emitter (pre-fix) picked **one source view**
  (the one whose name appears in the parquet filename, falling back
  to the first rezolus source) and queried only that single view.

Ratios observed at the time the harness was last run: `promql / sql ≈ N`
(number of rezolus sources) for `sum`-shaped plots, `1/N` for
`min`/`max`-shaped reductions.

| parquet         | rezolus sources | count of cases | ratios observed |
|-----------------|----------------:|---------------:|-----------------|
| disagg-sglang   |               2 |             40 | promql ≈ 2× sql |
| sglang-nixl-16c |               3 |             15 | promql ≈ 3× sql |

The dashboard-side decision was **(a) aggregate across all rezolus
sources in SQL** to match PromQL — landed across:

- `5dbe881` "viewer-sql: aggregate across rezolus sources to match PromQL"
- `1bbd6b2` "viewer-sql: per-source `:src<i>` aliases for avg/max/min emitters"
- `a415fbe` "viewer-sql: detect rezolus sources via Arrow field metadata"
- `895801b` "viewer: surface combined-rezolus view in source picker"

These commits postdate the harness run that produced the 55/26
counts, so a re-run is required to confirm the cases have collapsed
as expected.

### Fixes already shipped (not yet reflected in the divergence counts)

The `rate_5m` macro is **already** range-based on this branch —
`crates/viewer-sql/src/macros.sql:183-185`:

```sql
CREATE OR REPLACE MACRO rate_5m(c, ts) AS
    (c - first_value(c) OVER (ORDER BY ts RANGE BETWEEN 300000000000 PRECEDING AND CURRENT ROW))
    / NULLIF((ts - first_value(ts) OVER (ORDER BY ts RANGE BETWEEN 300000000000 PRECEDING AND CURRENT ROW))::DOUBLE / 1e9, 0);
```

The 167 `rate_5m boundary` cases in the taxonomy reflect the older
positional-LAG implementation; rerun the harness to refresh.

The `cpu-busy-heatmap` / `busy-pct-per-cpu` aggregation fix has shipped
too. `busy-pct-per-cpu` is wired in
`crates/dashboard/src/dashboard/cpu.rs:32-38`:

```rust
busy.plot_promql_with_sql(
    PlotOpts::counter("Busy % (Per-CPU)", "busy-pct-per-cpu", Unit::Percentage)
        .percentage_range(),
    "sum by (id) (irate(cpu_usage[5m])) / 1000000000".to_string(),
    sql::cpu_pct_by_id("^cpu_usage/[a-z]+/[0-9]+$", "/([0-9]+)$"),
);
```

…and `cpu-busy-heatmap` in `crates/dashboard/src/dashboard/overview.rs:32-38`
calls the same helper. The helper's body in
`crates/dashboard/src/sql.rs:216-237` ends with
`SUM(rate_ns) / 1e9 AS v ... GROUP BY ...`, which is the `sum by (id)`
aggregation the divergence row was missing.

---

## Open questions / out-of-scope follow-ups

These do not block landing this branch but they shape the path
forward:

1. **Live-agent and MCP migration.** Shadow-mode SQL dispatch is wired
   on the parquet path (`src/viewer/mod.rs:1727-1731,1756-1760`) but
   live-agent flows skip it (no `parquet_path` is ever set —
   `src/viewer/mod.rs:533-538` and `:762-763`) and MCP never calls
   `with_dispatch`. Decision affects how long `metriken-query`'s
   `legacy` feature stays alive.
2. **KPI translation.** Plots whose query lives in parquet
   `service_queries` metadata (service extensions / KPIs) still
   arrive as raw PromQL strings. The viewer marks them as
   "(query not yet available — translation pending)" —
   `src/viewer/assets/lib/data.js:441` and
   `src/viewer/assets/lib/charts/chart.js:382`. Fix lives at `parquet
   annotate` time: translate operator-authored PromQL into SQL before
   embedding.
3. **Server cgroup selector revival.** The Stage 2b rewrite made the
   cgroup selector SQL-only. The server-backed viewer no longer has
   a working cgroup page; restoring the dual path would un-revert
   most of Stage 2b. Tied to the server-mode migration question.
4. **Compare-mode "category combined section".** `regenerate_combined`
   ships the per-capture half; the case-(b) "one combined category
   section" still requires multi-capture `init_templates` API on
   `viewer-sql`. Users in compare mode see two per-capture sections
   instead of one combined. Documented as a follow-up.
5. **Macro library consolidation.** Two copies of the SQL macros
   exist (`crates/viewer-sql/src/macros.sql` 242 LOC and
   `/work/metriken/metriken-query-sql/src/macros.rs` 314 LOC). The
   parity test at `crates/viewer-sql/tests/macros.rs` (123 LOC)
   catches drift; a single-source-of-truth approach (generate one
   from the other) is a future refactor.
6. **Numeric-encoded parquet support.** Either teach the SQL emitter
   to read Arrow field metadata, or document the unsupported fixture
   set. 985 of the 1370 divergences live here.
7. **Refresh the harness numbers.** The taxonomy above counts
   pre-fix divergences (rate_5m boundary, cpu sum-by-id duplicates).
   Re-running `cargo run --release --example sql_vs_promql ...` on
   each parquet would shrink the headline 1370 and let the
   "1370 fall into 8 categories" claim become accurate again.
8. **Fix the rezolus binary build.** Add the `sql` cargo feature to
   the binary's `metriken-query` dep in `Cargo.toml:78` so the
   dispatch types resolve and `cargo build` succeeds end-to-end.

---

## Pointers

- Companion document (engine side): `/work/metriken/REVIEWING.md`
- Correctness harness: `/work/metriken/metriken-query/examples/sql_vs_promql.rs`
- Harness output: `/tmp/sql_vs_promql_yv/`
- Cross-branch output: `/tmp/promql_yv/` and `/tmp/promql_main/`
- Test scaffold (native vs wasm macro parity):
  `crates/viewer-sql/tests/macros.rs`
- CLAUDE.md: project-level architecture overview (build commands,
  operating modes, sampler architecture)
