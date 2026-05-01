# Rezolus Viewer & Metriken Query Patterns

A technical retrospective of the data access path that connects a parquet
capture on disk to a chart in the Rezolus viewer, tracing how that path
evolved through a dense burst of PRs in April–May 2026, and what those
changes teach us about handling multi-source parquet at scale.

> **Note on the working branch.** This report is committed to
> `claude/rezolus-query-patterns-HTQSU` on `iopsystems/rezolus`. The branch
> did not exist on either remote when the research started; it was branched
> off `main` at `553c790` for this delivery. The same branch name does not
> currently exist on `iopsystems/metriken`. Repository states surveyed:
> `iopsystems/rezolus@607f15c` and `iopsystems/metriken@606ca9a`.

---

## 1. The Two Viewer Stacks

The Rezolus viewer ships in two physically distinct stacks that share a
single Rust dashboard/query core. The unification of those stacks is itself
a recent event — see PR #806 below.

| | Standalone mode | WASM-only mode |
|---|---|---|
| Build target | `rezolus` binary (axum server) | `wasm_bindgen` module loaded by static site |
| Source root | `src/viewer/` | `crates/viewer/` + `site/viewer/` |
| Parquet load | `Tsdb::load(path)` over the local FS | `Tsdb::load_from_bytes(Bytes)` over a `Uint8Array` from `fetch()` |
| Transport | HTTP `/api/v1/{query, query_range, sections, …}` | `wasm_bindgen` calls wrapped by `viewer_api.js` |
| Engine | `metriken_query::QueryEngine<&Tsdb>` | `metriken_query::QueryEngine<&Tsdb>` (identical) |
| JS data layer | `site/viewer/lib/data.js` | `site/viewer/lib/data.js` (identical) |

Both stacks build dashboard JSON via the shared `crates/dashboard/` crate
and execute PromQL via `metriken_query`. Crucially, the JavaScript data
layer (`data.js`) cannot tell the two transports apart — it calls
`ViewerApi.queryRange(query, start, end, step, captureId)` and gets back the
same `{status, data: {resultType, result: [...]}}` shape from either side.

### 1.1 Standalone query path

`src/viewer/mod.rs:250-258` calls `Tsdb::load(path)` once at startup. The
result is held in an `Arc<RwLock<Tsdb>>` inside a `CaptureRegistry` with a
mandatory `baseline` slot and an optional `experiment` slot. Parquet KV
metadata (`systeminfo`, `selection`, `per_source_metadata`) is pulled
separately from the parquet footer using
`parquet::file::serialized_reader::SerializedFileReader` — i.e. without
touching column chunks.

Range queries are minimal:

```rust
async fn range_query(...) {
    let tsdb = tsdb_handle.read();
    let engine = QueryEngine::new(&*tsdb);
    match engine.query_range(&params.query, params.start, params.end, params.step) { ... }
}
```

### 1.2 WASM-only query path

The user drops a parquet (or hits `?demo=…`/`?capture=…`); `script.js`'s
`fetchDemoBytes` streams the file through `fetch().getReader()` with
progress callbacks, producing a `Uint8Array`. That buffer is handed to
`WasmCaptureRegistry::attach('baseline', data, filename)`, which calls
`Tsdb::load_from_bytes(Bytes::from(data.to_vec()))`. The whole parquet is
materialized into the in-memory TSDB before any query runs.

Both stacks carry the same `Slot::{Baseline, Experiment}` enum and run
queries against an `Arc<RwLock<Tsdb>>` (server) or `RefCell<Tsdb>` (wasm).

### 1.3 The shared data-shaping layer

`site/viewer/lib/data.js` is responsible for all per-plot query construction
and is invariant across stacks:

- Step computation: `windowDuration = min(3600, maxTime − minTime)`,
  `step = stepOverride ?? max(1, floor(windowDuration / 500))`. The default
  yields ≤500 points per series.
- `buildEffectiveQuery` rewrites per-plot PromQL for histograms
  (`histogram_percentiles(...)`, `histogram_heatmap(..., stride)`),
  counter-rate (`irate(m[5m]) → rate(m[Ns])` when granularity is overridden),
  cgroup substitution (`__SELECTED_CGROUPS__`), and PromQL-keyword-aware
  label injection (`{node="…"}`, `{instance="…"}`).
