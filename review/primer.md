# Primer — testing `yv/sql-testing`

For someone running the binary and the test suite. The code reviewer
reads `review.md`; the newcomer reads `architecture.md`.

- **What the branch does.** Migrates the file/upload/A-B viewer, MCP,
  `parquet annotate`, and Save-as-Report column trim off in-memory
  PromQL (`metriken_query::Tsdb`) and onto DuckDB-backed SQL through
  a new `metriken_query_sql::DuckDbBackend`. The live-agent viewer
  is unchanged: still PromQL, gated behind `live-mode` (default-on),
  and `--features sql-only` drops the metriken-query dep entirely.

- **Where the change lives** (concept order — engine first, callers after).
  - **The engine.** `/work/metriken/metriken-query-sql/` is a new
    crate. `backend.rs` owns the per-source connection pool with
    panic-safe slot eviction; `udf.rs` the H2 histogram UDFs;
    `shared_macros.sql` the 20-macro SQL layer (`cpu_busy_pct`,
    `irate_1s`, `h2_combine_lol`, …); `views.rs` materialises
    `_src_<source>` per-source views and the `_cgroup_index` JOIN
    table. The same `shared_macros.sql` is `include_str!`d by both
    the native engine and the WASM viewer so emitters can't drift.
  - **The emitters.** `crates/dashboard/src/sql.rs` (715 LOC, 16
    helpers) is what 130+ plot call sites in `dashboard/*.rs` call
    to produce each plot's `sql_query`. Every helper is
    snapshot-tested in `tests/sql_snapshots.rs`; the two newest
    (cgroup `irate_by_name`, `ratio_by_name`) also have a
    behavioural integration test in `tests/cgroup_null_gap.rs`
    that runs the SQL against a fabricated `_src` and asserts on
    row counts after a NULL transition.
  - **The viewer.** `Arc<DuckDbBackend>` lives on `AppState`
    (`src/viewer/state.rs`). Parquets are wrapped by `SqlCapture`
    (`sql_capture.rs`, 379 LOC) and held by `CaptureRegistry`
    (`capture_registry.rs`, 305 LOC). `/api/v1/query{,_range}` in
    `routes.rs` runs raw SQL through the backend, with a
    binder-error → empty-matrix shim that preserves the
    pre-migration "unknown metric → empty series" UX.
  - **Parallel migrations.** MCP (`src/mcp/backend.rs` is the
    shared helper; ~2,100 LOC of legacy Tsdb/PromQL deleted),
    Save-as-Report (`crates/report-save/src/lib.rs`:
    `resolve_kept_columns_sql` + `save_*_sql` paths; merge with
    main also threaded `events: Vec<Event>` → `KEY_EVENTS` through
    all four save entrypoints), `parquet annotate`
    (`src/parquet_tools/annotate.rs`: validates each KPI's SQL
    binds against the parquet).
  - **The verification harness.**
    `/work/metriken/metriken-query/examples/sql_vs_promql.rs`
    (~1,700 LOC) runs every dashboard plot through both PromQL
    (legacy evaluator) and SQL (wasm-style DuckDB connection
    mirroring the browser's setup), and diffs canonical JSON
    plot-by-plot. Substitutes `__SELECTED_CGROUPS__` per parquet
    from a fabricated `_cgroup_index` built from Arrow field
    metadata.

- **What's verified.** The harness against `demo.parquet`,
  `AB_level_pin.parquet`, `AB_base.parquet` reports **698 identical
  / 1 divergent / 0 errors / 0 skipped**. The single divergence is
  `numa-local-rate` on `AB_base.parquet`, `rel ≈ 2.7e-5` —
  floating-point residual from the 5-min RANGE arithmetic;
  sub-tolerance. Three behavioural fixes landed this session to
  bring SQL into alignment with PromQL semantics:
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
    Subsequent queries are warm-pool hits.
  - **Per-cgroup individual plots** show a ~5-min tail of
    carried-forward rate past an exited cgroup's last sample. This
    matches PromQL's `[5m]` lookback; the SQL pipeline was wrongly
    truncating before the cgroup-NULL-tail fix landed.
  - **`rezolus view http://agent:4241`** returns `capture_not_found`
    for `/api/v1/query{,_range}`. The live-agent query path is the
    explicit `live-mode` carve-out — pending the live-ingest
    storage decision in `review.md` "Removing Tsdb entirely" item D.

- **How to test — automated** (from `/work/rezolus` unless noted):

  ```bash
  # Build matrix
  cargo build --bin rezolus
  cargo build --bin rezolus --no-default-features --features sql-only
  cargo tree -p rezolus --no-default-features --features sql-only \
      | grep 'metriken-query v'                  # must be empty

  # Native + frontend + smoke
  cargo test --bin rezolus                       # ~174
  cargo test -p dashboard -p prom-matrix -p viewer-sql -p report-save
  cargo test --test mcp_cli                      # ~8
  node --test tests/*.mjs                        # 116 pass / 6 fail (see below)
  bash tests/viewer_smoke.sh                     # requires jq

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
  - 6 JS test failures (5 in `compare_node_filter.test.mjs`, 1 in
    `wasm_viewer_histogram_kpis.test.mjs`). PromQL-side
    `buildEffectiveQuery` paths skipped on `BACKEND='sql'` and a
    retired in-process WASM viewer. To be retired in follow-up.
  - `sql_vs_promql` `numa-local-rate` divergence on `AB_base`
    (`rel ≈ 2.7e-5`). Floating-point residual; sub-tolerance at
    `--rel-tol 1e-4`.
  - Live-agent mode charts blank; query returns
    `capture_not_found`. Explicit carve-out.
  - `--no-default-features --features sql-only` build emits ~24
    dead-code warnings from PromQL helpers still in tree.
    Self-clears with the legacy-deletion follow-up.

- **When something looks wrong** — quick triage.
  - **Wrong value vs. main.** Re-run `sql_vs_promql` against the
    offending parquet; look at `<out>/<stem>/<plot_id>.json`'s
    `verdict.divergent.reason` for the first mismatch.
  - **Empty chart that shouldn't be.** Check the network tab for
    `/api/v1/query_range` — if `status: "error"` and `errorType:
    "sql_error"`, the binder-error shim didn't catch it; report
    with the exact SQL.
  - **Slow first load.** Cold-start is expected (~hundreds of ms);
    multi-second pause on a small parquet is worth flagging.
  - **Events disappear.** Confirm with a `save_single_parquet_sql`
    round-trip — both engines should write `KEY_EVENTS` to the
    parquet footer and the viewer should re-read it on load.
