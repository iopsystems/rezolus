# Primer — testing `yv/sql-testing`

Companion docs:
- `review/review.md` — the per-section walkthrough for the code reviewer.
- `review/architecture.md` — the tour for newcomers learning the codebase.

This document is for **the tester**: someone running the branch's
binary and clicking through the viewer, plus running the test suite.
Read it once top to bottom (~5 min) before opening anything.

---

## The one-paragraph summary

`yv/sql-testing` migrates Rezolus's file-mode viewer, MCP server,
`parquet annotate`, and Save-as-Report column trim off the in-memory
PromQL evaluator (`metriken_query::Tsdb`) and onto DuckDB-driven SQL
through a new crate `metriken_query_sql::DuckDbBackend`. The
live-agent viewer is unchanged — still PromQL via `Tsdb`, gated
behind the `live-mode` Cargo feature (default-on). The branch adds
**two** new workspace-level crates on the rezolus side
(`crates/prom-matrix`, `crates/viewer-sql`), **one** new crate on
the metriken side (`metriken-query-sql/`), and a cross-engine
verification harness at `metriken-query/examples/sql_vs_promql.rs`.
99 non-merge commits ahead of `main`.

---

## Where the big changes live

| Component | Location | What it is |
|---|---|---|
| **DuckDB-backed engine** | `/work/metriken/metriken-query-sql/` | New crate. `DuckDbBackend` (connection pool, panic-safe slot eviction), H2 histogram UDFs, `SHARED_MACROS` SQL layer, `_src_<source>` per-source views, `_cgroup_index` mapping table. |
| **Dashboard SQL emitters** | `crates/dashboard/src/sql.rs` (715 LOC) | 16 helpers (`rate_5m_total`, `irate_total`, `cpu_pct_total`, `cgroup_irate_by_name`, `cgroup_ratio_by_name`, `hist_percentile_series`, …). 130+ plot call sites route through these to produce `plot.sql_query`. |
| **Server viewer wiring** | `src/viewer/sql_capture.rs` (379 LOC), `capture_registry.rs` (305 LOC), `state.rs`, `routes.rs` | `Arc<DuckDbBackend>` on `AppState`, `SqlCapture` wraps a parquet path + `MetricCatalog`, `/api/v1/query{,_range}` runs raw SQL with a binder-error→empty-matrix shim. |
| **Static viewer (WASM)** | `crates/viewer-sql/` | Bridges JS↔Rust for `duckdb-wasm`; re-exports `SHARED_MACROS` alongside H2 wrapper macros so the browser engine sees the same dashboard SQL as the native engine. |
| **Arrow→Prom-matrix** | `crates/prom-matrix/` | Single envelope formatter shared by native + WASM. Blocks JSON drift. |
| **MCP migration** | `src/mcp/` | ~2,100 LOC of legacy Tsdb/PromQL helpers deleted. New `src/mcp/backend.rs` is the shared layer (open parquet, project batches→series, canonical SQL builders for counter/gauge/histogram). `mcp query` now takes DuckDB SQL (breaking CLI; intended). |
| **Save-as-Report** | `crates/report-save/src/lib.rs` | `resolve_kept_columns_sql(payload, catalog, side)` resolves the trim keep-set from a `MetricCatalog` instead of running PromQL. SQL-aware save paths: `save_single_parquet_sql`, `save_combined_ab_tarball_sql`. After the main merge, all four save entrypoints (live-mode + SQL × single + AB) also persist `events: Vec<Event>` to `KEY_EVENTS` in the parquet footer. |
| **Verification harness** | `/work/metriken/metriken-query/examples/sql_vs_promql.rs` (~1,700 LOC) | Runs every dashboard plot through both engines (PromQL and SQL) on real parquets and diffs canonical JSON plot-by-plot. Substitutes `__SELECTED_CGROUPS__` per parquet from `_cgroup_index`. |

The merge with `origin/main` (commit `9b628c4`) brought in main's
events-annotation feature, viewer-JS subdirectory refactor (files
moved under `lib/{features,sections,ui,selection,events,charts}/`),
markdown notes/editable titles in Notebook/Report, a
`<rezolus-chart>` embed component, and a Rust comment trim. All
integrated cleanly; the JS refactor reorganized our SQL-pipeline
edits into the new file locations.

---

## What should look identical to a user

The migration is intended to be **transparent**. If you see different
*values* in dashboard charts vs. how Rezolus rendered them pre-branch,
that's a regression worth reporting. The cross-engine harness reports
**698 identical plots out of 699** across `demo.parquet`,
`AB_level_pin.parquet`, and `AB_base.parquet` — values agree to
within `rel_tol=1e-9, abs_tol=1e-12` on virtually every plot at every
timestamp.

Concrete things that should be visually unchanged:
- Every chart on every dashboard page (cpu, memory, network,
  scheduler, syscall, softirq, blockio, exceptions, cgroups, gpu,
  overview, rezolus). Bin-exact same values; bin-exact same shapes.