- `processDashboardData` walks every plot in a section, builds the effective
  query, and issues a `Promise.allSettled` of per-plot range queries. After
  PRs #835/#848/#851 this is gated by section visibility, not eagerly run
  for the whole dashboard.

There is **no client-side aggregation or downsampling** — aggregation is
done in PromQL (`rate`, `sum by(...)`), downsampling via the `step`
parameter, and pagination via the 3600 s window clamp on initial load. The
granularity selector (PR #773) lets users widen `step` to 1 s/15 s/1 m/15 m
for sparser/cheaper renders.

---

## 2. How Metriken Handles Data and Queries

Metriken splits cleanly into a writer side and a reader side.

### 2.1 Writer: `metriken-exposition`

A capture is **a single parquet file** — there is no horizontal partitioning
across files. Each row is one snapshot tick. The schema (built in
`ParquetSchema::finalize`, `metriken-exposition/src/parquet.rs`):

| Column | Type | Notes |
|---|---|---|
| `timestamp` | `UInt64` ns | non-null |
| `duration` | `UInt64` ns | nullable |
| `<counter_name>` | `UInt64` | nullable |
| `<gauge_name>` | `Int64` | nullable |
| `<histogram>:buckets` | `List<UInt64>` | dense |
| `<histogram>:bucket_indices` + `:bucket_counts` | `List<UInt64>` | sparse |

Per-field Arrow metadata carries `metric_type`, `unit`, `grouping_power`,
`max_value_power`, plus user labels. Default row-group size is 50 000 rows;
default compression is zstd-3. The writer comment notes the explicit
trade-off: larger row groups compress better and use more memory during
creation, but operations on the file can be parallelized per row group, so
too few row groups limit core utilization on the read side.

### 2.2 Reader: `metriken-query`

This is what the viewer consumes. The public surface:

```rust
pub use bytes::Bytes;
pub use promql::{QueryEngine, QueryError, QueryResult};
pub use tsdb::Tsdb;
```

Entry points:

- `Tsdb::load(&Path)` and `Tsdb::load_from_bytes(Bytes)` — single-file
  ingest. The reader uses `parquet` + `arrow` directly. **There is no
  DataFusion** anywhere in the workspace.
- `QueryEngine::new(tsdb)` — generic over `T: Deref<Target = Tsdb>`, so it
  accepts `&Tsdb` for zero-copy borrowed access.
- `QueryEngine::query_range(query, start, end, step)` — the workhorse
  range query, returning `QueryResult::Matrix { result: Vec<MatrixSample> }`.
- `QueryEngine::query(query, time)` — instant query, internally collapsed by
  `matrix_to_vector` over a degenerate range.
- Two non-standard PromQL functions, pre-parsed before the AST parser:
  `histogram_quantiles([qs], metric{...}[, stride])` and
  `histogram_heatmap(metric{...}[, stride])`.

### 2.3 In-memory model

```rust
pub struct Tsdb {
    sampling_interval_ms: u64,
    counters:   HashMap<String, CounterCollection>,
    gauges:     HashMap<String, GaugeCollection>,
    histograms: HashMap<String, HistogramCollection>,
    /* ... */
}
```

Each `*Collection` is a `BTreeMap<Labels, *Series>` (one metric name → many
label-distinguished series). Counter and gauge series store sorted
`Vec<(u64, V)>`. Histograms use a flat **CSR-like layout**:

```rust
pub struct HistogramSeries {
    config: Option<Config>,        // shared across all snapshots
    timestamps: Vec<u64>,          // 8 B/snapshot
    offsets: Vec<u32>,             // 4 B/snapshot
    indices: Vec<u32>,             // concatenated non-zero bucket indices
    counts:  Vec<u32>,             // concatenated running-cumulative
}
```

Per-snapshot fixed overhead is **12 B**, down from 88 B in the original
cumulative-form layout. Each snapshot stores the **per-period delta** of
non-zero bucket indices; counts are a local prefix sum within the snapshot.
`iter()` borrows directly into the flat buffers via `from_parts_unchecked` —
zero allocation per snapshot.

### 2.4 Time range, step, multi-metric reads

- **Time range:** `Tsdb::time_range` reduces every series's `time_bounds()`
  to an overall `(min_ns, max_ns)`.
- **Step / downsampling:** the viewer passes `step` (seconds) into
  `query_range`; `Ctx { start_ns, end_ns, step_ns, interval_ns }` is built
  once and threaded through the streaming pipeline.
- **Multi-metric joins:** there is no inter-file query layer. Within a
  single `Tsdb`, multi-metric queries are joined by the streaming dispatcher
  through the PromQL AST: vector selectors, binary ops, and aggregations
  walk the same in-memory `*Collection` map.
- **Multi-source data:** handled either by ingesting multiple `Snapshot`s
  into the same `Tsdb` with `source` labels intact, or by spinning up
  multiple `Tsdb`s and letting the viewer pick one. The dual-TSDB compare
  mode is the only place the viewer sees more than one TSDB at runtime.

---

## 3. The Streaming PromQL Pipeline

After PR #94 in metriken, **streaming is the only PromQL evaluator**:

```rust
pub type Point = (u64, f64);
pub struct LabeledSeries<'a> {
    pub labels: Labels,
    pub iter: Box<dyn Iterator<Item = Point> + 'a>,
}
pub type SeriesSet<'a> = Vec<LabeledSeries<'a>>;
```

Operators (`CounterIrate`, `GaugeAvgOverTime`, `MergeReduce`,
`ScalarBroadcast`, `matrix_matrix_op`, …) wrap upstream iterators and pull
lazily, holding only their windowed state. A single `collect_to_matrix`
boundary collector drains the chain into the `MatrixSample` shape so the
JSON serializer is untouched. Histogram quantiles don't fit cleanly (a
lending-iterator problem) and run as a hand-rolled per-tick loop.

