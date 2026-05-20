# Primer — testing `yv/sql-testing`

For someone running the binary and the test suite. The code reviewer
reads `review.md`; the newcomer reads `architecture.md`.

- **What the branch does.** Migrates every viewer path (file / upload /
  A-B / live-agent), MCP, `parquet annotate`, Save-as-Report column
  trim, and `validate_service_extensions` off in-memory PromQL
  (`metriken_query::Tsdb`) and onto DuckDB-backed SQL through
  `metriken_query_sql::DuckDbBackend`. Live mode now appends
  snapshots to a `LiveSource` registered with the same backend
  (`live:baseline` key); `/api/v1/query{,_range}` dispatches on a
  data-source string but the SQL code path is identical for both
  ingest paths. The `metriken-query` crate is deleted from the
  workspace; the `live-mode` / `sql-only` feature seam is gone;
  single build: `cargo build --bin rezolus`.

- **Where the change lives** (concept order — engine first, callers after).
  - **The engine.** `/work/metriken/metriken-query-sql/` is a new
    crate. `backend.rs` owns the per-source connection pool with
    panic-safe slot eviction and a `try_lock` scan that picks the
    first idle slot before falling back to round-robin; `live.rs`
    (~800 LOC) owns `LiveSource`, a single-`Connection` in-memory
    DuckDB table that grows via `ALTER TABLE _src ADD COLUMN` +
    `INSERT` as new metrics appear (the parquet pool model can't
    apply — each slot is an independent in-memory DB, wrong for a
    shared mutable table). `canonical_column_name` is a public free
    fn that keeps live `_src` column names byte-identical to the
    parquet path. `udf.rs` the H2 histogram UDFs; `shared_macros.sql`
    the 20-macro SQL layer (`cpu_busy_pct`, `irate_1s`,
    `h2_combine_lol`, …); `views.rs` materialises `_src_<source>`
    per-source views and the `_cgroup_index` JOIN table. The same
    `shared_macros.sql` is `include_str!`d by both the native engine
    and the WASM viewer so emitters can't drift.
  - **The emitters.** `crates/dashboard/src/sql.rs` (852 LOC, 21
    `pub fn` helpers including `rate_5m_total`, `irate_total`,
    `hist_percentile_series`, `cpu_pct_total`, `cgroup_irate_total`,
    `cgroup_irate_by_name`, `cgroup_ratio_by_name`,
    `bucket_heatmap_sql`, `quantile_spectrum_sql`,
    `percentile_kpi_sql`, `multi_percentile_kpi_sql`) is what ~170
    plot call sites in `dashboard/*.rs` call to produce each
    plot's `sql_query`. `crates/dashboard/tests/sql_snapshots.rs`
    pins 25 helper outputs as snapshots; `tests/cgroup_null_gap.rs`
    runs cgroup `irate_by_name` / `ratio_by_name` SQL against a
    fabricated `_src` and asserts on row counts after a NULL
    transition.
  - **The viewer.** `Arc<DuckDbBackend>` lives on `AppState`
    (`src/viewer/state.rs`). Parquets are wrapped by `SqlCapture`
    (`sql_capture.rs`, 384 LOC) and held by `CaptureRegistry`
    (`capture_registry.rs`, 265 LOC). Live captures wrap a
    `LiveSource` in a `LiveCapture` (`live_capture.rs`, 177 LOC; the
    `DashboardData` shim for the live slot); the shared
    `Arc<LiveSource>` is registered with the backend under
    `LIVE_BASELINE_DATA_SOURCE = "live:baseline"`.
    `data_source_for(state, capture)` in `routes.rs:523` resolves
    the live key ahead of any parquet path, so
    `/api/v1/query{,_range}` dispatch is uniform across modes.
    `src/viewer/live_ingest.rs` (~358 LOC) is the
    `metriken_exposition::Snapshot` → `LiveSource::append` bridge.
    The SQL handler runs through `spawn_blocking` so 20+ parallel
    chart fetches don't starve the tokio runtime. A binder-error →
    empty-matrix shim in `run_sql` preserves the pre-migration
    "unknown metric → empty series" UX.
  - **Parallel migrations.** MCP (`src/mcp/backend.rs` is the
    shared helper; ~2,100 LOC of legacy Tsdb/PromQL deleted),
    Save-as-Report (`crates/report-save/src/lib.rs`:
    `resolve_kept_columns_sql` + `save_*_sql` paths; merge with
    main also threaded `events: Vec<Event>` → `KEY_EVENTS` through
    all four save entrypoints), `parquet annotate`
    (`src/parquet_tools/annotate.rs`: validates each KPI's SQL
    binds against the parquet), live-agent ingest
    (`src/viewer/live_ingest.rs` + `metriken_query_sql::LiveSource`,
    commits `17f1107` / `1d471cd` / `494b4fc` / `f5482ff` — `_src`
    is byte-shape-identical to parquet so the same dashboard SQL
    binds in both modes).
  - **Verification (post-Tsdb-deletion).** No more PromQL evaluator
    to diff against. Correctness rests on dashboard snapshot tests
    (`crates/dashboard/tests/sql_snapshots.rs`), parquet ↔ live
    tests in `metriken-query-sql/src/live.rs`.

