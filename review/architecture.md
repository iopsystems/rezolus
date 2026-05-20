# Rezolus: a tour for newcomers

The shortest possible summary: **Rezolus is one binary with seven subcommands. The only one that actually collects anything is the agent. Everything else either ships the agent's snapshot somewhere, writes it to a parquet file, or reads from a parquet file.** Once you internalize that, the codebase falls into place.

## The big picture

```
            ┌──────────────────────────── COLLECTION ────────────────────────────┐
            │                                                                    │
            │   Linux kernel  ── eBPF programs (src/agent/bpf, *.bpf.c)          │
            │   /proc, /sys   ── procfs/sysfs samplers                           │
            │                       │                                            │
            │            Sampler trait, registered via linkme                    │
            │            distributed_slice SAMPLERS                              │
            │            (src/agent/samplers/{cpu,blockio,scheduler,             │
            │             tcp,network,syscall,memory,gpu,rezolus}/…)             │
            │                       │                                            │
            │             writes into the per-process                            │
            │              [ metriken metric registry ]                          │
            │                       │                                            │
            │  external metrics ──► [ SnapshotBuilder ]  ◄── systeminfo crate    │
            │  (unix socket,         src/agent/exposition       (crates/         │
            │   line/binary)                                     systeminfo)     │
            │                       │                                            │
            │             msgpack-encoded SnapshotV2                             │
            │                       │                                            │
            │            ┌──────────┴──────────┐                                 │
            │            │  AGENT HTTP :4241   │  /metrics/binary  (msgpack)     │
            │            │  axum, tokio        │  /metrics/json                  │
            │            │  (src/agent/        │  /metrics/descriptions          │
            │            │   exposition/http)  │  /systeminfo                    │
            │            └──────────┬──────────┘                                 │
            └───────────────────────┼────────────────────────────────────────────┘
                                    │
       ┌─────────────────────┬──────┴────────┬────────────────────┐
       ▼                     ▼               ▼                    ▼
  ┌──────────┐         ┌──────────┐    ┌──────────┐         ┌──────────┐
  │ EXPORTER │         │ RECORDER │    │HINDSIGHT │         │ VIEWER   │
  │          │         │          │    │          │         │ (live)   │
  │ polls,   │         │ polls,   │    │ polls    │         │ polls,   │
  │ converts │         │ streams  │    │ into     │         │ ingests  │
  │ to Prom  │         │ to       │    │ on-disk  │         │ into     │
  │ text on  │         │ parquet  │    │ ring     │         │ DuckDB-  │
  │ /metrics │         │ on disk  │    │ buffer;  │         │ backed   │
  │          │         │ (also    │    │ SIGHUP   │         │ Live-    │
  │          │         │ accepts  │    │ or HTTP  │         │ Source   │
  │          │         │ Prom     │    │ trigger  │         │ (same    │
  │          │         │ scrape)  │    │ → parquet│         │ backend  │
  │          │         │          │    │          │         │ as file) │
  └──────────┘         └────┬─────┘    └────┬─────┘         └──────────┘
                            │               │
                            ▼               ▼
                ┌────────────────────────────────────┐
                │      PARQUET FILE  (on disk)       │   ◄── the shared currency
                │                                    │
                │  columnar layout from              │
                │   metriken-exposition              │
                │   • timestamp (u64 ns)             │
                │   • duration (u64 ns, nullable)    │
                │   • one column per metric          │
                │     (counter u64 / gauge i64 /     │
                │      histogram List<u64>)          │
                │  + footer KV metadata:             │
                │    source, sampling_interval_ms,   │
                │    systeminfo JSON, descriptions,  │
                │    per_source_metadata             │
                │    (incl. service-extension KPIs)  │
                └──────────────────┬─────────────────┘
                                   │
        ┌────────────────┬─────────┼────────────┬──────────────────┐
        ▼                ▼         ▼            ▼                  ▼
  ┌──────────┐   ┌─────────────┐ ┌──────┐  ┌──────────────┐  ┌──────────────┐
  │ parquet  │   │ VIEWER      │ │ MCP  │  │  viewer-sql  │  │ third-party  │
  │ tools    │   │ (file / A-B │ │ tool │  │  WASM crate  │  │ (your        │
  │ metadata │   │  / upload)  │ │ srvr │  │  in browser  │  │  notebook,   │
  │ annotate │   │             │ │ + CLI│  │              │  │  Pandas,…)   │
  │ combine  │   │  axum +     │ │      │  │  duckdb-wasm │  └──────────────┘
  │ filter   │   │  SqlCapture │ │ same │  │  + shared    │
  │ events   │   │  + DuckDB   │ │ DuckDB│ │  macros via  │
  └──────────┘   │  backend    │ │ back │  │  include_str!│
                 └──────┬──────┘ │ end  │  │  from        │
                        │        └───┬──┘  │  metriken    │
                        ▼            ▼     │  sibling     │
                 ┌────────────────────┐    │  repo        │
                 │  /api/v1/query{,   │    └──────┬───────┘
                 │   _range}          │           │
                 │  takes raw SQL,    │           ▼
                 │  runs through      │    JS frontend, served from
                 │  DuckDbBackend,    │    site/viewer/ via the same
                 │  projects Arrow    │    src/viewer/assets symlinks
                 │  → Prometheus      │    that the native viewer uses
                 │  matrix JSON       │
                 │  via prom-matrix   │◄────── shared crate; one
                 └─────────┬──────────┘        envelope formatter,
                           ▼                   two front-doors
                  ┌─────────────────┐          (native, wasm)
                  │ JS frontend     │
                  │ src/viewer/     │
                  │  assets/        │
                  │  (vanilla JS,   │
                  │  uplot etc.)    │
                  │ Dashboard JSON  │
                  │ comes from      │
                  │ crates/dashboard│
                  └─────────────────┘
```