`dispatch::try_streaming` recognizes the PromQL subset Rezolus actually
exercises: parens, `irate`/`rate`/`avg_over_time`/`idelta`/`deriv` over
matrix selectors, bare gauge selectors,
`sum`/`avg`/`min`/`max`/`count [by|without]`, `+ - * /` with `on(..)` /
`ignoring(..)`, scalars, and `histogram_quantile`. Anything else returns
`QueryError::Unsupported`. As the PR description put it, metriken-query
"stops being a general PromQL implementation and becomes 'the PromQL subset
rezolus exercises'."

---

## 4. The PR Sequence That Reshaped the Read Path

The April–May 2026 window contains the most significant rework of the
data-access path in either repo's history. Listed in dependency order
(roughly):

### Rezolus

| PR | Title | Why it matters |
|---|---|---|
| #773 | granularity (step) selector | First user-visible knob over query cost. Underlies the per-plot `irate → rate` and histogram-stride rewrites. |
| #777 | `parquet combine` subcommand | Offline alignment of multiple parquets into one. Validates equal sampling intervals, computes timestamp intersection, uses `arrow::compute::take` to align rows. |
| #779 | `parquet filter` subcommand | Drops columns the dashboard never queries, deriving the keep-list from each KPI's PromQL. Smaller files, smaller TSDB. |
| #795 | multi-node viewer + KPI validation | Restructured `per_source_metadata` to nested `{source_type:{node:metadata}}`; PromQL **label injection** (`{node="…"}`) on the frontend. |
| #800 | Import WASM viewer crate | Brought the WASM viewer into the main repo with a shared `[profile.wasm-release]`, single source of truth. |
| #803 | bump arrow/parquet 54 → 58 | Affects parquet decode in `Tsdb::load`. Dropped a redundant `sum(cpu_cores)` wrapper that was no longer needed. |
| #806 | unify dashboard JSON via shared crate | Extracted `crates/dashboard/`; deleted `dashboards.js`, 13 pre-generated JSON files, the `generate-dashboards` xtask. **Net −5 400 LOC.** WASM viewer now generates dashboards at runtime. |
| #820 | A/B compare mode | Introduced `CaptureRegistry` / `WasmCaptureRegistry` — the dual-TSDB structure. Two parquets are *never* fused at runtime; results are overlaid at chart-render time. |
| #834 | (closed) defer chart fetches, cap parallelism | "Section navigation used to fire one PromQL query per plot up front through `Promise.allSettled`, awaiting all completions before painting." Closed in favor of the narrower #835. |
| #835 | reduce viewer chart loading memory | Removed eager section preloading; lazy compare-mode heatmap matrices; only the displayed heatmap resolution is materialized. |
| #848 | split sections metadata from payloads (Phase 1) | `GET /api/v1/sections` returns navigation only; route cache bounded to 3 entries. |
| #851 | lazy section generation (Phase 2) | `generate(...) → HashMap` becomes `generate_section(data, route, ctx) → Option`. **Section generation is now per-click, not per-load.** |
| #855 | (closed) bump metriken-query 0.9.5 → 0.9.6 + memory regression test | Measured peak heap during `Tsdb::load_from_bytes`: `demo.parquet` 88 → 9 MiB (−90 %); `cachecannon.parquet` 239 → 56 MiB (−77 %); the 5-source nixl combined parquet went from `handle_alloc_error` (browser OOM) to 171 MiB. Rolled into #858. |
| #858 | bump metriken-query 0.9.5 → 0.10.2 | Picks up the streaming PromQL engine and per-row-group decode in both stacks. |