- **Historical verification (pre-deletion).** The `sql_vs_promql`
  harness against `demo.parquet`, `AB_level_pin.parquet`,
  `AB_base.parquet` reported **698 identical / 1 divergent / 0
  errors / 0 skipped**. The single divergence was `numa-local-rate`
  on `AB_base.parquet`, `rel ≈ 2.7e-5` — floating-point residual
  from the 5-min RANGE arithmetic; sub-tolerance. The harness and
  PromQL evaluator are gone post-deletion; the L2 parquet↔live
  parity tests in `metriken-query-sql/src/live.rs::tests` are the
  current regression catch. Three behavioural fixes landed in the
  SQL pipeline to align with PromQL semantics:
  - `rate_5m_total`: `RANGE 5m PRECEDING` → `5m − 1 ns` to exclude
    the boundary sample at `t − 5m` (PromQL's strict-greater
    semantics on parquets with sub-second start offsets).
  - `cgroup_irate_total` / `cgroup_irate_by_name`: per-column irate
    (PARTITION BY col) before SUM, matching `sum(irate(...))`
    instead of `irate(sum(...))`. Catches cgroup-id-recycle resets.
  - `cgroup_irate_by_name` / `cgroup_ratio_by_name`: `UNPIVOT
    INCLUDE NULLS` + `LAST_VALUE(rate IGNORE NULLS) OVER (RANGE
    5min PRECEDING)` carry-forward to keep emitting for 5 min past
    a cgroup's last non-NULL sample.

- **What should look identical to a user.** Every chart on every
  dashboard page (cpu, memory, network, scheduler, syscall, softirq,
  blockio, exceptions, cgroups, gpu, overview, rezolus). A/B compare
  shape. Save-as-Report round-trip. WASM static viewer
  (`site/viewer/`). If the *value* of any plot differs from how it
  rendered pre-branch, that's a regression worth flagging.

- **What should look intentionally different.**
  - **`mcp query` takes DuckDB SQL**, not PromQL. The CLI surface
    changed; M-in-MCP clients are LLMs and SQL-fluent.
  - **First query per parquet has a cold-start cost** while the
    `DuckDbBackend` warms a slot (~hundreds of ms for tens of MB).
    Subsequent queries are warm-pool hits. Live mode has no
    cold-start — `_src` grows in place as snapshots arrive.
  - **Per-cgroup individual plots** show a ~5-min tail of
    carried-forward rate past an exited cgroup's last sample. This
    matches PromQL's `[5m]` lookback; the SQL pipeline was wrongly
    truncating before the cgroup-NULL-tail fix landed.

