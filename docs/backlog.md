# Backlog

The repo's consolidated backlog. Most items are **deferred/reopen conditions from
the [engineering journal](journal/README.md)** — each links its source entry (the
"why" and mechanism) and, where relevant, a code path; the journal entries are the
record, this file is the *ordering* layer. The last section holds **net-new
capability requests** not yet tied to an effort. When you pick an item up, read
its source (entry or origin) first, and close it out there.

Status key: **Open** (actionable now), **Roadmap** (planned next phase),
**By design** (documented limitation, reopen only if the assumption changes),
**Idea** (net-new capability, not yet scoped).

## Viewer — compare mode (A/B)

Source: [A/B compare mode](journal/2026-04-21-ab-compare-mode.md).

- **N-way compare (N > 2)** — Open/Roadmap. `CaptureRegistry`, the `capture=`
  query param, and the `alias=path` positional syntax were built to generalize,
  but v1 assumes two slots and the wire-stable `baseline`/`experiment` ids are
  hard-coded. A third slot needs those ids to become positional or named-but-open.
  *Reopen:* when a third capture is actually needed. Design constraint captured in
  the entry (n-way extension). **Now on the critical path** of
  [retire the `.parquet.ab.tar` container](journal/2026-08-27-retire-ab-tarball.md):
  a two-arm, two-source comparison encodes in a `.rez` as four `{arm, source}`
  recordings, so replacing the tarball needs this.
- **Hot-swap a capture** (replace one side, keep the other) — Open. Out of scope
  for v1; no architectural obstacle noted.
- **Live-agent compare** (file+live or live+live) — Roadmap. Explicitly excluded;
  requires a capture slot backed by a running-agent `Tsdb` rather than a
  loaded-once parquet. No near-term demand.
- **Baseline-anchor drag UI** — Open. Selection state already carries
  `anchors.baseline`; anchoring the baseline to a non-first sample has no UI yet.