### Metriken

| PR | Title | Why it matters |
|---|---|---|
| #73 | histograms + grouped metrics as first-class types | Recorder-level support; back-compatible wire format. |
| #80 | snap TSDB timestamps to sampling-interval boundaries | Eliminates spurious nulls when binary ops combine metrics from different samplers (e.g. `irate(cpu_usage) / cpu_cores`). |
| #82 | strip internal metadata from parquet column labels | Removes spurious `metric_type='counter'` labels that broke `sum by` joins. |
| #86 | honour `on()` / `ignoring()` | Modifiers were parsed but previously dropped. |
| #88 | store histograms as `CumulativeROHistogram` | Dense `Box<[u64]>` per tick → columnar non-zero buckets, binary-search quantiles. |
| #90 | **−75 % resident memory** | The single largest in-memory restructuring. See § 5. |
| #92 | streaming time-series prototype (`irate` + `sum by`) | First streaming operators. Motivation: "the eager engine materialises every intermediate stage as `Vec<(f64, f64)>`, so a typical `sum by (label) (irate(metric[5s]))` over many series produces a transient `O(stages × points × series)` heap footprint just to be reduced down to `O(stages × points)` at the boundary." |
| #93 | wire `query_range` to the streaming dispatcher | Non-cloning `*_ref` borrow accessors. **Peak heap −65 %, allocated −63 %; aggregations essentially flat (~60 KiB) regardless of input series count.** |
| #94 | collapse evaluator to streaming-only | Removed the eager evaluator entirely (~2 300 LOC shrink). |
| #96 | cache parquet metadata + decode one column at a time | `vllm.parquet` load: 20 989 ms → 739 ms (**28×**). |
| #98 | restore matcher-less single-right binary broadcast | Regression fix for `sum(...) / cpu_cores` after #94's cleanup. |

---

## 5. The CPU ↔ Memory Tradeoff Through the PR Series

Tracking the same workload across the series surfaces a clear pattern: each
optimization picked one axis and pushed; the next PR usually unwound the
collateral cost on the other axis.

### 5.1 The early CPU/memory cliff

The pre-#90 reader followed an "Arrow-natural" pattern: build a full
`RecordBatch` per row group with every column's Arrow buffers decoded
simultaneously, walk it row by row. Convenient for the implementer; on a
wide capture (5 000+ columns from a multi-source combine), it pinned **all
columns' decompressed buffers** into peak memory at once. Resident memory
across an 11-fixture corpus was 2 342 MiB.

### 5.2 PR #90 — the −75 % step

PR #90 layered five techniques (in win order):