## Reading the diagram

**The msgpack snapshot is the wire format; parquet is the on-disk format.** Both originate from `metriken-exposition` (a sibling-repo crate). Everything to the right of the agent is some flavor of "drain the snapshot, possibly persist it, possibly query it."

**Three subcommands consume live msgpack, do different things with it.** The exporter is for people who already have Prometheus. The recorder is for "I want this on disk now." Hindsight is for "I want a rolling window so that when something explodes I can post-mortem the last N minutes." Those three look almost identical inside — a tokio interval, a `reqwest::Client`, a write target. Don't be fooled by directory boundaries.

**The viewer has one query engine, two ingest paths.** File mode (and A-B compare, and the upload UI) opens parquet through `SqlCapture` + `metriken_query_sql::DuckDbBackend`. Live mode polls msgpack from the agent and appends each snapshot to a `LiveSource` registered with the *same* backend under the key `live:baseline`; a `LiveCapture` wraps the `LiveSource` to satisfy `DashboardData` reads on the live capture slot. `/api/v1/query{,_range}` dispatches on a data-source string but the SQL code path is identical. `validate_service_extensions` (the load-time KPI availability check) runs each `kpi.sql` through the same backend — no PromQL anywhere. The C2–C5 sequence that landed this state (Phase 0 prune → `validate_service_extensions` to SQL → drop the Tsdb ingest arm → delete the `metriken-query` crate) is done; the codebase has a single build matrix and `metriken-query` is gone from the dep tree. Treat the SQL pipeline as the spine, not a parallel system.

**The static site viewer is the same frontend wearing a different backend.** `site/viewer/lib/` is symlinks into `src/viewer/assets/lib/`, so any JS you change shows up in both. What differs is who runs the SQL: server viewer hands queries to native DuckDB via `duckdb-rs`; the static viewer hands them to `duckdb-wasm` running in the browser, with a thin Rust→WASM shim in `crates/viewer-sql` that mostly exists to project Arrow into the Prometheus-matrix JSON shape the frontend already understands. The `crates/prom-matrix` crate enforces that the JSON shape can't drift between native and WASM — there's one envelope formatter, two entry points.