- **How to test — automated** (from `/work/rezolus` unless noted):

  ```bash
  # Build (single configuration; no feature flags)
  cargo build --bin rezolus
  cargo tree -p rezolus | grep 'metriken-query ' # empty (only metriken-query-sql appears)

  # Native + frontend + smoke
  cargo test --bin rezolus                       # 192 tests
  cargo test -p dashboard -p prom-matrix -p viewer-sql -p report-save
  cargo test --test mcp_cli                      # 8
  node --test tests/*.mjs                        # 137 pass / 0 fail
  bash tests/viewer_smoke.sh                     # requires jq

  # Headless-Chromium per-section render check. Drives `rezolus view
  # <parquet>` and asserts every section in /api/v1/sections rendered
  # a chart, an `_unavailable` placeholder, or a `.section-notes`
  # no-data callout. Catches the silent-render mode the API-only
  # viewer_smoke.sh can't see (section returns 200 but body is blank).
  # Requires chromium, jq, python3, `pip install --user websockets`.
  bash scripts/viewer_chromium_smoke.sh site/viewer/data/cachecannon.parquet
  # Live-mode variant (post-f5482ff): waits N seconds after viewer
  # startup so _src has rows, then walks every section against a
  # running agent.
  bash scripts/viewer_chromium_smoke.sh --live http://localhost:4241 --ingest-wait 5

  # Engine side
  cd /work/metriken && cargo test --workspace --all-features --all-targets

  # Cross-engine shake-down — the single most important automated check
  cd /work/rezolus
  rm -rf /tmp/dashboard_json && mkdir -p /tmp/dashboard_json
  cargo run -p dashboard -- /tmp/dashboard_json
  cd /work/metriken
  rm -rf /tmp/sql_vs_promql && mkdir -p /tmp/sql_vs_promql
  cargo run --release --example sql_vs_promql --features "legacy,harness" -- \
      --dashboard-dir /tmp/dashboard_json \
      --parquets /work/rezolus/site/viewer/data/{demo,AB_level_pin,AB_base}.parquet \
      --out /tmp/sql_vs_promql
  # Expected: pairs=699 identical=698 divergent=1 (numa-local-rate)
  ```

- **How to test — manual (browser).** Already running on `:8080`
  with `vllm.parquet`; for a fresh viewer:

  ```bash
  REZOLUS_NO_OPEN=1 ./target/debug/rezolus view <parquet> --listen 127.0.0.1:8080
  ```

  Things to exercise:
  - Every section in the sidebar populates; time-range slider
    refetches cleanly.
  - Cgroup section (try `AB_level_pin.parquet`): select cgroups;
    individual plots fan out; some series should extend ~5 min past
    last data with a near-zero tail — that's the new NULL-tail
    behaviour, not a bug.
  - Save-as-Report: pin charts, save, re-open in a fresh viewer —
    only pinned charts in the same time window.
  - Events feature (main's contribution; integrated through our SQL
    save paths): freeze a tooltip, click "+ Add Event", submit;
    marker should appear across all charts; persist through
    Save-as-Report → re-open.
  - A/B compare: load two parquets.

- **Known not-regressions** — documented; don't report.
  - `sql_vs_promql` historical regression check (pre-deletion): the
    only divergence was `numa-local-rate` on `AB_base` (`rel ≈
    2.7e-5`), a floating-point residual sub-tolerance at `--rel-tol
    1e-4`. The harness + PromQL evaluator are both gone post-C5;
    correctness now rests on the L2 parquet ↔ live parity tests in
    `metriken-query-sql/src/live.rs::tests` + dashboard snapshot
    tests + chromium per-section smoke.
  - The pre-deletion `~24 dead-code warnings from PromQL helpers
    still in tree` warning that the SQL-only build used to emit is
    gone with `metriken-query`.

- **When something looks wrong** — quick triage.
  - **Wrong value vs. main.** Re-run `sql_vs_promql` against the
    offending parquet; look at `<out>/<stem>/<plot_id>.json`'s
    `verdict.divergent.reason` for the first mismatch.
  - **Empty chart that shouldn't be.** Check the network tab for
    `/api/v1/query_range` — if `status: "error"` and `errorType:
    "sql_error"`, the binder-error shim didn't catch it; report
    with the exact SQL. For a *whole section* that looks blank,
    run `bash scripts/viewer_chromium_smoke.sh <parquet>` — it
    distinguishes "section silently empty" (real bug) from
    "section rendered placeholders / no-data notes" (expected on
    parquets missing matching metrics or pre-`91ea72e` KPI
    templates). Per-section screenshots land in the printed
    output dir.
  - **Slow first load.** Cold-start is expected (~hundreds of ms);
    multi-second pause on a small parquet is worth flagging.
  - **Events disappear.** Confirm with a `save_single_parquet_sql`
    round-trip — both engines should write `KEY_EVENTS` to the
    parquet footer and the viewer should re-read it on load.