1. **Stream parquet column-by-column** with `ProjectionMask::roots([col])`,
   bounding peak to *one column × one batch* of decoded data plus a rolling
   `prev` cumulative for histograms. Drops resident by ~70 % alone.
   Particularly important on WASM because "transient pages claimed by the
   decoder otherwise stay forever."
2. **Per-period delta histograms + u32 narrowing** — store the typically
   5–15 non-zero buckets per snapshot, not the cumulative-since-start union
   that grows monotonically.
3. **`BTreeMap` → sorted `Vec<(u64, V)>`** for all four series types — node
   overhead drops from ~50 B/entry to 16 B packed; range windows use
   `partition_point`.
4. **Config hoist + CSR flatten** — single shared `Config`, three parallel
   buffers. Per-snapshot overhead 88 B → 12 B.
5. *Considered and reverted:* `grouping_power 7→5` auto-downsample. Live
   memory only fell 4.7 % because deltas are already sparse — "collapsing 4
   adjacent buckets into 1 mostly merges already-zero buckets."

Net: resident **2 342 → 571 MiB (−76 %)**, live (jemalloc allocated)
**739 → 262 MiB (−65 %)**.

The technique that saved the most memory cost CPU on the read path: a
column-by-column reader had to open the parquet decode pipeline N times.
That re-pricing is what PR #96 then attacked.

### 5.3 PR #92–#94 — the streaming evaluator

The eager evaluator buffered every intermediate `Vec<(f64, f64)>`. For a
`sum by (label) (irate(metric[5s]))` over many series, that's
`O(stages × points × series)` transient heap to produce
`O(stages × points)` output. Painful in the browser where the wasm32 heap
caps at 4 GiB.

The streaming pipeline pulls Points lazily, holding only operator state.
Across a 43-query bench:

- Peak heap: **12.82 → 7.53 MiB (−41 %)** (full collapse, with histogram
  streaming).
- Peak excluding heatmap: **25.27 → 2.59 MiB (−90 %)**.
- `histogram_percentiles([5 quantiles], response_latency)`: eager peak
  2.63 MiB, **streaming peak 20 KiB (−99 %)**.
- Aggregation peak essentially **flat (~60 KiB) regardless of input series
  count**.

The CPU side was a wash to slightly favorable — fewer allocations and
better cache behavior offset the extra iterator dispatch — but the *real*
CPU saving was in the matching cleanup (PR #94 deleted the cloning
`counters` / `gauges` accessors and replaced them with non-cloning `_ref`
borrows). Total Rust shrink in #94: ~2 300 lines.

### 5.4 PR #96 — paying back the per-column setup tax

PR #90's column-wise loader paid every per-column setup cost — footer
parse, ProjectionMask construction, decompression-pipeline init, page-index
decode, Arrow array materialization — once per column. On a 5 000-column
combined parquet that exploded to minutes of load. Quoting the PR:

> "The pre-refactor loader opened a fresh `ParquetRecordBatchReaderBuilder`
> per column, projected to that single column, and walked its row-groups.
> That kept Arrow-side memory minimal but paid every per-column setup cost
> N times. On wide captures (5000+ columns) this exploded into minutes."

The fix shares the `ArrowReaderMetadata` across all columns and processes
one (row-group, column) chunk at a time. Load time on `vllm.parquet` went
from **20.989 s → 0.739 s (28×)**, with `O(rows + cols)` instead of
`O(rows × cols)` complexity, while keeping #90's peak memory bound.

### 5.5 The viewer-side parallel: PRs #835, #848, #851, #834

The same eager-vs-lazy pattern repeated on the JS/Rust seam in Rezolus.
Initially `dashboard::dashboard::generate(...)` rendered every section's
plot specs at startup, and `processDashboardData` fired one PromQL query
per plot up front. Big sections (CPU, scheduler, syscall) blocked first
paint behind 20+ queries.

The progression:

- **#835** (codex) — JS-side: removed eager section preloading; lazy
  compare-mode heatmap matrices; only the *currently displayed* heatmap
  resolution stays in memory.
- **#848** — Rust-side Phase 1: split section navigation metadata from
  section payloads. Sections cache bounded to 3 entries with `overview`
  pinned. Important caveat from the PR: this only changes the **cache
  layout** — the producer (`dashboard::generate`) was still eager.