**The SQL macros — `rate_5m`, `hist_p99`, `cpu_busy_pct`, etc. — are the contract.** Source of truth is `shared_macros.sql` in the sibling `metriken` repo. The native backend registers them as DuckDB scalar UDFs; the WASM viewer can't run UDFs, so `crates/viewer-sql/src/macros.sql` ships pure-SQL substitutes for the H2-histogram primitives and `include_str!`'s the rest verbatim across the paired-repo layout. If queries behave differently in the two viewers, look here first.

**MCP is parquet-only.** The May-2026 migration moved every MCP analysis tool — anomaly detection, correlation, free-form SQL — onto `DuckDbBackend`. It opens a parquet, runs SQL, returns results. The audit-and-delete pass already landed (`510b3c3` / `a06c6ab`); `grep "live-mode" src/mcp/` is empty. MCP builds unconditionally — there is only one build configuration.

**Service extensions are the user-facing reason any of this composes.** A "service extension" is a JSON template under `config/templates/{vllm,sglang,valkey,…}.json` that names a service, lists per-source KPIs, and ships a SQL body for each KPI. When a recording's parquet metadata names a matching `source`, the viewer activates that section; `parquet annotate` re-validates each KPI by running its SQL through DuckDB and stamps `available: true/false` into the footer. 209 / 218 in-tree KPIs ship SQL; the 9 SQL-less KPIs all live in `inference-library.json` (a placeholder template). Templates ship `include_dir!`-baked into the release binary; `--templates` overrides for development.

## The dependency picture

```
   metriken-core  ──►  metriken  ──►  metriken-exposition  ──►  parquet/arrow
                          │                  │
                          │                  └─► msgpack
                          │
                          └──►  metriken-query-sql  ──►  duckdb-rs (native)
                                (DuckDbBackend, LiveSource, MetricCatalog,
                                 shared_macros.sql, H2 histogram UDFs)
```

The whole `metriken*` family lives in a sibling repo at `../metriken/`. Cross-repo `include_str!` is intentional (see `crates/viewer-sql/src/lib.rs` comments) — both crates are co-developed and the rezolus build assumes the paired layout.

## Things that surprise first-time readers

- **`build.rs` is non-trivial.** It compiles every `mod.bpf.c` next to a sampler into a Rust skeleton via `libbpf-cargo`, parameterized by per-arch `vmlinux.h` in `src/agent/bpf/{x86_64,aarch64}/`. You need clang. Mac builds get a stub.
- **Samplers self-register.** No central enable list — each module's `#[distributed_slice(SAMPLERS)]` attribute (via linkme) is what makes it appear in the agent. Greppable but not visible from `mod.rs`.
- **Recorder also speaks Prometheus.** Pointed at `http://host:9090/metrics`, it scrapes Prometheus text and writes the same parquet schema. That's how you combine a service's Prom metrics with Rezolus's BPF metrics — `parquet combine` merges the two files post-hoc.
- **Hindsight has an HTTP control plane too.** Not just SIGHUP. The dump endpoint accepts a time-range filter so you can grab "the last 90 seconds" rather than the whole ring buffer.
- **The dashboard crate is a code generator, not a service.** `crates/dashboard/src/dashboard/*.rs` builds plot definitions (with SQL) that get serialized to JSON and shipped to the frontend. The viewer's HTTP handlers don't *render* charts — they hand the JS the SQL to run.
- **A single build configuration.** `cargo build --bin rezolus` is the only build matrix. The `live-mode` / `sql-only` feature seams existed during the C1–C5 migration but were removed once the `metriken-query` crate left the dep tree; `cargo tree -p rezolus | grep 'metriken-query '` is empty (only `metriken-query-sql` appears). One binary, one code path — don't add new code that depends on PromQL or Tsdb, neither exists in the dep graph.

That's the whole forest. Once a specific tree calls — say, why a particular sampler is laid out the way it is, or how the `prom-matrix` projection handles labels — start at the relevant box and walk inward.