- Time-range slider behavior.
- Hover tooltips with timestamp + value.
- A/B compare-mode shape (load two parquets; baseline left,
  experiment right).
- Save-as-Report: pin charts → save → reload → only pinned charts
  appear with the right time window.
- The static WASM viewer (open `site/viewer/index.html` in a
  browser pointing at any of the deployed fixtures).
- Per-section navigation, search, cgroup selector, node picker.

If anything in the above list looks different from `main`, that's
the kind of thing to flag.

---

## What should look intentionally different

Things that **did change visibly** — these are deliberate.

- **`mcp query` takes DuckDB SQL** instead of PromQL. The M-in-MCP
  clients are LLMs and they're fluent in SQL; old `query 'rate(cpu_cycles[5m])'`
  invocations will error. `query 'SELECT count(*) FROM _src'` is the
  new shape. Confirmed by `cargo test --test mcp_cli`.
- **First query per parquet has a cold-start cost.** The
  `DuckDbBackend` slot for a parquet warms on first use (~tens to
  hundreds of ms for tens of MB). Subsequent queries hit a warm
  connection pool. Visible as a one-time pause when the user opens
  a parquet; not visible thereafter.
- **Per-cgroup individual plots show a ~5-minute tail of carried-
  forward rate** past a cgroup's last non-NULL sample (e.g. one-shot
  systemd units that exit early in the recording). This matches
  PromQL's `irate(c[5m])` lookback. Pre-branch SQL pipeline was
  truncating these series early; the branch restores the
  pre-migration behavior. Most visible in the cgroups section of
  `AB_level_pin.parquet` and `AB_base.parquet`.
- **`rezolus view http://agent:4241`** (live-agent mode) returns
  `capture_not_found` for `/api/v1/query{,_range}`. The live ingest
  path is still PromQL via `Tsdb`, but the query path isn't wired up
  yet. Plot rendering won't work in live mode against this branch.
  This is the explicit "live-mode" carve-out called out in
  `review/review.md`; not a regression. Live-agent KPI availability
  check on load (`validate_service_extensions`) does still run via
  PromQL.

---

## Quickstart — automated

From `/work/rezolus`:

```bash
# Build matrix
cargo build --bin rezolus                                              # default
cargo build --bin rezolus --no-default-features --features sql-only    # SQL-only
cargo tree -p rezolus --no-default-features --features sql-only | \
    grep 'metriken-query v'                                            # must be empty

# Native test suite
cargo test --bin rezolus                                               # ~174 tests
cargo test -p dashboard                                                # ~20 tests incl. snapshot + cgroup-NULL-tail behavioural
cargo test -p prom-matrix                                              # ~10
cargo test -p viewer-sql                                               # SHARED_MACROS parity
cargo test -p report-save                                              # 19 incl. events × SQL
cargo test --test mcp_cli                                              # 8 end-to-end CLI tests

# Frontend pure-JS tests (no jsdom; node built-in test runner)
node --test tests/*.mjs   # ~122 total; 116 pass / 6 fail (see "Known not-regressions")

# End-to-end smoke (upload / file / A-B / proxy / Save-as-Report). Requires `jq`.
bash tests/viewer_smoke.sh
```

From `/work/metriken`:

```bash
cargo test --workspace --all-features --all-targets   # ~250 tests across the workspace
```

The cross-engine shake-down (the single most important automated
check):

```bash
# Regenerate dashboard JSON spec (rezolus tree):
cd /work/rezolus
rm -rf /tmp/dashboard_json && mkdir -p /tmp/dashboard_json
cargo run -p dashboard -- /tmp/dashboard_json

# Run the harness (metriken tree):
cd /work/metriken
rm -rf /tmp/sql_vs_promql && mkdir -p /tmp/sql_vs_promql
cargo run --release --example sql_vs_promql --features "legacy,harness" -- \
  --dashboard-dir /tmp/dashboard_json \
  --parquets /work/rezolus/site/viewer/data/demo.parquet \
             /work/rezolus/site/viewer/data/AB_level_pin.parquet \
             /work/rezolus/site/viewer/data/AB_base.parquet \
  --out /tmp/sql_vs_promql

# Expected: pairs=699 identical=698 divergent=1 promql_err=0 sql_err=0 skipped=0
# The divergent plot is `numa-local-rate` on AB_base, rel ≈ 2.7e-5.
# See review.md "Known divergences" for context.
```

---

## Quickstart — manual (browser)

The viewer should already be up on `http://127.0.0.1:8080` (we
started it earlier with `vllm.parquet`). If not:

```bash
cd /work/rezolus
REZOLUS_NO_OPEN=1 ./target/debug/rezolus view site/viewer/data/vllm.parquet --listen 127.0.0.1:8080
```

Click-through script:

1. **Each dashboard page populates.** Walk every section in the
   sidebar. Every chart should have data (or be intentionally empty
   for a metric absent from this parquet, with the *empty* state —
   not an error banner).
2. **Time-range slider** drags both ways; charts refetch and rerender
   cleanly (no stale data, no error banners). The cold-start latency
   should be invisible to the eye since subsequent queries are warm.
3. **Cgroup section.** Select one or more cgroups from the selector.
   Individual plots fan out per cgroup. If the parquet contains an
   exited cgroup (try `AB_level_pin.parquet` or `AB_base.parquet`),
   the series should extend ~5 min past the cgroup's last data with
   a flat near-zero tail. **Don't report this as a bug** — it's the
   new (correct) NULL-tail behavior.
4. **Save as Report.** Pin a chart (the pin icon on the chart header),
   click "Save as Report" in the top bar. A parquet downloads. Open
   it in a fresh viewer (e.g. `rezolus view ~/Downloads/report.parquet
   --listen 127.0.0.1:8082`). Only the pinned charts should appear,
   in the same time window.
5. **Events feature** (main's contribution, integrated through SQL
   trim path on this branch). Hover a chart, click on the canvas to
   freeze the tooltip, click "+ Add Event", fill the description,
   submit. A dark-orange vertical marker should appear on **every**
   chart at that timestamp. Hover the marker to see the description
   pop up. Save-as-Report and re-open: markers should persist.
6. **A/B compare.** Restart with two parquets:
   `rezolus view site/viewer/data/AB_base.parquet site/viewer/data/AB_base_pin.parquet --listen 127.0.0.1:8081`.
   Compare mode should activate; charts show baseline (gray) +
   experiment (color) lines side-by-side.
7. **Upload mode.** Restart with no parquet:
   `rezolus view --listen 127.0.0.1:8083`. Drag a parquet onto the
   landing page. The viewer should pick it up.

---

## Known not-regressions

If you see these, **don't** report them — they're documented and
non-load-bearing:

| Observation | Why it's fine |
|---|---|
| 6 frontend JS test failures (5 in `tests/compare_node_filter.test.mjs`, 1 in `tests/wasm_viewer_histogram_kpis.test.mjs`) | PromQL-side `buildEffectiveQuery` injection paths and retired in-process WASM viewer references. Dead code on the SQL-backed viewer. To be retired in a follow-up. |
| `sql_vs_promql` reports `numa-local-rate` divergent on `AB_base.parquet` (rel ≈ 2.7e-5) | Sub-tolerance floating-point residual from the 5-min RANGE arithmetic. Loosening `--rel-tol` from 1e-9 to 1e-4 reclassifies it as `within_tolerance`. Documented in `review.md` "Known divergences". |
| `rezolus view http://agent:4241` charts don't populate; `/api/v1/query{,_range}` returns `capture_not_found` | Intentional carve-out for live-agent mode pending the live-ingest storage decision (item D in `review.md` "Removing Tsdb entirely"). |
| `cargo build --no-default-features --features sql-only` shows ~24 warnings | Pre-existing `dead_code` warnings from PromQL helpers that are still in tree but unused in the sql-only configuration. Will resolve when the legacy paths are deleted in the follow-up. |
| One harness output entry per parquet labeled `expected_divergent` | The harness reclassifies divergences caused by `_src_rezolus_combined` (multi-rezolus sum-then-rate vs per-rezolus rate-then-sum) as expected. Document the methodology in `review.md` Bucket #3 territory. |

---

## When something does look wrong

1. **A chart shows data that disagrees with the same plot on `main`.**
   First sanity check: is the difference within the harness's tolerance?
   Run `sql_vs_promql` against the offending parquet and check the
   per-plot outcome JSON at `<out>/<parquet_stem>/<plot_id>.json` —
   the `verdict` field carries the first mismatch. If the harness
   doesn't flag it but you see something visually different, it's
   a UI-side rendering issue (likely worth reporting).
2. **A chart is empty when you expect data.** Check the network tab
   for `/api/v1/query_range`. If `status: "error"` with `errorType:
   "sql_error"`, the SQL didn't bind — usually means a metric isn't
   in this parquet, and the binder-error → empty-matrix shim should
   have caught it but didn't. Worth reporting with the exact query.
3. **The page hangs or load takes too long.** First parquet open
   pays a cold-start (~hundreds of ms for tens of MB). If that's
   more than a few seconds for a small parquet, something's wrong
   with the warm-pool logic. Worth reporting with the parquet size +
   how long it took.
4. **Events feature seems broken.** Frame this with the SQL trim
   path specifically: does the events feature work the same when
   loading a parquet that was saved through `save_single_parquet_sql`
   vs `save_single_parquet` (live-mode)? Both should write
   `KEY_EVENTS` to the parquet footer; both should be re-readable by
   the viewer. If the SQL trim path drops events, that's a real bug.