- **#851** — Rust-side Phase 2: `generate(...) → HashMap` becomes
  `generate_section(data, route, ctx) → Option`. Section generation is
  finally one-per-click on both stacks.
- **#834** (closed) — proposed `IntersectionObserver`-driven plot fetches
  with a shared `QueryPool` (cap 8). More aggressive than what shipped;
  closed in favor of #835's narrower approach. Worth flagging as the
  next-step candidate if first-paint latency on big sections becomes a
  pressure point again.

### 5.6 Summary of the tradeoff curve

```
Phase                   | CPU (load)  | Memory (resident) | Memory (peak query)
------------------------|-------------|--------------------|---------------------
pre-#90 (eager-all)     |  baseline   |  baseline (2.3 G)  |  baseline
#90 (col-by-col load)   |  much worse |  −76 %             |  unchanged
#92–94 (streaming PQL)  |  ≈ same     |  unchanged         |  −41 % to −99 %
#96 (cached metadata)   |  −96 % (28×)|  same as #90       |  unchanged
#835/848/851 (lazy gen) |  −∼90 %     |  −much             |  irrelevant (per-click)
```

The clean takeaway: **pessimize once on the dimension you don't actually
need, prove it's the bottleneck, then unwind the collateral cost with a
smaller targeted PR.** That's how the team got both wins instead of having
to choose.

---

## 6. Lessons Learned: Multi-Source Parquet at Scale

Stepping back, the PR series points at a small set of durable design
principles for systems that need to query many parquet files of different
provenance.

### 6.1 Don't fuse at runtime if you can fuse offline