- **Alias collision in saved A/B tarballs** — By design. When both sides share a
  filename basename, the compare badge shows two identical labels;
  `synthesize_ab_manifest` does not dedupe. Decided to let the user rename
  (documented in #960). Reopen only if it bites in practice.

## A/B container consolidation (`.ab.tar` → `.rez`)

Source: [retire the `.parquet.ab.tar` container](journal/2026-08-27-retire-ab-tarball.md).

- **GATE: can the WASM viewer read a v3 `.rez`?** — Open, and first. Everything
  below is premised on this passing. *Reopen as a new entry* if it does not.
  - **Wave 1 (bundle cost) — measured 2026-08-27, not disqualifying.**
    `rusqlite 0.40` builds for `wasm32-unknown-unknown` with stock clang and no
    emscripten (it resolves to `sqlite-wasm-rs`, not `libsqlite3-sys`); viewer
    bundle +425 KB gzipped (1.277×), an upper bound with no `wasm-opt`.
  - **Wave 2 (memory model) — Open, and now the real question.** The default
    `sqlite-wasm-rs` VFS is in-RAM, which would put the whole archive in wasm
    memory — the property the v3 entry rejected tar for. Root cause is that
    SQLite's `xRead` is synchronous while browser file APIs are async; the entry
    explains the interface. Measure peak wasm memory opening real `.rez` archives
    at several sizes; evaluate a read-only path that skips the SQL engine and
    streams segment BLOBs; test the `FileReaderSync`-backed VFS hypothesis (sync
    random access over the picked `File`, no OPFS copy, worker required —
    unverified); failing all three, cost the dedicated-worker + OPFS (`sahpool`)
    restructure.

  *Effort parked 2026-09-01 pending pickup.*
- **Point `combine --ab` at the `.rez` form; add `.rez` to the picker `accept`
  list** — Open, independent of the gate. `src/viewer/assets/lib/ui/layout.js:26`
  omits `.rez` in both viewers even though the server ingests it by content
  sniffing. Cheap, and it slows growth of the tarball corpus.
- **`.rez` manifest carries selection + events** — Open. `KEY_SELECTION`/`KEY_EVENTS`
  are parquet footer keys today; in a `.rez` they are a catalog `UPDATE`, the shape
  `annotate` established in #1073.
- **Column-level trim for `.rez`** — Open. `filter` drops whole tables by sampler;
  Save-as-Report needs per-column projection, which means decode → project →
  re-encode segments. The one item that breaks the verbatim-BLOB-copy property
  `rez_v3_rewrite` has held since #1073 — write it knowing that.
- **Windowless `.rez` tables (non-rezolus sides)** — Open. Ingest is close (the
  recorder already converts Prometheus scrapes to Snapshots; `demote_from_rez`
  documents the refusal as policy, `src/recorder/mod.rs:744-760`). The reader must
  report *no* band for such a table rather than a fabricated one. Open sub-question:
  whether these should be recordable by `record` at all, or only constructible by
  `combine`.

## Viewer — charts & heatmap UX

Source: [viewer chart & heatmap UX](journal/2026-04-19-viewer-chart-ux.md).

- **Tick-label design review** — Open. X-axis tick formatting is inconsistent
  across chart types (`line.js` vs `heatmap.js` vs `histogram_heatmap.js`, which
  hard-codes `splitNumber: 5`) and across file vs live mode. The live-mode tick
  *overlap* observed 2026-06-21 is one visible symptom. Proper fix: a span-aware
  `minInterval` + width-bounded `splitNumber` cap + matching formatter, shared via
  `src/viewer/assets/lib/charts/util/`. *Reopen:* when fixing visible tick overlap
  or starting a chart-rendering quality pass.
- **Single `quantiles()` call for count/mean/percentiles** — Open. Count, mean and
  percentiles are all derivable from one `metriken quantiles()` call on one
  histogram column; the current #938 design emits separate `histogram_mean` /
  `histogram_count` / `histogram_quantiles` queries. Consolidating cuts parquet
  columns and query fan-out. Touches dashboard chart generation
  (`crates/dashboard/src/dashboard/*.rs`) and the viewer's histogram/percentile
  query paths. *Reopen:* when touching either.
- **In-chart label filtering** — Open. No way to hide series by label predicate
  (e.g. exclude `GPU=0`) or auto-hide flat/inactive series, so aggregates silently
  include dead series. *Reopen:* when working the chart toolbar, or after further
  "misleading average" reports.
- **Edit / delete existing event annotations** — Open. Event markers are read-only
  after creation in v1; changes go through `parquet annotate --add-events` /
  `--clear-events` outside the viewer. *Reopen:* if a `/events` management UI is
  requested.

## Viewer — selection / notebook / report

Source: [Selection → Notebook → Report](journal/2026-05-10-selection-notebook-report.md).

- **Customizable report title + browser tab title** — Open. Multiple report tabs
  are indistinguishable. Add a user-settable title persisted in the payload (same
  additive approach as `tagline`) and set `document.title` to
  `Report/Notebook[: <title>]` on those routes. No schema change; belongs with the
  `titleOverride`/preamble machinery in `selection/selection.js`.
- **Row / time trim on Save-as-Report** (`trim_range_ms`) — Open. The frontend
  already sends the field; the server ignores it (PR4 non-goal). Separate PR when
  file-size reduction by time range is needed.
- **Live-mode trim** — Open. `save_with_selection` in live mode converts msgpack
  snapshots to parquet at save time and skips the trim path.
- **De-duplicate `report_save` trim logic** — Open (cleanup). `crates/viewer/src/report_save.rs`
  is a parallel copy of `src/viewer/report_save.rs` over `Bytes`. Fold into a
  shared workspace crate if the surface grows past ~150 lines.
- **Report schema-drift guard** — Open. If a report's notes are re-applied against
  the wrong parquet, nothing warns. Add optional `baseline_checksum` /
  `experiment_checksum` to the v3 payload and show a banner on mismatch (warn,
  don't refuse to render).

## Viewer — display-mode decimation

Source: [display-mode decimation](journal/2026-07-13-viewer-display-decimation.md) (PR #1006).

- **A/B compare-mode line-envelopes + divergence band** — Done (PR #1006).
  Per-capture min/max envelope (thin capture-colored lines) plus a neutral
  gap-shading **divergence band** between the two medians. Browser-verified in
  file compare mode across gauges, counters, and percentiles (2026-07-15); the
  validation pass fixed four overlay/color/grid-alignment bugs — see the journal.
- **Cache headers on the viewer's JS assets** — Done. `routes.rs` `lib`/`index` now
  send an ETag (byte hash) + `Cache-Control: no-cache` and honor `If-None-Match`
  with a `304`, so refreshes revalidate and never load a stale/mixed module set.
- **`reloadCurrentSection` client-only-route guard** — Done. Skips the server
  section reload for client-only `source/` routes (`app.js`), killing the
  per-selection 404 + console error.
- **Live mock-agent + synthetic-live** — Open (manual eyeball of live mode done
  2026-07-15). Automating it still needs a mock server replaying synthetic msgpack
  snapshots. Pairs with a decision on the default live window (bounded rolling vs
  full history) and in-memory TSDB retention.
- **Automated browser testing** — Idea. Drive the viewer headless (Chrome CDP) and
  assert rendered chart options; the synthetic generator + scriptable viewer make
  it tractable. A WASM-runtime parity test (server vs WASM display bytes for a
  fixture) is the specific gap the `viewer-parity` skill calls for.
- **`crates/viewer/build.sh` wasm-pack flag conflict** — Done (PR #1007). Was
  passing `--profile wasm-release` while wasm-pack 0.13.1 also adds `--release`.
- **Reopen conditions for the 5 measured NO-GOs** (strided-median read, cumulative
  histogram quantiles, decode worker, aggregation worker, M4) live in the journal
  entry — don't re-litigate without the stated trigger.
- **Mean vs. median for the decimated line** — Open (discussion). Source:
  [mean vs. median](journal/2026-07-21-decimation-mean-vs-median.md). Median
  line is deliberate (robust typical level; envelope carries extremes) but
  forfeits conservation (`mean × width = Σ samples`); leaning is to carry a
  per-bucket mean in the display wire and surface it in the tooltip. Concrete
  sub-items regardless of outcome: a "(median)" qualifier on the tooltip value,
  and verifying notebook/compare stats recompute from raw queries rather than
  decimated medians.
- **Band views + budget policy redesign** — Open (design landed, pre-build).
  Source: [band views](journal/2026-07-21-viewer-band-views.md). Three decided
  pieces: (1) split spread ("what happened") from measurement ("what we can
  claim") into distinct chart views, never overlaid; (2) budget policy
  `native ≤ px/5 → raw, else min(px, ⌈native/5⌉)` honest buckets — fixes the
  48-floor-as-cap bug and the stale "<4 min → native" doc claim
  (element-gating is the recorded fallback); (3) interval-hull worst-case
  envelope `[min(lo_i), max(hi_i)]` in the measurement view (possibility, not
  observation — needs its own visual voice). Open: view-toggle scope
  (per-chart + sticky global default is the leaning). End-state: unsnapped
  timestamps contract the measurement view into an exception surface —
  *partially landed* via #1023 (Aligned/Raw Time modes, metriken-query
  0.16.0); new open question is the measurement view's relationship to that
  Time-mode control (see the entry's 2026-07-25 update).

## Viewer — performance / live mode

Source: [viewer performance & JS restructure](journal/2026-04-18-viewer-perf-restructure.md).

- **`LazySectionStore` never invalidates in live mode** — Open (known bug).
  `get_or_generate` (`src/viewer/state.rs:66–82`) memoizes section bodies; the
  cache is only cleared by replacing the whole store (startup/upload/connect/
  regenerate), never during the live ingest loop. Low impact today (section
  *structure* rarely changes mid-session; chart *data* bypasses the cache via live
  PromQL). Fix: a per-route bypass in `routes.rs` keyed on `state.live`, with a
  `generate_fresh` that doesn't write into `cached_bodies`. *Reopen:* when
  addressing the live-view no-update bug or if section structure observably freezes
  mid-session.

## Viewer — simple capture

Source: [simple-capture viewer](journal/2026-07-03-simple-capture-viewer.md).

- **Combined-file per-source isolation** — Open. A combined Rezolus+foreign file
  shares one merged TSDB, so a foreign source's fingerprint can bleed and it falls
  back to Query-Explorer-only (pre-feature behavior; no crash, single-source path
  unaffected). Needs per-column `source` metadata for per-source metric routing.
  *Reopen:* when combined simple-captures are needed.
- **Minor cleanups** — Open (non-blocking). Redundant clone/read in the metrics
  handler; an `assemble_catalog` loop unroll.
- **Jitter distribution side panels (CDF + PDF)** — Idea. Beside the timestamp
  jitter chart (to its right; stacked below on very narrow screens), summarize
  the *selected time range* with two small distribution plots: a CDF of the
  inter-sample interval and a probability-density plot of the jitter (deviation
  from nominal). Complements the timeline — it shows *when* cadence degraded;
  the distributions show *how much* and how often (tail behavior, bimodality
  from a stalled sampling loop). Must re-derive from the zoom selection, not
  the full recording.
  Related caveat, deliberately parked: the jitter timeline bypasses display-mode
  decimation (`promql_query: null`) and renders through echarts LTTB, which has
  no min/max envelope guarantee — an isolated spike can vanish at wide zoom
  (~47 samples/px on a 28k-point recording at 600px). Client-side boxplot
  bucketing of the deltas would fix it, but distributions may make it moot
  (tail mass shows the spike regardless of timeline rendering). *Decision
  2026-07-21: build the distributions first; add timeline bucketing only if
  interpretability is still lacking.*

## Parquet / recorder

Source: [per-source descriptions](journal/2026-07-04-per-source-descriptions.md).

- **Backfill descriptions on a parquet lacking `# HELP`** — Open. A
  Prometheus capture whose exporter emits no `# HELP` has blank descriptions
  (nothing to harvest at record time). A `parquet annotate --descriptions name=text`
  path could backfill the footer `descriptions` key after the fact. *Reopen:* if
  blank-description foreign captures become a recurring annoyance.
  **Second motivation (2026-08-13):** `parquet convert` gave raw recordings the
  same gap, and harder — `annotate` already takes `--systeminfo`, so a converted
  file can get its hardware summary back, but descriptions have no annotate route
  at all. The only recovery is a full `--force` reconvert of the original raw
  input, which is a poor trade for one footer key. Making `annotate` accept
  `--descriptions` (file or `name=text`) closes both cases at once; scoped as its
  own PR, deliberately kept out of the `convert` change.
- Per-source-not-per-node descriptions, and "descriptions only exist if the origin
  supplied them," are **by design** — not backlog.

Source: [streaming segmented `.rez` writer](journal/2026-08-11-rez-streaming-writer.md)
(implemented + measured 2026-08-12; finalize 19.6–37.1 ms independent of
recording length, 55 ms under backpressure).

- **Fleet-scale size cost of segmentation** — Open. The Linux fleet
  re-measurement (2026-08-12) covered finalize, kill recovery, cadence and the
  read path, but not size: the +1.28 % overhead figure is still macOS-only,
  from a bespoke replay harness. *Reopen:* before quoting a fleet size
  overhead. Related: `syscall_latency` reached 144 segments in 900 s.
## Acquisition-window sidecars

Source: [window sidecar cost](journal/2026-08-17-window-sidecar-cost.md)
(design, pre-build; every figure measured on a 32-core host).

- **Emit sidecars only for metrics that have a window** — Open, recorder-side,
  lossless, **lands on its own**. `rez.rs:262` pushes both sidecar fields
  unconditionally, but only 413 of `cpu_usage`'s ~2,068 metrics have a window,
  so **3,310 of its 6,206 columns are all-null**; six tables (`cpu_perf`,
  `cpu_bandwidth`, `cpu_frequency`, `cpu_l3`, `cpu_dtlb`, `cpu_branch`) are
  windowless entirely and pay 3×. Worth 2.1× on `cpu_usage`, 2.8× on
  `syscall_counts`. Settle first: a reader must treat an absent sidecar as it
  treats an all-null one (`metriken-query` pairing logic).
- **Window the region read, not each entry** — Open, agent-side. One `begin`
  and one `end` per map read, which is what an acquisition is; `cpu_usage`'s
  window columns 826 → 6. Honestly ~1.75× wider typical windows (12–37 µs →
  21–65 µs), which is 0.004% → 0.0065% of a 1 s scrape — negligible where it
  lands. The real gain besides columns: an entry's `end` currently records
  where the sweep reached it, a property of our loop rather than of the
  observation.
- **Bound the counter sweep to *possible* CPUs** — Open, agent-side,
  independent of the above. `src/agent/bpf/counters.rs:157` sweeps
  `0..MAX_CPUS` with `MAX_CPUS = 1024` (`src/agent/mod.rs:50`)
  unconditionally, walking 992 empty slots on a 32-core host every refresh.
  Bound by `/sys/devices/system/cpu/possible`, never by online, or a
  hotplugged CPU is missed.
- **Measure the per-entry clock-read cost** — Open, and it gates any
  performance claim for the two above. The obvious estimate (8,192 iterations
  in a 65 µs group span) implies 8 ns/iteration, below one vDSO
  `clock_gettime`, so it cannot be right: either the loop is shorter than
  modelled or the window closes before the sweep ends. `perf` on the agent.

## Recorder resource footprint

Source: [Recorder resource footprint — seal cost and peak RSS](journal/2026-08-13-recorder-resource-footprint.md)
(peak RSS 843 → 189 MB; seal policy retuned).

- **Per-metric `:window_*` sidecars triple every table's column count** —
  **Diagnosed**, and the cause is not the redundancy this item assumed. See
  [window sidecar cost](journal/2026-08-17-window-sidecar-cost.md); the three
  proposals below replace it.
- **WAL-sourced seals — drop `TableBuilder`** — **Done.** Sealing replays the
  live WAL instead of encoding a parallel builder, so a tick's values are
  written once. Peak RSS −40% (192 → 115 MB), process CPU −23%, and **dropped
  ticks 8.9% → 0.4%** at a 50 ms cadence — the per-tick saving was not
  headroom, the loop had been losing about one tick in eleven to its own
  bookkeeping. Archive grows 6.4% only because it holds the recovered ticks;
  per row it is slightly smaller.
- **Recorder and hindsight have no self-metrics** — Open. Neither registers a
  single metric, so hindsight's own footprint is invisible on every fleet host
  it runs on. Natural shape is a per-sampler table at the recorder's own cadence.
- **`rezolus_memory_usage_resident_set_size` reports a peak, not current RSS** —
  Open, and a defect in a shipped metric. It is fed from `ru_maxrss`
  (`src/agent/samplers/rezolus/rusage/mod.rs`), a monotonic high-water mark,
  under a name and description ("The total amount of memory allocated by
  Rezolus") that read as current usage — so it can never decrease. Either point
  it at `/proc/self/statm` or rename it to say high-water.
- **Flaky test** — Open.
  `hindsight::buffer::tests::at_retention_bound_flips_once_the_recording_outlasts_the_lookback`
  fails ~1 run in 6. Deterministic inputs, so the race is likely the writer
  thread's channel not being drained before the later assertions.
- **`WRITER_CACHE_SIZE_KIB` sized, not fitted** — Open, low value. 16 MiB is
  reasoned from `SealPolicy::max_bytes`; the knee between 2 and 256 MiB is
  un-swept.

## `.rez` v3 — SQLite container

Source: [`.rez` v3 — SQLite container with a real WAL](journal/2026-08-12-rez-sqlite-container.md)
(design landed 2026-08-12, `f0d58a74`; both gating measurements passed).

- **Adopt a target-encoded-size cap** — Open, **lower urgency after the 2026-08-13
  footprint work**: segment count drives read cost (the compactor's problem) and
  does not affect peak RSS, which is set by column count. A single global
  *in-memory* `max_bytes` is mismatched at both ends: it makes `syscall_latency` emit 190
  segments of 0.63 MB (7.6× past the ~25/table guidance) while letting
  `cpu_usage` emit 6.23 MiB ones, because the compression ratio spans 1.32:1 to
  62:1. Now that the per-table ratio is measured and stable within ±5%, a cap of
  *target encoded size × an EWMA of the observed ratio* fixes both ends.
- **`page_size` untested** — Open. Left at the 4096 default through the gating
  measurements; larger pages would shorten overflow chains for multi-MB BLOBs.
  Un-optimized, not chosen.
- **`-wal` sidecar footprint** — Open. Reaches 24–79 MB depending on
  `wal_autocheckpoint` and persists at its high-water size; must be counted in
  hindsight's footprint or capped via `journal_size_limit` plus a checkpoint at
  finalize. The default autocheckpoint (1000 pages) measured best for tail
  latency.
- **High-water-mark file growth** — By design, mitigated. SQLite never returns
  freed pages to the OS, so a transient volume spike inflates a hindsight file
  permanently (measured 16.0× when the working set shrank 16×).
  `auto_vacuum=INCREMENTAL` at creation is adopted to defend this; it is free in
  steady state and **cannot be enabled later** without a full `VACUUM`.
- **Hindsight migration to segments** — Roadmap. Retires the 4 KB slot ring
  (`src/hindsight/state.rs`) and the separate dump-to-parquet path
  (`src/hindsight/mod.rs:316`); dump becomes a consistent read or `VACUUM INTO`.
- **v2 tar → v3 conversion tool** — Open (on demand). Reading v2 stays
  supported; a converter is only needed to bring old recordings forward.

- **Per-table kill-loss for low-volume tables** — Open. At fleet scale a quiet
  table seals every 180–300 s, so an unclean kill can lose its whole recording
  while busy tables lose seconds (measured: 16 of 26 tables recovered nothing
  from a 120 s run). Correct by policy; a WAL covering the unsealed tail would
  close it.
- **`record` cannot write a multi-recording `.rez`** — **DONE** (this PR).
  Source: [multi-endpoint `.rez`](journal/2026-08-28-multi-endpoint-rez-record.md).
  Everything else in the stack is multi-recording — the manifest is a
  `Vec<RezRecording>`, the SQLite schema keys on `(recording_id, sampler, seq)`,
  the reader enumerates N, and `combine` assembles N offline — but
  `src/recorder/mod.rs:842` demotes to parquet when `endpoints.len() > 1`. So
  multi-host and A/B capture are offline-only, and two arms recorded
  sequentially differ in load as well as in the experiment. *Also fixes:* the
  seal stagger hashes the sampler name alone (`seal_policy.rs:164`), so two
  agents with identical sampler sets would seal in permanent lockstep — the key
  must widen to the sampler plus the recording's canonical label set (not the
  recording id, which would make segmentation depend on endpoint order).
- **Prometheus sources inside `.rez`** — **DONE**, and it did improve
  measurement honesty rather than compromise it. `PrometheusConverter` emits
  `SnapshotV3` with one group per target (`prometheus/scrape` — the slash is
  load-bearing, since `.rez` dispatches a table's WAL rows on it), windowed by
  the real HTTP round trip now that `scrape_one` returns the request and
  response instants. Both refusals are gone; only `--separate` still demotes
  the format. Schema members are ordered by assigned metric id, sorted
  numerically, so the schema changes only when the metric set does. The parquet
  path is unchanged and a test pins that on our side of the
  `metriken-exposition` boundary rather than trusting its contract. Fell out of
  it: `infer_source_name` used host and port alone, so two exporters behind one
  address (`/metrics` and `/federate`) inferred the SAME source and became
  recordings nothing could tell apart — the path joins the name now.
  *Considered and declined:* widening the window with the exporter's own
  embedded timestamp to account for a caching exporter. It is on the EXPORTER's
  clock while the window is on ours, so folding it in re-introduces the
  two-clocks-in-one-interval mixing the embedded-timestamp entry below removed
  — and the failure is silent and unmeasurable from here: an exporter five
  minutes slow inflates every band by five minutes, indistinguishable from a
  value genuinely five minutes stale. Recording the observed staleness
  (`response_received - embedded_ts`) as a SEPARATE signal would detect caching
  exporters without contaminating a quantity that describes our clock; judged
  not worth the design step it needs. The round-trip window therefore stays a
  lower bound on uncertainty — which a zero-width window also was, and far
  worse.
  *Original analysis, kept because it is what the work followed:* A scrape is
  one acquisition —
  one request, one response — so it models naturally as one acquisition group
  per target with the window set to the real HTTP round-trip. Today the
  Prometheus path already emits windows (`prometheus.rs:335`) but they are
  `Window::new(ns, ns)`, **zero width**: a whole scrape asserted to have been
  read at an instant, which is exactly what
  [all-sampler observation windows](journal/2026-07-10-all-sampler-observation-windows.md)
  calls the lie the arc kills. The writer already ingests `Snapshot::V2` (what
  `PrometheusConverter` emits) via `group_by_sampler`, so the `.rez` refusal at
  `src/recorder/mod.rs:836` is a policy check, not a capability limit. *Needs:*
  `PrometheusConverter` emitting `SnapshotV3` with **one group per target**
  rather than `SnapshotV2` — the V1/V2 branch of `write_table_parquet`
  (`rez.rs:305-328`) emits `<name>:window_begin`/`<name>:window_width` per value
  column, so routing V2 in unchanged would **triple the schema width** of every
  Prometheus table, which is exactly the cost acquisition groups removed. Also
  needs the HTTP round-trip pair plumbed to the converter (it is handed only
  parsed text and one `fetch_ns`, so the request instant never reaches it), the
  embedded line timestamp dropped as a window source (see the bug below), a
  table key for a source with no `sampler` label, and an honesty review of the
  caching-exporter case (a round-trip window under-states if the exporter serves
  stale values — still better than zero width). *Supersedes* an earlier
  by-design ruling in the multi-endpoint entry, which was wrong.
- **Prometheus embedded timestamps become epoch-anchored windows** — **DONE**.
  Was a live correctness bug on the shipping parquet path, not just a `.rez`
  concern.
  Prometheus exposition allows an optional trailing timestamp in *milliseconds
  since epoch*, intended as a federation/pushgateway staleness marker.
  `convert` passes `fetch_ns` to `Scrape::parse_at` as the default, so
  `sample.timestamp` is the embedded value when present and the fetch instant
  otherwise — two semantics silently mixed within one recording. The embedded
  value is then taken at face value by `sample_window`
  (`src/recorder/prometheus.rs:335`): the existing test
  `embedded_timestamp_becomes_window` asserts `m_total 3 1000` yields
  `begin_ns == 1_000_000_000`, a window beginning **one second after the Unix
  epoch** — decades before the recording holding it. Any exporter that emits
  timestamps (pushgateway, federation) writes that today, and the window offset
  is stored relative to the row timestamp, so the resulting `rate()` uncertainty
  band is nonsense rather than merely wide. Fixed by deriving the window from `fetch_ns` — the recorder's own
  clock for the tick — and ignoring `sample.timestamp` entirely, which also
  ends the two-semantics mixing. The remaining zero width is **fixed too**:
  `scrape_one` returns the request and response instants, so the window is now
  the real round-trip `[request_sent, response_received]` — a widening of this
  window rather than a change of its anchor, as predicted.
- **Viewer shows only the first two recordings** — **PARTLY DONE**. `rezolus
  view` now takes `--baseline k=v` / `--experiment k=v` (repeatable, ANDed,
  subset match — the same selector semantics as the MCP `--recording` flag,
  sharing `src/mcp/recording_selector.rs`), so which two arms of a 3+-recording
  `.rez` fill the A/B slots is a choice rather than manifest order. Each flag
  must name exactly one recording; matching none or several is refused with a
  listing, never narrowed to a first match. The default with no flags is
  unchanged (recordings 0 and 1, warning improved to list the archive). What
  remains is genuine N-way faceting — showing more than two arms at once —
  which is the "N-way compare (N > 2)" entry above: it needs the wire-stable
  `baseline`/`experiment` capture ids to become open-ended, which is UI work,
  not selection work.
- **Reopening a table can panic on a live archive** — **DONE**. Was open,
  `SamplerReader::reader` (`crates/rez/src/reader.rs:229-233`) reopens a table's
  segments with `.expect("segments opened at probe time cannot fail to
  reopen")`. That holds for a finished archive but not a live one: a `.rez` is
  readable while it is written, and `table_segments` returns sealed segments
  plus the materialized WAL tail, so a table that had rows at probe time can
  have none at reopen. The plausible production sequence is hindsight
  retention — `evict_before` drops everything older than the cutoff, and a
  quiet sampler's only rows can go between the two reads — leaving the viewer
  or MCP panicking rather than erroring. Surfaced while fixing a test that
  raced the writer; the test's own cause was different (an unjoined writer),
  but the assumption is unsound for the live-read case the format advertises.
  Fixed by making `SamplerReader::reader` return `Option<&TableReader>`: a
  table whose segments have gone is reported absent, which is what a table with
  no rows already is, so a query naming only its metrics gets the ordinary
  "references no metric present" error and its neighbours keep answering.
- **WASM viewer cannot open `.rez` at all** — **DONE**. The static-site viewer
  reads both containers now. What it took: the format moved out of the
  binary-only `rezolus` crate into `crates/rez` (nothing could depend on it
  where it was), the read path was decoupled from `metriken` (whose registry
  declares a `linkme` distributed slice, which has no wasm32 implementation),
  and the reader gained a byte-based entry point — a browser has an upload, not
  a path. A 2-recording archive maps onto the A/B slots exactly as `rezolus
  view` maps it; above two, the rest are named in a notice rather than dropped.
  Costs ~1.5 MB of bundle (SQLite), 4.5 → 6.1 MB raw, 1.4 → 2.0 MB gzipped.
  Fell out of it: a plain copy of a live archive was losing everything SQLite
  had not checkpointed — 123 ticks (~2 min) on a 2000-tick fixture, unbounded
  for a slow recording — so the writer now checkpoints on a 10s timer as well
  as at 4 MiB, and `rezolus recording snapshot` takes an exact copy.
  Still open around it: Save as Report is parquet-only, so a capture opened
  from an archive reports that it cannot be saved (`Viewer::can_save`), and
  there is no in-browser picker for which arms of a 3+-recording archive to
  show — the CLI has `--baseline`/`--experiment` for that.
- **Recovered-archive state not surfaced to consumers** — Open, one consumer
  closed. `RezReader` warns and `parquet metadata` reports "not cleanly
  finalized", but the viewer API and MCP output don't, so a truncated recording
  can be analyzed silently. The WASM viewer now says it — `RezReader::complete()`
  carries the flag, and `WasmCaptureRegistry::notices()` reports it — because
  that consumer has an extra way to read short: a browser is handed one file,
  and SQLite's `-wal` sidecar (pages committed but not yet checkpointed into the
  archive) is a separate one. `rezolus view archive.rez` opens by path with the
  sidecar beside it and sees further. Still open: the server viewer's API and
  MCP output, which have the flag available and do not report it.
- **Unbounded startup probe** — Open. `probe_endpoint`/`fetch_agent_metadata`
  have no timeout; a hung (SIGSTOPed) agent hangs `rezolus record` at startup
  and the first ctrl-c doesn't break out. D2 bounded only the per-tick path.
  The trade differs at startup (too tight aborts the recording rather than
  skipping a sample).
- **Manifest resolution is O(archive bytes)** — By design, worth knowing. The
  authoritative manifest is the last tar entry, so resolution scans the archive:
  sub-10 ms at production settings, 19.3 s on a pathological 18 GB /
  15k-segment archive. *Reopen:* if segment counts get pathological in practice
  (the compactor below is the real answer).

- **Offline `.rez` compactor** — Roadmap. Merge a segmented archive's per-table
  segments into single files offline (likely under `rezolus parquet`): recovers
  the compression ratio and per-segment footer overhead that streaming trades
  for durability, and its output is fully v1-readable (`file` + `files`,
  `version: 1`), making it the forward-compatibility downgrade path. With the
  segment-aware read path in scope, this is an optimization, not a read-speed
  requirement. Not needed for the streaming writer to ship.
- **Full metric-identity column keys in `.rez` tables** — Open. Column names
  are per-agent-process numeric ids, so an agent restart mid-recording remaps
  them; the merge policy splits conflicting columns. Keying on metric name +
  labels at write time would make restarts seamless (write-format change).
  *Reopen:* if restart-heavy recordings make split columns a real annoyance.
- **Seal thresholds as compile-time constants** — Open. Byte-first seal
  thresholds (est. bytes primary, row cap, ~5 min age bound for the kill-loss
  window) ship as constants in `crates/rez/src/rez.rs`. *Reopen:* if real
  workloads need tuning — promote to a `--flag` or config knob.
- **Fast finalize for the classic parquet path** — By design. The single-file
  parquet's wide schema is only knowable once recording ends, so its finalize
  replays the whole msgpack spool. *Reopen:* if a client needs `.parquet` output
  with fast stop — likely shape: record to `.rez`, convert offline.

## `.rez` v3 — read path

Source: [`.rez` v3 versus parquet on the read path](journal/2026-08-27-rez-vs-parquet-read-path.md).

- **~~Open only the tables a query touches~~** — **DONE.** The v3 read path no
  longer materializes the archive: routing matches a per-table name catalog
  (`metriken_query::referenced_metrics`, metriken#138), `time_range`/`interval`
  come from probed spans and `segment_span` (no BLOB), and a table's payload is
  fetched on first query via `SegmentSource::Db`. Measured 629 → 56 ms; `.rez` is
  now 0.74–0.84× parquet's query time at 50 ms and 1.00–1.19× at 1 s. No format
  change was needed.
- **The last fixed open cost is ~50 name probes** — Open, low priority. One
  footer per table at open builds the routing catalog. Caching per-table metric
  names in the SQLite catalog at write time would remove them; not needed to be
  competitive. *Reopen:* if table counts grow well past 50.
- **Share the parsed schema across a table's segments** — Open, metriken-query.
  `SegmentedParquetReader::open_bytes_with_pool` opens each segment and builds
  four identity indexes per segment per metric kind (~418 footer parses, ~1,670
  schema passes for one archive). A table's segments have identical schemas, which
  `schema_hash` already asserts — one pass per table would serve all of them.
- **Seal larger segments** — Open, and now the ONLY lever on the remaining axis:
  `.rez` is still 2.25× parquet's size at 50 ms. Segments average 218 KB against
  the 1.4 MiB the container was priced at, so compression cannot work across
  boundaries. **It is a trade, not a win** — `max_rows: 900` was chosen to cut
  finalize 1147.6 → 549.8 ms, and with query latency now ahead of parquet the
  trade is harder to justify than it looked. *Reopen:* if archive size becomes
  the binding constraint.
- **Read cost on a live/unsealed archive is unmeasured** — Open. Both benchmark
  arms were finalized; hindsight reads a buffer with a live WAL tail, which
  materializes differently. *Reopen:* measure alongside the first fix.

## Agent — drive health sampler

Source: [drive health sampler — Phase 1 (module-free)](journal/2026-07-06-drive-health-sampler.md).
Phase 1 (temperature) + NVMe thermal-throttle counters shipped in #992 via
read-only pass-through ioctls — SATA ATA PASS-THROUGH (`ata.rs`) and NVMe Get Log
Page 0x02 (`nvme.rs`) — no kernel module.

- **NVMe hardware validation** — Open. The NVMe path (temperature *and* the new
  `drive_thermal_throttle_*` / `drive_temperature_{warning,critical}_time`
  counters) is fixture-verified only; no NVMe drive was on the GO-check host.
  *Reopen:* confirm on a host with an NVMe drive (bonus: one that has actually
  throttled, to exercise nonzero counters).
- **Time-bounded / synchronous refresh** — Roadmap. `drivehealth` is the first
  sampler whose refresh isn't time-bounded to the snapshot (temperature gauge may
  be up to `interval` stale, unobservably). Intended fix: read inline on the
  sample cycle where the per-bus cost is *measured* affordable; async+throttle only
  for expensive reads, and there expose a read-age. *Gated on* measuring NVMe read
  cost on real hardware. See the journal's "async freshness" design note. (The
  throttle counters made this non-urgent — they're monotonic and cadence-robust.)
- **SAS (true SCSI) temperature** — Roadmap. SATA (incl. SATA-behind-SAS) ships via
  ATA pass-through; pure-SAS drives need SCSI LOG SENSE page 0x0D. Deferred — no
  SAS-only hardware to verify against.
- **Phase 2 — NVMe SMART-log health (remainder)** — Roadmap. Wear
  (`percentage_used`), available spare, critical-warning bits, media errors,
  power-on hours — extends the Phase-1 NVMe Get Log Page 0x02 read (`nvme.rs`). The
  *thermal-throttle* subset of Phase 2 already shipped in #992.
- **Phase 3 — ATA/SATA + SAS SMART attributes** — Roadmap. Vendor-specific
  attribute parsing (reallocated sectors, etc.) over the pass-through path
  (`ata.rs`).
- **SATA serial label** — Open. Phase 1 leaves `serial` empty for SATA (NVMe serial
  comes from sysfs); SATA serial via ATA IDENTIFY is deferred. *Reopen:* if stable
  SATA fleet identity is needed.
- **Hotplug discovery** — Open. Phase 1 discovers drives once at startup; drives
  added later are missed. *Reopen:* if hotplug matters.

## Agent — NVIDIA GPU sampler

Source: PR #1108 (Tegra placeholder gating), grounded in a measured Tegra
recording (single iGPU, 55 min at 1 s).

- **Video engine (NVENC/NVDEC) utilization** — Open, and the highest-value gap.
  NVML's `utilization_rates().gpu` covers only the SM/graphics engine; NVENC and
  NVDEC are separate fixed-function blocks it does not count. On a transcode-heavy
  workload the video engines can be saturated while `gpu_utilization` reads ~0, so
  the recording cannot explain what the GPU is doing. The measured Tegra recording
  shows exactly this shape: ~18.6 W drawn and a 49.6 °C → 61.5 °C thermal ramp
  while `gpu_utilization` is 0, with power *lowest* during the 91% SM plateau. We
  already record `gpu_clock{clock="video"}` — the engine's clock — but never its
  utilization. *Add:* `encoder_utilization()` / `decoder_utilization()`
  (`UtilizationInfo{utilization, sampling_period}`), and probably `encoder_stats()`
  (`session_count`, `average_fps`, `average_latency`). *Avoid:* `encoder_sessions()`
  — a per-session `Vec`, unbounded cardinality, wrong for an always-on fleetwide
  sampler. *Note:* `nvml-wrapper` 0.12.1 has no bindings for
  `nvmlDeviceGetJpgUtilization`/`GetOfaUtilization`, so NVJPEG and the optical-flow
  engine need raw FFI or a crate bump. Per principles 13/16/17 this is an NVML
  library call, not an mmap read: the effort must carry a *measured* per-refresh
  overhead number and a cadence decision. And per #1108's own lesson, verify on
  Tegra whether these return real values or placeholders before trusting them.
- **NVML utilization support is Tegra-generation-dependent** — Open, and the
  reason the gating in #1108 is deliberately narrow. NVIDIA's stated position is
  that NVML is not supported on Jetson (users are pointed at `tegrastats`), and
  there are Orin reports of NVML utilization not working; JetPack 7 / Thor
  release notes, by contrast, advertise newly-added NVML GPU monitoring. The
  measured recording behind #1108 shows `utilization_rates().gpu` working, so at
  least one generation populates it — but that is one host, and `utilization_rates`
  is documented only for "fully supported devices", a list Tegra iGPUs are not on.
  Consequence: on a generation where NVML does not populate it, the agent records
  a constant `0`, indistinguishable from a genuinely idle GPU. Note this is
  *main's existing behaviour*, not a regression from #1108 — that PR declined to
  gate `.gpu` rather than introducing the exposure. *Fix:* prefer the nvgpu
  driver's own load node (`/sys/devices/.../<addr>.gpu/load`, permille — the
  source `tegrastats` GR3D reads) as the `gpu_utilization` source when
  `is_tegra_soc()`, which works on every Tegra generation; it is one small sysfs
  read per tick, so per principles 13/16/17 it needs a *measured* per-refresh
  number. Failing that, stamp the SoC `compatible` string into snapshot metadata
  so a consumer can at least tell which generation produced a zero.
- **`GPU_ENERGY_CONSUMPTION` can publish a fabricated zero** — Open,
  pre-existing, adjacent to #1108 and the same bug class. It is a `CounterGroup`
  (`stats.rs`), and metriken's `CounterGroup::value()` has **no** sentinel: it
  returns `Some(0)` for an unwritten slot as soon as *any* index in that group
  has been written. So on a mixed multi-GPU host where `total_energy_consumption()`
  succeeds for device 0 and fails for device 1, device 1 publishes a constant-zero
  energy counter that reads as a real measurement. Cannot fire on a single-GPU
  host (the group stays uninitialised and reads `None`), which is why #1108's
  Tegra target does not hit it. Contrast `GaugeGroup`, which uses an `i64::MIN`
  sentinel and correctly yields `None`. *Fix:* track written membership
  explicitly, or move the metric to a gauge-like sentinel representation.
- **Per-device Tegra discrimination** — Open. `is_tegra_soc()` reads
  `/proc/device-tree/compatible`, a *host* property, but "no real PCIe link / no
  utilization counters" is a *device* property. A Tegra board carrying a discrete
  PCIe GPU (NVIDIA IGX Orin is `nvidia,tegra234`) would have that card's genuine
  `gpu_pcie_bandwidth` and `gpu_memory_utilization` suppressed. *Fix:* AND the
  host probe with a per-device discriminator (`Device::bus_type()` or
  `pci_info()`), stored as a `Vec<bool>` beside `gpm_supported`. *Gated on* access
  to a Tegra host with a discrete GPU — the discriminator's behaviour on a Tegra
  iGPU is unverified, and guessing risks breaking the fix on its target platform.
- **Tegra probe is invisible in containers** — Open. `/sys/firmware` is in runc's
  default masked-paths list, so a non-privileged container reads the device tree as
  absent and a genuine Tegra host is treated as not-Tegra (back to recording the
  placeholders). The documented deployments use `--privileged`, which disables
  masking, so the shipped path works; a Kubernetes pod granted only
  `CAP_BPF`/`CAP_PERFMON` does not. Nothing in the recording distinguishes
  "not Tegra" from "couldn't tell". *Fix:* subsumed by per-device discrimination
  above; failing that, make the probe three-valued and surface it in snapshot
  metadata (per the project's surface-errors-not-journald preference).
- **Gating is invisible to operators** — Open. On a Jetson an operator sees empty
  `gpu_pcie_bandwidth` / `gpu_memory_utilization` charts and cannot tell
  "deliberately not read" from "the sampler broke". `SamplerState::Unsupported`
  (`src/agent/sampler_status.rs`) is whole-sampler only; there is no per-metric
  equivalent today. *Reopen:* if per-metric status machinery lands.

## Agent — per-cgroup I/O attribution

Source: [per-cgroup attribution for block I/O and network samplers](journal/2026-06-16-cgroup-io-attribution.md).
Rezolus attributes CPU, scheduler, syscall and TLB activity per cgroup, but
`blockio_*` is labeled only by `op` and `network_traffic` only by `direction` —
so a hypervisor host cannot answer "which guest is doing this I/O?". Nothing is
built: `blockio/requests/mod.bpf.c` and `network/traffic/mod.bpf.c` contain no
cgroup references.

- **`cgroup_blockio_operations` / `cgroup_blockio_bytes`** — Open, and first.
  Cleanest attribution story: completion runs in IRQ/softirq context so
  `bpf_get_current_cgroup_id()` is wrong; the issuing cgroup comes off the
  request as `rq → bio → bi_blkg → blkcg → css.id`, a CO-RE read against types
  already in the checked-in `vmlinux.h`. Add the counters in `blockio/requests`
  only — `block_rq_complete` is already shared with `blockio/latency` (principle
  11, and the "Known drift" note in `docs/principles.md`).
- **`cgroup_network_bytes` / `_packets`, TX first** — Open, after blockio.
  `skb->sk` is populated for locally originated traffic at
  `net_dev_start_xmit`, so TX attributes; at `netif_receive_skb` it is typically
  NULL (pre-demux), so RX does not.
- **Network RX attribution** — Open, and the decision that gates the network
  work. Either accept TX-only at the device hook, or move attribution to the
  socket layer — which overlaps the existing `tcp/*` samplers and so becomes a
  cross-sampler consolidation (principle 11) covering only TCP.
- **Interface as the tenant proxy** — Open, and arguably the *primary* network
  approach rather than cgroup attribution. Each guest already gets a dedicated
  host-side netdev (tap/macvtap/VF representor/veth), `skb->dev` is populated on
  both hooks (solving RX), and the chain is 2 derefs rather than blockio's 4.
  Breaks on kernel-bypass datapaths (OVS-DPDK, vhost-user, SR-IOV passthrough
  without a representor), shared interfaces, and ifindex reuse. Prefer emitting
  interface-keyed metrics and joining `ifname → tenant` downstream (principle
  9). Note `network_interfaces` is global-only today, so this is a real addition
  rather than a relabel.
- **Per-cgroup blockio size histograms** — Deferred, deliberately.
  `MAX_CGROUPS × 496` H2 buckets is ~16 MB per op, ~64 MB for four; that breaks
  the bounded-memory discipline (principles 8, 13). *Reopen:* only with config
  gating or a sparse representation, as its own proposal.
- **Counter layout and metric naming** — Open (minor). Confirm `packed_counters`
  keyed by cgroup id, matching `cgroup_cpu_usage`; and prefer the distinct
  `cgroup_`-prefixed metric over adding a `cgroup` label to `blockio_*`.
- **Size the tax empirically, don't guess** — Open. Every sampler already exports
  `rezolus_bpf_run_time`/`rezolus_bpf_run_count`; take mean ns per invocation for
  `cpu_usage` as the fleetwide baseline, then measure `blockio_requests` /
  `network_traffic` before and after under fio / a packet generator (principle
  16). Per-event cost is roughly the cgroup tax `cpu/usage` already pays; the
  axis that actually differs is hook firing rate (Mpps / millions of IOPS vs
  tick accounting).

## Agent — blockio latency (rq-field method)

Source: [blockio latency — drop the map, read rq timestamps](journal/2026-09-03-blockio-latency-rq-fields.md).
The sampler now computes device/queue/total latency from `struct request`'s own
`start_time_ns`/`io_start_time_ns` at `block_rq_complete`, no side map. Deferred
items from that effort:

- **Requeue under load** — Open. `block_rq_requeue` is left unhooked; a requeued
  request measures device latency from its *last* dispatch (`io_start_time_ns` is
  re-stamped). Not stress-tested (0 requeues on the probe workloads). *Reopen:*
  if a requeue-heavy workload shows anomalous device tails.
- **True partial completions** — Open. Recording is gated on
  `nr_bytes == __data_len` (final completion) for stateless dedup; the
  `nr_bytes < __data_len` branch is unexercised because real partials need SCSI
  residual / specific drivers we could not force. *Reopen:* if a device known to
  do partial completions shows count inflation or low-biased percentiles.
- **Tag-allocation wait phase** — Idea. `alloc_time_ns` yields a fourth phase
  (`start_time_ns − alloc_time_ns`, the wait for a free request tag — a deeper
  saturation signal than queue wait) but is 0% populated without an active
  iocost/iolatency controller. *Reopen:* expose it where a controller is active.

## metriken — measurement uncertainty (arc)

Source: [measurement uncertainty](journal/2026-07-08-measurement-uncertainty.md).
Cross-cutting foundational arc, **temporal-first**: drop the unified-timestamp
myth (samplers sample at different instants, some with large intra-collection
spread), plot them together honestly, and put **error bars on rates**. Core lands
in metriken; rezolus is first consumer. Value-uncertainty is modeled but deferred
(except the counter increment quantum, needed for rate error bars). Phased.

- **Phase 1 — observation acquisition windows** — Specced, pre-build. See
  [Phase 1 spec](journal/2026-07-10-measurement-uncertainty-phase-1.md). Scoped to
  the **window (+ derived kind)** in the metriken *format* (exposition), with an
  optional additive per-index window store on groups; drivehealth captures
  per-device windows, visible on `/metrics/json`. `start_epoch` / quantum / HZ are
  deferred to Phase 3 (the shape is extensible for them); metriken-core read API
  stays general.
- **Phase 2 — archive + plot-together** — Roadmap. Common `.mtk`/`.rez` archive
  (tar of per-cohort parquet + manifest) + recorder + v2→v3 converter; viewer plots
  heterogeneous cohorts on one axis.
- **Phase 3 — rate error bars end-to-end** (headline) — Roadmap. TSDB carries
  windows+quantum+epoch; `rate()`/`increase()` return error bars; correlation
  ceiling in the viewer. May land on the live path before the archive.
- **Phase 4 — cross-host clock uncertainty** — Roadmap. NTP offset/frequency/root
  dispersion as a first-class term → honest cross-host correlation.
- **Phase 5 — fuller value uncertainty** (histogram percentile bounds, gauge
  precision) + statistical propagation + MCP confidence — Roadmap.
- **Open decisions** — interval-vs-statistical propagation math (pin before
  Phase 3/4); query back-compat for the error-bearing `rate()` return type;
  archive name + manifest schema; archive PII posture; metriken `next` branch vs
  hard-fork + no crates.io publish until migration is solid (a real cross-team
  gate).

## Agent — fentry migration for hot kprobe samplers

Source: [fentry vs kprobe dispatch](journal/2026-09-04-fentry-vs-kprobe-dispatch.md).
Measured: an fentry probe is ~61 ns/call (56%) cheaper than kprobe on a clean
`tcp_sendmsg` (kernel 6.12). 8 samplers still use kprobe:
`cpu/{tlb_flush,bandwidth,usage}`, `network/interfaces`,
`tcp/{traffic,receive,retransmit,connect_latency}`.

- **Add BTF-gated fentry twins to the hot single-hook samplers** — Open. fentry
  needs BTF, so it is a twin with the kprobe kept as the CO-RE-only fallback
  (principle 2), like the tp_btf/raw_tp pattern. Order by hook rate; `tcp_traffic`
  (`tcp_sendmsg`/`tcp_cleanup_rbuf`, per-message) first. Re-measure each on a
  clean function with `scripts/bench-fentry-vs-kprobe.sh` before/after.
- **Consolidated hooks are a separate case** — Open. kprobes sharing one function
  share a single ftrace dispatch (a second sampler is cheap incremental), while
  fentries each need a trampoline; the 61 ns standalone win does not transfer to
  a hook several samplers share (principle 11). Measure those separately before
  migrating.

## Tooling / skills

Source: [`document-feature` skill](journal/2026-07-02-document-feature-skill.md).

- **`document-feature` trigger-description optimizer** — Open (blocked). The
  skill-creator `run_loop.py` optimizer needs `ANTHROPIC_API_KEY` + the `anthropic`
  SDK; the `claude` CLI auth doesn't expose the key, so it couldn't run. The
  20-query eval set is bundled at `.claude/skills/document-feature/evals/trigger-evals.json`.
  *Reopen:* when an API key is available.
- The per-subcommand `--help` backlog (view/parquet/exporter/hindsight/agent/mcp)
  from #986 is **cleared** — applied across all subcommands in #987 and the backlog
  doc retired in #988. Kept here only as a pointer; not open.

## Desired future capabilities

Net-new instrumentation/feature ideas — mostly raised during the Exceptions
dashboard work (#873). Each notes *what* and *why it matters operationally*;
implementation is decided per item. These are **Idea**-state (not yet scoped to an
effort); promote one to a journal entry when it's picked up.

- **Hardirq instrumentation** — Idea. Per-CPU hardware-interrupt delivery rate,
  broken down by source (per-device IRQ, IPI, LAPIC timer). Rezolus tracks softirq
  cost per CPU but not hardirq. *Why:* on CPU-isolated hosts any hardirq on an
  isolated CPU is a misconfiguration; on VMs, IPI traffic pays a multiplied VMEXIT
  cost; the LAPIC-timer rate shows whether `nohz_full` actually quiets the tick.
- **Per-CPU block-IO completion distribution** — Idea. `blockio_operations`
  aggregates across CPUs; a per-CPU breakdown shows how completions spread across
  cores. *Why:* lopsided completion (one CPU draining most) signals IRQ-affinity
  misconfig on multi-queue devices — invisible today until tail latency spikes.
- **IO submitter→completer CPU correlation** — Idea. Directly measure the fraction
  of IOs that complete on a different CPU than they were submitted from. *Why:*
  verifies `rq_affinity`; cross-CPU completion routing costs cache/NUMA traffic on
  every IO, and there's no metric that confirms it's working.
- **Protocol-level IO error breakdown** — Idea. `blockio_errors` buckets
  `blk_status_t` into 7 coarse classes; go deeper into protocol codes (NVMe SCT/SC,
  SCSI sense keys) to distinguish Media Error vs Aborted-by-Host vs Capacity
  Exceeded. *Why:* the coarse classes say "is storage misbehaving"; protocol codes
  say "how" — triage without `dmesg` archaeology.
- **Per-cgroup off-CPU latency distribution** — Idea. `cgroup_scheduler_offcpu` is
  a counter (total ns blocked); a per-cgroup histogram distinguishes many-short
  blocks from few-long. *Why:* two cgroups with equal total off-CPU time can have
  very different tail latency — the shape is the diagnostic (long tail → lock/IO
  stalls; short-and-many → scheduler interleaving).
- **System-configuration visibility** — Idea. Surface boot/runtime config that sets
  performance posture: CPU isolation (`isolcpus`, `nohz_full`, cgroup `cpuset`),
  block tuning (IO scheduler, completion affinity, NVMe queue mode), IRQ affinity.
  *Why:* lets dashboards flag drift (e.g. a completion landing on an `isolcpus` CPU)
  and lets fleets compare intent vs reality at scale.
- **Streaming data adapter for embed-friendly charts** — Idea (partly shipped). The
  `<rezolus-chart>` web component + local WASM data adapter shipped in #915; the
  remaining piece is a server-streamed (SSE/Datastar) data adapter behind the same
  `Plot`/`View` descriptor + component API, for live data — plus a `<rezolus-section>`
  wrapper. *Why:* a clean split between the static file-mode viewer and a future
  streaming server viewer without forking the frontend.
- **One WAL commit (and fsync) per recording per tick** — **DONE**. A tick is
  staged per recording (`StreamRecorderV3::stage`) and committed once
  (`RezArchive::wal_tick` -> `RezDb::insert_wal_rows_batch`), so an archive
  pays one transaction — one fsync at `synchronous=FULL` — per tick however
  many endpoints it holds. `RezDb::commits()` makes that assertable: the test
  pins one commit for four recordings AND that all four recordings' rows are in
  it, since a count alone would be satisfied by a writer that committed once
  and dropped three. Mutation-checked against the old per-recording commit,
  which reads 4. A side benefit worth knowing: the tick is now atomic across
  recordings, so a crash cannot leave one endpoint's row for tick N present and
  another's missing. *Considered and declined:* moving to `synchronous=NORMAL`
  with an fsync on a uniform clock. It trades the documented "survives power
  loss, not merely process death" property for a win the existing measurement
  says is not where the tail is ("the tail is checkpoint and prune work, not
  fsync"), and it would leave the cost linear in endpoint count — N commits
  still cost N commit records and N sets of page writes, just without the
  fsyncs. Coalescing fixes the linearity at its root and keeps durability.
  *Original entry:* found reviewing
  multi-endpoint `.rez` (#1109). `writer_loop` handles one `Msg::Wal` per
  recording per tick, and `RezDb::insert_wal_rows` is one transaction — one
  fsync at `synchronous=FULL`. An archive used to be one recording, so a tick
  was one commit; N recordings make it N. This is the same cost `seal_batch`
  already refuses to pay ("12 implicit commits would be 12 fsyncs at
  `synchronous=FULL` against a ~46 ms tick"), and the argument was not carried
  across recordings. It lands on the scrape loop: `RecordingWriter::wal` is a
  blocking send on a bound-1 channel from inside the tick. Measured runs show
  zero dropped ticks at 1 s and at 50 ms with two recordings, so this is not
  urgent — but it scales linearly with endpoint count and the documented
  multi-host example is the case that grows it. *Fix:* batch the tick — one
  `Msg` carrying every recording's rows, committed in a single transaction, or
  a `TickBegin`/`TickEnd` bracket. The fan-out point already exists in
  `RezStream::ingest`. *Why:* the format's claim is bounded, predictable write
  cost; per-tick fsyncs scaling with endpoint count quietly erodes it.
  *Related, and NOT the same axis:* the writer also checkpoints the WAL on a
  10s timer (`rez_v3_writer::CHECKPOINT_INTERVAL`) so a plain copy of a live
  archive cannot fall arbitrarily far behind. That is one fsync per 10s on the
  writer thread, independent of tick rate or endpoint count — anyone measuring
  the tick path should know it exists, and should not confuse it with the
  per-tick commits above.
- **A `--recording` selector for the MCP tools** — **DONE** (this PR), the real
  fix behind the refusal added in #1109. `RezReader::open_with_pool` flattens
  every recording into one view, so a multi-recording archive gives each
  sampler two owners and `route()` refuses every query as cross-recording.
  `mcp open_source` now refuses such an archive up front with a message,
  because the analysis tools fold a per-metric query error into `NoData` and
  would otherwise report "analyzed N metrics, found anomalies in 0" — a
  clean-looking wrong answer. That is honest but not useful:
  `record --endpoint a --endpoint b -o out.rez` is now a documented, ordinary
  capture, and no MCP tool can read one. Shipped: `--recording key=value`
  (repeatable, ANDed) on all six `mcp` subcommands, and an equivalent optional
  `recording` object on the stdio server's six tools, resolved by
  `RecordingSelector` against `RezReader::open_recordings`; it must name
  exactly one recording, and matching none or several is an error listing the
  candidates.
- **The seal stagger still aliases on bit 5 (ASCII case)** — **DONE**, and not
  by changing the hash. Revisited with numbers, as this entry asked. The
  measurement overturned the proposed fix: **the spread and the alias are the
  same property** — the low-bit affine structure that makes this hash spread a
  real sampler set PERFECTLY is exactly what the alias exploits. Colliding
  sampler-pairs normalised by a uniform random assignment (0 = perfect, 1.0 =
  random), over 500 recording keys:
  | candidate | 12 samplers | 26 samplers | alias |
  |---|---|---|---|
  | shipping hash | 0.000 | 0.394 | total lockstep |
  | + absorb `b >> 5` (this entry's fix) | 1.939 | 1.378 | **8/26 — not closed** |
  | + fold `b>>5 ^ b>>6` | 1.939 | 1.182 | 1/26 |
  | reduce from top bits | 0.981 | 1.002 | closed |
  Every candidate that closes it lands at or worse than random, and the
  proposed `b >> 5` fold does not even close it. So the hash stands and the
  situation is DETECTED instead: `seal_policy::staggers_identically` states the
  condition exactly (differ only in bit 5, an even number of times — not a
  case-insensitive compare, which would cry wolf on the odd-count pairs that
  stagger fine) and the recorder warns at startup, beside the existing
  identical-labels warning. A test pins the shortcut against `stagger_bucket`
  itself, since a drifted shortcut would warn about safe pairs or stay silent
  on lockstep with nothing else noticing. The spread numbers are pinned too, so
  a future hash change has to answer for them.
  *Original entry:* found in the
  second review pass on #1109. The first pass closed bits 6-7 of every absorbed
  byte, but the same algebra survives one bit lower: `x ^ 0x20` is
  `x + 32 (mod 64)` and `51 * 32 == 32 (mod 64)`, so flipping bit 5 XORs 0x20
  through the whole chain. Two recording keys differing by an **even** number
  of bit-5 flips share a bucket for *every* sampler — measured 12/12 for
  `host=Web-01` vs `host=weB-01`, and for `arm=valkey` vs `arm=VALKEY` (6
  letters); an odd count, like `redis`/`REDIS`, does not collide. In printable
  ASCII bit 5 is the case bit, so this needs two recordings whose labels differ
  only in capitalisation — within one `record` run that means an operator
  typing two `source=` values that differ only in case, which is unlikely but
  not impossible. *Fix:* absorb `b >> 5` as a third pass, or any XOR-shift
  finalizer before the reduction. *Why not already:* each extra fold costs some
  of the low-bit structure that measures better than random here (0.144 vs
  0.188 for 12 samplers), and this class is far narrower than the one closed —
  so it is a deliberate trade to revisit with numbers, not an oversight.
- **`RezReader::open_with_pool` has no production caller** — Open, observed
  while fixing #1109. The viewer opens recordings individually, and `mcp` now
  does too, because flattening a multi-recording archive gives every sampler
  two owners and makes `route` refuse every query. The flattening entry point
  is now exercised only by the cross-recording regression tests that pin that
  refusal. *Fix:* either delete it and rewrite those tests against
  `open_recordings`, or keep it and say in one place that flattening is a
  test-only shape. *Why:* a `pub` constructor with no caller is the kind of
  thing a future consumer reaches for by name and then inherits the refusal
  from — the reason `mcp` had to grow an explicit guard at all.
- **A failed `.rez` creation can leave a file that blocks the retry** — **DONE**.
  Both remaining windows closed: `RezDb::create` removes what it claimed if
  anything after the claim fails, and `RezArchive::create` removes it if the
  thread spawn fails. `RezStream::discard` and both of those go through one
  `RezDb::remove_archive`, which takes the `-wal`/`-shm` sidecars with the
  archive — a stray sidecar is WORSE than a stray main file, since `O_EXCL`
  catches the main file and says so while a sidecar beside a newly-created
  database is adopted silently and its frames replayed in. Also pinned: a
  cleanly finalized archive leaves exactly one file, which is what makes
  "copy it, ship it, upload it" sound advice. The old "a spawn failure leaves
  a valid empty recording" rationale was true and useless — valid is not
  useful when it holds nothing and blocks the retry.
  *Original entry:* pre-existing, surfaced twice while reviewing #1109. `RezDb::create` claims the
  output path with `O_EXCL`, and the writer refuses to overwrite an existing
  `.rez` — which is the right default for a container committed as it goes, but
  it means anything left behind by a failed start blocks the re-run until the
  operator removes it by hand. Two windows remain: (a) `RezArchive::create`
  failing *after* `RezDb::create` succeeded — the pragma steps or the thread
  spawn — returns `Err` without unlinking; (b) `RezStream::discard` removes the
  main file but not the `-wal`/`-shm` sidecars, which SQLite normally cleans on
  a clean close but not after an unclean one. #1109 fixed the third window (a
  partial `start_rez_recorder`, which now calls `discard()`), so these are what
  is left of the family. *Fix:* have `RezArchive::create` unlink on its own
  failure path, and have `discard` remove the sidecars alongside the main file.
  *Why:* the failure mode is "the retry says the file already exists", which
  reads as a bug in the retry rather than fallout from the original error.
- **Selector output is not shell-escaped, and the cache identity can alias** —
  **DONE**. Both closed. (a) `picker_form`'s flag branch now runs each `k=v`
  through `shell_word`, POSIX single-quoting any token that is not already
  shell-safe — so `select with: --recording note='first run'` survives being
  pasted, and a value containing the literal ` --recording ` stays one word
  instead of parsing back as a duplicate key. Single-quote style because it is
  total: inside `'...'` the only escape needed is `'` itself. The round-trip
  tests now shell-split the rendered line with `shlex` (a dev-dependency, a
  DIFFERENT implementation from the quoter) before parsing, so a quoting bug
  surfaces as a wrong word count rather than passing on a shared mistake;
  mutation-checked by reverting to the raw join, which fails them. (b) The
  server's post-open identity dedup keys on the label `BTreeMap` itself, not
  `recording_stagger_key`'s `\u{1}`-joined string — `{x: "a\u{1}y=b"}` and
  `{x: "a", y: "b"}` flatten identically but compare unequal as maps, so the
  second lookup no longer returns the first's reader; mutation-checked by
  reverting to the flattened identity, which conflates them.
  *Original entry:* Two narrow holes, both
  needing an operator-chosen label value with an unusual character. (a)
  `flag_form` joins raw label values with `" --recording "` and presents the
  result as something to paste, so a value containing a space —
  `select with: --recording note=first run` — splits in the shell into a flag
  plus a stray positional, which for `detect-anomalies` lands in the optional
  `QUERY` slot. A value containing the literal `" --recording "` parses back as
  a duplicate key. (b) The stdio server's post-open identity dedup uses
  `recording_stagger_key`, a `\u{1}`-separated `k=v` join, so `{a: "\u{1}b=c"}`
  and `{a: "", b: "c"}` render one identity and the second lookup would return
  the first's reader. *Fix:* single-quote any value containing whitespace or a
  quote in `flag_form`; dedup on the `BTreeMap` itself rather than a flattened
  string. *Why:* both are the same species as the defects that arc kept
  finding — a wrong answer that looks like a right one — just behind inputs
  nobody types by accident.