The viewer **never fuses two parquets in memory.** Multi-source data is
combined offline by `parquet combine` (#777, #845), which validates equal
sampling intervals, detects column-name conflicts, computes the timestamp
intersection, and uses `arrow::compute::take` to align rows. The output is
a single parquet with a `source` JSON-array metadata entry.

The runtime then needs only a `source=…` label-matcher to slice by source.
This is dramatically simpler than maintaining an inter-file query layer:

- No cross-file joins on hot paths.
- No partial-failure modes when one of N files is corrupted.
- One `Tsdb` to budget memory against.
- `parquet filter` (#779) can shrink the combined file by dropping columns
  no KPI references, mechanically derived from the dashboard's PromQL.

The compare-mode (#820) is the explicit exception: keeping two `Tsdb`s
side-by-side is fine because the cardinality is bounded at 2, and results
are overlaid only at chart-render time.

### 6.2 Ingest once, query many — but ingest narrowly

`Tsdb::load_from_bytes` is a one-shot, all-or-nothing ingest into an
in-memory model. That decision is what makes streaming queries cheap (no
re-decode per query) but it puts pressure on the ingest itself. The
critical insight from #90 → #96 is that *both* peak memory during ingest
and CPU cost of ingest matter:

- **#90's column-by-column** decode bounds peak memory but multiplies CPU.
- **#96's metadata-cache + projection** restores CPU without losing the
  memory bound.

The general shape: when a single eager pass over wide data is too
expensive, decompose it into a series of narrow passes, *but cache the
shared setup work between them*. That's what an actual query engine like
DataFusion does for free — and it's a real question whether metriken should
adopt one. The conscious answer in #94 was "no": the workload is small
enough and the schema regular enough that a hand-tuned reader beats a
generic engine on both binary size and peak heap, and that matters in
WASM.

### 6.3 Make the in-memory layout cardinality-aware

Histograms went through three representations:

1. Dense `Box<[u64]>` per tick — convenient, ~50–200 buckets/snapshot.
2. `CumulativeROHistogram` columnar non-zero — reduced storage but each
   snapshot still re-stores the cumulative union.
3. **CSR delta** — per-period deltas (5–15 non-zero buckets/snapshot)
   shared `Config` across the entire series.

The progression matches what cardinality-aware columnar formats do: the
real signal is *which buckets changed since the last tick*, not the
absolute distribution. The CSR layout drops fixed overhead from 88 B to
12 B per snapshot — that's the difference between a long-running capture
fitting in browser memory and not. The same lesson applies to any
multi-source parquet system where each source's metric vocabulary is sparse
within the union: **store deltas of the sparse representation, not the
dense one**.

### 6.4 Scope the query language to what the consumer uses

PR #94 was a deliberate retreat from PromQL completeness. The dispatcher
only handles the operators Rezolus's dashboards actually emit; everything
else is `QueryError::Unsupported`. That single decision allowed:

- A 2 300-line shrink.
- A consistent streaming evaluator (no eager backstop for "weird" queries).
- A bounded test surface (43 queries cover the workload).
- Predictable memory: aggregations fixed at ~60 KiB regardless of input
  cardinality.

The lesson generalizes: **own your query DSL.** A vendored subset of a
standard language with a tight back-end is often a better architectural
fit than a complete general-purpose engine, especially on a constrained
target like WASM.

### 6.5 Lazy by default, eager only with evidence

The viewer's progression from "render every section, fire every chart
query at startup" to "generate one section per click, fetch when visible"
mirrored metriken's "materialize every intermediate" → "stream lazily"
arc. In both cases the eager design was simpler to write and shipped first;
the lazy redesign was justified by measured CPU/memory pressure. The
pattern worth repeating:

1. Ship eager, instrument it.
2. Find the workload that makes it fall over (5-source combined parquet
   in browser; 43-query bench peak heap).
3. Convert to lazy at the layer that actually saves work — usually the
   broadest layer that still has visibility into "what's needed now."
4. Cache aggressively at that layer (sections cache bounded to 3,
   `cached_bodies: RefCell<HashMap>` on the WASM side).
5. Keep an unmerged "more aggressive" PR on file (e.g. #834) for when the
   lazy version starts to feel slow again.

### 6.6 Treat WASM as a memory budget, not a deployment target

Several decisions only make sense once you accept that wasm32's 4 GiB cap
is the real constraint:

- Browser-side `handle_alloc_error` was the *failure mode* that drove
  PR #855's metric tracking.
- `[profile.wasm-release]` and `zstd-sys` configured for `no_asm`.
- Streaming PromQL was prototyped specifically because the eager evaluator's
  transient heap footprint was painful in the WASM viewer.
- `Tsdb::load_from_bytes` accepts `Bytes` (not `Vec<u8>`) so the JS-side
  `Uint8Array` can be moved without a copy.

Designing the data path *first* for the smaller environment — and letting
the server stack inherit the savings — has been a consistent winning
strategy in this codebase. The server gets cheaper queries for free; the
browser becomes the regression test.

### 6.7 When in doubt, instrument

The single most useful artifact in the PR history is the memory regression
test scaffolding from PR #855: a `GlobalAlloc` wrapper that tracked peak
resident bytes during `Tsdb::load_from_bytes` across three representative
parquets. It's what made every subsequent PR's claims (−65 %, −76 %,
−41 %, 28× speedup) **falsifiable**, which is what makes a tradeoff
discussion meaningful instead of decorative.

---

## 7. Open Questions / Watchlist

- **First-paint latency on big sections.** The closed PR #834 had a more
  aggressive lazy/parallelism design than what shipped. If big-section
  paint becomes a complaint again, that's the place to start.
- **Multi-source ingest on the live agent path.** Today combine is offline.
  An online combine would be a meaningful step up in operational
  flexibility, at non-trivial CPU cost.
- **Histogram percentile streaming.** PR #94 explicitly notes the lending-
  iterator obstacle that kept histogram quantiles on the per-tick path. A
  proper solution would close the last non-streaming corner of the engine.
- **DataFusion reconsideration.** Worth a periodic check: as Arrow's
  decode pipeline gets more efficient and DataFusion's footprint shrinks,
  the gap between hand-tuned reader and generic engine narrows. Today the
  vote is "stay hand-tuned"; that vote should be revisited yearly.
- **`claude/rezolus-query-patterns-HTQSU` on `iopsystems/metriken`** — the
  task instructions named the same branch on both repos, but no Metriken
  changes were needed to deliver this report. If a corresponding branch is
  expected on Metriken (e.g. for cross-linking), it would need to be
  created separately.
