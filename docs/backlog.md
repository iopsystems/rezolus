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
  *Reopen:* for the remaining UI polish (a compare strip listing N) and browser
  verification. **N-way overlay is functionally complete**: both registries hold
  N, a multi-recording `.rez` attaches every recording under named-from-labels
  (positional fallback) ids via `dashboard::capture_alias::assign_capture_identities`,
  `/api/v1/captures` enumerates them, the overlay renderer draws N, and
  `?capture=<named-id>` round-trips on the server. Diff/side-by-side/spectrum stay
  gated to exactly 2. This unblocked the `.parquet.ab.tar` retirement below.
- **Hot-swap a capture** (replace one side, keep the other) — Open. Out of scope
  for v1; no architectural obstacle noted.
- **Live-agent compare** (file+live or live+live) — Roadmap. Explicitly excluded;
  requires a capture slot backed by a running-agent `Tsdb` rather than a
  loaded-once parquet. No near-term demand.
- **Baseline-anchor drag UI** — **Closed.** There was never a drag UI for
  either arm; the only anchor writer was a clamp. Event-anchored alignment
  ([events as ranges](journal/2026-09-28-events-ranges-and-alignment.md))
  is the first anchor UI and writes every capture's anchor from an event
  kind. A numeric offset editor has no demand yet (see that section).
- **Alias collision in saved A/B tarballs** — By design. When both sides share a
  filename basename, the compare badge shows two identical labels;
  `synthesize_ab_manifest` does not dedupe. Decided to let the user rename
  (documented in #960). Reopen only if it bites in practice.

## A/B container consolidation (`.ab.tar` → `.rez`)

Source: [retire the `.parquet.ab.tar` container](journal/2026-08-27-retire-ab-tarball.md).

- **GATE: can the WASM viewer read a v3 `.rez`?** — DONE (#1121–#1123). The WASM
  viewer opens a v3 `.rez` via `sqlite3_deserialize` (whole image into wasm RAM,
  no `-wal`/`-shm` sidecars, no async-VFS problem); the N-way overlay attaches
  every recording. Everything below was premised on this passing; it passed.
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
  list** — DONE. The picker `accept` list gained `.rez` in #1122
  (`ui/landing.js`, `ui/layout.js`). `combine`'s `--about`/`long_about`, the
  `--ab` flag help, and the examples now lead with the preferred 2-recording
  `.rez` A/B form and mark the tarball legacy (for parquet-only inputs). Slows
  growth of the tarball corpus while the deeper workstreams below land.
- **`.rez` manifest carries selection + events** — Storage + read DONE; the
  Save-as-Report *writer* of selection is the only piece left (it rides the
  column-trim item below, since a saved report also projects columns).
  `KEY_SELECTION`/`KEY_EVENTS` now live in each recording's manifest metadata
  (`BTreeMap` catalog column), written via the `#1073` `UPDATE` shape:
  `annotate <file>.rez --event/--add-events/--clear-events` embeds events, and
  both viewer backends read them — the server's `init_file_mode_rez` fills
  `state.selection` from the anchor and events ride `file_metadata`; the WASM
  `Viewer` already read both from `file_metadata`, so it needed no change. The
  Save-as-Report *writer* now emits a `.rez` on the SERVER (see below).
- **Column-level trim for `.rez`** — DONE. `rez_v3_rewrite::project_segment_columns`
  decodes → projects → re-encodes a segment (`rez::segment_writer_props()`,
  reusing the segment's `SegmentMeta` since a projection changes neither rows
  nor windows); `CopySpec.keep_metrics` threads it through the single copy path.
  Exposed as `filter <file>.rez --metrics a,b,c` (composes with `--samplers`),
  with an empty-archive guard. The one operation that breaks the
  verbatim-BLOB-copy property, as flagged.
- **Save-as-Report emits a `.rez` (both backends)** — DONE. A `.rez` source and
  a parquet **compare** now save a `.rez` on the server AND in the browser; only
  a single parquet still saves a parquet (the tarball only ever existed for the
  compare case). The trim/assemble path was made reader-available and shared
  (`report_save::build_rez_report_from_rez` / `build_rez_report_from_parquets`,
  in-memory via `RezDb::create_in_memory`/`serialize`; `rez_v3_rewrite` was
  metriken-free all along, its `write` gate lifted). Both viewer backends embed
  `KEY_SELECTION`/`KEY_EVENTS` and stamp `KEY_REPORT=trimmed` on the anchor, and
  `init_file_mode_rez` reads the marker back. The viewer-side `.parquet.ab.tar`
  **writers** are retired (server `save_combined_ab_tarball` glue + the WASM
  equivalent gone). Read compat (`ab_extract`) and the CLI `combine --ab` stay
  for a later cleanup; the shared crate's now-unused `save_combined_ab_tarball`
  is a small follow-up removal.
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
- **Edit existing event annotations** — Open. Notebook bubbles offer Delete
  (`chart.js::_renderEventBubbles` → `openEventInfo` with `onDelete`), so the
  earlier "read-only after creation" wording was stale; Edit does not exist.
  Changes other than delete go through `recording annotate --add-events` /
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

## dendro archives (6.0)

Source: [The layout of a rezolus dendro archive](journal/2026-09-25-dendro-archive-layout.md).

- ~~**Long layout: confirm by measurement**~~ — Done 2026-09-27. Long won
  for every group table with slots measured (task, cgroup, per-CPU, drive),
  and the entry now writes all of them long. A synthetic thread and cgroup
  spike at 1 s and 100 ms (2026-09-28) confirmed it. Still unmeasured:
  query-engine reads of a long table, and mount, interface and GPU tables.
- ~~**The occupant table's encoding**~~ — Decided 2026-09-28: a parquet
  stream `<stream>/occupants` beside its data stream, first-sight rows plus
  a restatement of live occupants every 300 s, with the `.rez` writer
  settings. 9.15 MB for the busy host's 396,117 occupants, about what
  compressed `caller_rows` blobs would cost. See the layout entry, "The
  occupant stream".
- **A sort key in dendro's `CompactSpec`** — Open. Sorting a task table by
  `(occupant, timestamp)` made a single-thread read 50 to 110 times smaller
  than arrival order. On disk it can go either way: a third smaller on the
  existing recordings, up to 50% larger on the spike's short-lived
  occupants. Compaction already re-encodes,
  so a caller-named sort key sorts off the tick path; segments declare it in
  parquet `sorting_columns`. It is also the reopen condition for the
  changes-only stream (NO-GO in arrival order, "Agent — changes-only
  stream" below).
- ~~**Sort at seal**~~ — Decided 2026-09-28: no. The writer seals long
  segments in arrival order (metriken#184); sorting made the 100 ms
  replays 6–20% larger and did not improve the tick path. Sorting is the
  `CompactSpec` item above.
- **A wide `.rez` segment with tens of thousands of columns cannot be
  read** — Open, a defect of the `.rez` layout today. A 1 s recording of the
  synthetic spike wrote task segments of up to 90,227 columns (the age bound
  seals them; at 100 ms the 8 MiB cap seals near 11,000). arrow-rs fails on
  the 52,109-column one with `TooManyTables` from the `ARROW:schema`
  flatbuffer verifier (`VerifierOptions::default()`, `max_tables`
  1,000,000, arrow-ipc 58 `src/convert.rs:990`), and `RezReader` reports the
  table as evicted (`crates/rez/src/reader.rs:1289`) because
  `TableReader::reader` returns `None` for a read failure and for eviction
  alike. The busy host's largest segment, 38,388 columns, reads. Two fixes:
  report a read failure as one; and until the long layout, seal a segment
  on column count as well as bytes, rows and age.
- **`docs/labels.md` omits `name` from the identity labels** — Open. cgroup
  slots set it through `SlotIdentity` (`src/agent/bpf/mod.rs:339`).
- ~~**A rezolus reader for dendro archives, on dendro's API**~~ — Done
  (#1312, #1315; the reader moved to metriken-archive in #1320).
- ~~**Recording to dendro archives**~~ — Done
  ([entry](journal/2026-09-28-dendro-writer-adoption.md)): `record` and
  `hindsight` write dendro by default (stage E), and the `recording`
  subcommands and Save-as-Report accept it (#1326, #1329, #1336, #1339,
  #1340, #1342, #1345, and E).
- ~~**dendro's `vacuum_into` fails on the read handle**~~ — Done (dendro
  #25, released in 0.3.2): `vacuum_into` lifts `query_only` for the one
  statement.
- **The 5 min seal bound in row time** — Open. dendro's `max_age` is wall
  time, so a paused producer or an offline conversion seals differently
  from a live recording. A row-time bound (seal once a segment spans N of
  row time, the first staggered) was prototyped for the seal-policy
  measurement ([entry](journal/2026-09-28-dendro-writer-adoption.md), "Seal
  policy") and matched the wall-time policy at 1 s.
- ~~**The reader routes a table by one segment's footer**~~ — Done
  (metriken-archive 0.2.8, metriken #199). Sealed segments carry a names
  fingerprint and the reader probes one footer per distinct fingerprint,
  plus the live tail's schema-carrying rows; a segment without a
  fingerprint (a `.rez`, a conversion) keeps the old assumption.
- **The reshaping converter** — Roadmap, after the reader. Replaces #1301's byte
  copy. Oracle: on 5.x `.rez --stream` recordings the occupants derived from
  the columns must equal those the recorded index gives, and every series must read back
  the same as through the `.rez` reader.
- **5.18–5.20 mid-segment occupant changes** — By design. The file does not
  record the new occupant's labels (#1232), so a conversion keeps what the file
  records. Reopen only if a recording from that range needs per-task
  attribution badly enough to accept unlabelled occupants.

## Stream consumers and membership as events (6.0)

Source: metriken `docs/journal/2026-09-30-membership-as-events.md` (phase
5d). The recorder, hindsight (#1385) and the live viewer (#1386) record the
agent's stream, and slot groups travel in the long layout
(`/metrics/stream?layout=long`).

- **What bounds the live viewer's temporary archive** — Open. `rezolus view
  http://agent:4241` records into a temporary dendro archive with no
  retention (`src/viewer/live.rs`), so a viewer left open for days grows
  without bound. Hindsight's retention is the likely answer. Do before 6.0
  if hindsight's retention fits; otherwise when a long-running live session
  is reported.

## Agent — histogram groups with slots (after 6.0)

Source: [The layout of a rezolus dendro archive](journal/2026-09-25-dendro-archive-layout.md).
No sampler has a histogram group with slots. In the `.rez` layout each slot
would be a 496-bucket list column in every segment. The long layout, which
metriken-query reads from 0.31.0 (iopsystems/metriken#165), holds one row
per observation, and a single-slot query decodes only its pages. Both items
wait for the 6.0 writer, and both are gated on the refresh cost measured at
fleet scale (docs/principles.md principle 16).

- **Per-device block IO latency** — Idea. `blockio_latency` keeps host-wide
  histograms per `op`, for device, queue and total time
  (`src/agent/samplers/blockio/linux/latency/stats.rs`). Per device, a slow
  or saturated device on a multi-disk host would stop averaging into the
  rest, and the device/queue split would say which device queues. BPF memory
  at grouping power 3: 496 buckets × 8 bytes, about 4 KB per histogram; 12
  per device (4 ops × 3 families), about 48 KB; 64 devices, about 3 MB.
- **Per-cgroup runqueue latency** — Idea. Which container waits for CPU,
  where `scheduler_runqueue_runqlat` has one host-wide histogram. About 4 KB
  per cgroup, 16 MB at the 4,096-cgroup cap, and a histogram exported per
  live cgroup every tick, so the refresh cost is the question.

## Agent — per-task CPU usage completeness

Source: [The layout of a rezolus dendro archive](journal/2026-09-25-dendro-archive-layout.md),
"Is the per-task data worth keeping". The question came from the insights-model
repo's `docs/signal-gaps.md`, where per-task `task_cpu_usage` missed CPU the
cgroup counters saw.

**Measured on a 32-CPU x86_64 host, kernel 6.12, 2026-09-25/26.** rezolus 5.20.0 agent (its BPF accounting
is identical to `main`'s; only #1244 and #1266 touched this sampler since),
recorded with the current recorder at 1 s, against the kernel's `cpu.stat` per
workload sampled every 0.5 s. Four workloads for 120 s: A, two long-lived CPU
burners; B, 794 processes of 0.3 s; C, `stress-ng --pthread`, about 2.15M
threads (17,900/s), mostly kernel time; D, 84 threads of 2–4 s. Then D alone as
a control (39 threads, 60 s).

| | kernel | rezolus |
|---|---|---|
| user, A–D together | 610.6 core-s | 591.7 (−3%) |
| system, A–D together | 182.3 | 56.3 (**−69%**) |
| D alone, total | 60.7 | 59.9 (−1%) |
| A, live per-task series | 237.0 | 234.4 (−1%) |
| B, live per-task series | 235.9 | 0.0 |
| D alone, live per-task series | 60.7 | 21.0 (35%) |

In every run the cgroup total equalled the per-task totals moved to the
exited counter, so the loss is not between rezolus's own counters. Three
separate mechanisms account for the rest:

- **Fix 1 — accounting must not depend on metadata delivery** — Done,
  #1303. That PR also fixes a second cause found while measuring it: every
  task's first observation was skipped, which drops all the CPU of a thread
  that lives about one tick. Under 60 s of `stress-ng --pthread` (about 18,000
  threads/s) the slice's CPU went from 22% of `cpu.stat` (5.20.0) to 32% with
  this fix alone and 85% with both; per-run probe cost unchanged (986 against
  985 ns). The remaining 15% under that churn is unexplained; see #1303.
  Original description:
  Under task churn the `task_info` ring buffer overflows: userspace drains it
  only when a snapshot is taken (`rb.consume()` after `sync.wait_trigger()`,
  `src/agent/bpf/builder.rs:966-970`), and it holds about 1,130 events
  (262,144 bytes, `TASK_RINGBUF_CAPACITY`; 220-byte `task_info` plus header).
  A task whose new-task event is dropped re-enters `handle_new_task` on every
  `cpuacct_account_field` hit and re-zeroes `task_utime`/`task_stime` each
  time (`cpu/linux/usage/mod.bpf.c:236-262` documents this), so its deltas are
  all skipped and its CPU is missing from the **per-CPU and per-cgroup
  totals**, not only the per-task view. That is the −69% system time above;
  with no churn (D alone) totals were within 1%. The comment there assumes
  ring buffer pressure is "normally sub-millisecond, since it drains every
  snapshot"; at a 1 s snapshot cadence it lasts up to a second. Fix: commit the
  task's baseline unconditionally and retry the metadata send through its own
  flag, so a dropped event costs at most an unlabelled task. Verify by
  repeating workload C and comparing system time to `cpu.stat`.
- **Fix 2 — stop losing task events silently** — Open.
  - Count `task_info`, `task_exit` and `cgroup_info` ring buffer drops in
    BPF and surface them in `rezolus status`.
  - Shrink `task_info`: 192 of its 220 bytes are three 64-byte cgroup path
    names (`src/agent/bpf/task.h:18-26`) that the cgroup identity already
    carries. Sending the cgroup id instead fits about six times the events per
    drain.
  - Handle a dropped exit. `sched_process_exit` fires once, so a dropped
    `task_exit` leaves a phantom metadata-presence member
    (`mod.bpf.c:434-442`): 1,573 series from workload C were still exported
    more than five minutes after their threads exited. Untested: whether a
    phantom clears when its TID is reused and relabelled.
- **Fix 3 — optionally export only tasks above a CPU threshold per
  interval** — Idea. The churn threads carry little per-thread meaning, and
  their CPU stays exact in the cgroup and exited totals. A threshold keeps the
  hot threads analysts look for and cuts the cardinality that dominates
  recordings and the wire. `signal-gaps.md` already names "top-N thresholds".
- **Fix 4a — the reader treats a task series as starting from zero** — Open.
  Short-lived tasks are exported (in the control, about 38 of 39 threads
  appeared), but `rate()` takes a series' first sample as its baseline, so the
  CPU a task used before its first scrape is never counted, and the CPU after
  its last scrape goes to the exited counter. That left 35% of a 2–4 s
  thread's CPU visible per task. The agent zeroes a task's counters at creation
  (`mod.bpf.c:230-234`), so the reader can credit the first sample's whole
  level.
- **Fix 4b — consumers read `cgroup_cpu_usage_exited_tasks`** — Open. A task
  shorter than one scrape interval (workload B) is visible only there, by
  design (`mod.bpf.c:402-408`). The insights-model analyses and the
  `measure-performance` skill should add it to per-task attribution.
- **Threads under one tick of CPU mostly get no series** — By design (the
  kernel's tick accounting). Measured 2026-09-28 on a 32-CPU host at 250 Hz: 180,000 threads
  that each ran 0.5 ms produced 24,976 task series, about 14%, where 12.5% is
  0.5 ms over a 4 ms tick at 250 Hz. `cpuacct_account_field` is charged per
  tick, so a short thread is charged a whole tick or nothing, and totals are
  right on average. Threads of 10 ms nearly all appeared
  (177,854 of 180,000 at 1 s). Where
  `VIRT_CPU_ACCOUNTING_GEN` is active (`nohz_full` CPUs) this may differ; not
  measured. Source: the layout entry's "A synthetic spike".
- **Revisit condition:** if per-task data is still not useful after fixes 1–4,
  because analyses do not use it or cannot trust it, make it opt-in rather than
  removing it. Fix 1 comes first either way: until it lands, per-task
  telemetry can make host and cgroup CPU totals wrong on high-churn hosts.
  **Done: export is opt-in** (`task_attribution`, off by default). The
  accounting still runs, since the totals depend on it; the export (events,
  the exposition-time walk of populated task slots, series) is what costs,
  and it is off unless asked for. Measured under 27 K short threads/s:
  refresh p50 78 µs off against 3,690 µs on, per-tick probe 1,424 against
  2,822 ns, host totals unchanged. The V2 snapshot path still loops over
  every pid slot of `task_cpu_usage` with the export off (it did before);
  the V3 path walks only populated slots.

Also found on these runs, separate from the sampler:

- **Recordings of a 5.22.2-alpha agent end with a row stamped about 940 s
  late** — Open. In both recordings of the #1303 build, every table's last
  row was about 940 s past the recording's end, and `rate()` over the
  recording spans the gap. Not investigated; producer-stamped timestamps
  (#1269) are the first place to look.

- **A 5.20-or-earlier recording can attribute a new cgroup's CPU to the
  previous occupant of its CSS id** — By design (fixed forward by #1232). The
  first run's `/rzt.slice` reused id 107, and the 5.20.0 recorder filed all of
  its CPU under `/system.slice/slipwayd.service` for the whole recording.
- **metriken-query supports no `increase` or `max_over_time`, and a bare
  selector on `cgroup_cpu_usage` fails with "Metric not found" while `rate()`
  works** — Open. Reproduced on recordings from both the 5.20.0 and the current
  recorder. It forced these measurements to integrate `rate()`.

## Agent — cgroup slots

Source: [The layout of a rezolus dendro archive](journal/2026-09-25-dendro-archive-layout.md), "Cgroups: long, reversed by the gate".

- **Cgroups past `MAX_CGROUPS` are dropped silently** — Open. `MAX_CGROUPS =
  4096` (`src/agent/bpf/cgroup.h:10`) is rezolus's BPF map size, not a kernel
  limit: the kernel allocates the CPU controller's `css.id` lowest-free with no
  upper bound (`kernel/cgroup/cgroup.c`, `cgroup_idr_alloc(&ss->css_idr, NULL,
  2, 0, …)`). A cgroup whose id is 4,096 or more returns `-1` from
  `handle_new_cgroup_read` (`src/agent/bpf/cgroup.h`) and is skipped at every
  other use of the id (each sampler checks `cgroup_id < MAX_CGROUPS`), with no counter and nothing in
  `rezolus status`. That happens once more than about 4,094 CPU-controller
  cgroups are live, counting dying ones that still hold their ids. Count the
  drops in BPF, surface them as a metric and a `status` degradation, and then
  decide whether the cap should be larger or sized to the host.
- **A cgroup can go unnamed while the `cgroup_info` ringbuf is full** — Open.
  `handle_new_cgroup_read` returns `-1` without advancing the serial number,
  so a later event retries (`src/agent/bpf/cgroup.h`), but until one arrives the cgroup's
  values have no `name`. Count ringbuf-full drops next to the overflow count,
  so an unrecorded or unnamed cgroup is visible either way.
- **Tasks cannot overflow the same way** — By design. `MAX_PID = 4194304`
  (`src/agent/bpf/task.h:14`) equals the kernel's `PID_MAX_LIMIT` on 64-bit
  (`include/linux/threads.h:34`), so every TID fits.

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

## Agent — filesystem sampler

Source: [Filesystem occupancy sampler — local mounts only](journal/2026-09-12-filesystem-sampler.md).

- **Network mounts** — By design. Never sampled: `statvfs` on a `hard` NFS/CIFS
  mount blocks until the server answers and the timeout is a mount option the
  agent cannot set. Reopen on demand for them; an opt-in needs its own blocking
  budget (bounded thread, per-mount deadline). The sampler's module doc
  (`src/agent/samplers/filesystem/linux/mod.rs`) points here.
- **Event-driven mount-table rescan** — Idea. `poll()` on the mountinfo
  descriptor reports `POLLPRI` on change; a sweep could rescan only then.
  Reopen if a many-thousand-mount host shows the per-sweep parse mattering.
- **`MAX_MOUNTS` = 256** — By design, raised from 64. Mounts past the cap are
  dropped, with a warning when their count changes. Reopen if a real host
  exceeds it.
- **Runtime degraded status** — Open, #1208. Resolution failures and stuck
  sweeps are logged as warnings; `rezolus status` cannot show them.
- **Partition-to-drive join** — Open, #1217. `block_device` names the partition
  or mapped device and `drivehealth` names the drive, so no query joins them.
  Reopen when someone needs that join.
- **A network mount stacked mid-sweep** — Accepted. Covered local mounts are
  dropped and a changed mount id refuses publication, but a network mount
  stacked over a local path between the table read and the `open` can still
  park the sweep thread on the lookup. Reopen if a sweep is ever observed
  parked in `open`.
- **Label changes inside a `.rez` segment** — Open, #1205. A relabeled or reused
  slot keeps writing into the column created with its first labels, because the
  group table builder keys columns by descriptor name alone. Shared with
  `cpu_usage`'s per-PID task slots; the fix belongs in `crates/rez`.
- **Filesystem context** — Open, #1206. Source, mount root and the per-mount and
  superblock option strings are not recorded.
- **Fleet-scale sweep cost** — Open. Measured only on a 3-filesystem host with an
  87-line mount table; container hosts carry thousands of mount lines. Reopen:
  measure on such a host before enabling the sampler fleet-wide.

## Agent — ext4 samplers

Source: [ext4 telemetry through eBPF](journal/2026-09-28-ext4-sampler.md).
The entry specifies `ext4_journal` (phase 1, implemented and measured),
`ext4_alloc` (phase 2) and per-filesystem counters (phase 3).

- **Probe-cost bench** — DONE, GO. `null_blk` in a guest at 450 K fsync/s:
  +225 ± 130 instructions per fsync (+0.68%), cycles +0.8% to +2.1%
  depending on baseline, throughput −1.8% to +0.1%; the two baselines
  disagree by 2%, recorded in the entry. Reopen only for a bench needing
  tighter than ±2%.
- **Fleet probes on more kernels** — Open. Probes 1–3 passed on aarch64
  Debian 13 (`6.12.75`, built-in ext4) and x86_64 Debian 13 (`6.12.63`,
  `CONFIG_EXT4_FS=m` with module BTF, where the sampler runs healthy with
  `tp_btf` twins from module BTF). Still unprobed: RHEL-family (Rocky 10)
  and anything at the 5.8 floor.
- **Counter sweep at `MAX_CPUS`** — Idea. `Counters::refresh` walks 1,024
  banks whatever the CPU count; bounding it to possible CPUs is a
  `bpf/counters.rs` change shared by every `Counters` sampler.
- **`ext4_alloc` (phase 2)** — DONE. Ten hooks including the gaps entry's
  metadata reads; every counter exact against tracefs on the Debian 13
  module-ext4 guest; refresh 151–301 µs. Not probe-cost benched: allocations
  run at write-batch rate, far below the fsync rate phase 1 benched.
- **`sync` class in `syscall_latency`** — DONE. `fsync`, `fdatasync`, `sync`,
  `syncfs` (class 9) and `msync` (class 10) moved to class 16
  (`src/agent/samplers/syscall/linux/mod.rs`) with their own histogram and
  per-CPU/per-cgroup counters; `COUNTER_GROUP_WIDTH` 16 → 24 in both syscall
  BPF programs. No new probe.
- **sysfs `errors_count` and `lifetime_write_kbytes` in the `filesystem` sweep**
  — DONE. `filesystem_errors` and `filesystem_written_bytes` per ext4 mount,
  read by `read_ext4_sysfs` on the existing 60 s off-cycle sweep
  (`src/agent/samplers/filesystem/linux/mod.rs`); absent on other types.
- **Per-filesystem sync latency** — Roadmap. Needs histogram groups with slots
  (above, after 6.0) plus a per-thread start map: the `MAX_PID` array
  (32 MB, as `syscall_latency`) or `BPF_MAP_TYPE_TASK_STORAGE` once the kernel
  floor is 5.11.
- **Phase 3 lookup map** — Built as `BPF_MAP_TYPE_HASH` keyed by `dev_t`,
  userspace-written, BPF read-only (justification in `bpf/filesystem.h`).
  Open: the per-filesystem path costs +914 instructions per fsync over the
  host-wide phase 1 programs (ext4 entry, "Results — phase 3"), and the bench
  does not split the hash lookup from the device derivation's pointer reads.
  Measure a bounded linear scan against the hash, and a `sb_dev` read cached
  per program run, before deciding either is worth changing.
- **VFS-layer read/write latency via `fentry`/`fexit`** — Idea.
  `ext4_file_read_iter`/`ext4_file_write_iter`; page-cache hit/miss split per
  filesystem. Reopen with phase 3; check the symbol set on the oldest fleet
  kernel first.
- **Extent-status cache, handle-level stats, per-page writeback hooks** —
  Idea. `ext4_es_lookup_extent_exit`, `jbd2_handle_stats`,
  `ext4_journal_start`, `ext4_da_write_pages`, `ext4_da_reserve_space`: all
  fire per lookup, per handle or per page. Measure the rate on a
  representative workload before attaching any of them.
- **Fast commit** — By design. `ext4_fc_*` off by default; reopen if a fleet
  enables `fast_commit`.
- **jbd2 counts include ocfs2** — Resolved by phase 3 (per-filesystem
  counters): an ocfs2 mount has its own slot, labeled `fstype="ocfs2"`.
- **Degraded on module-ext4 kernels below 5.11** — By design. No module BTF,
  no CO-RE against jbd2 structs; `rezolus status` shows the sampler degraded.
- **XFS** — `xfs_stats` (#1351) and `xfs_log` (#1352, opt-in) shipped, see
  [XFS telemetry](journal/2026-09-29-xfs-samplers.md); the log-space
  latency path is verified only at zero there.

## Agent — filesystem telemetry gaps

Source: [Filesystem telemetry gaps](journal/2026-09-28-filesystem-telemetry-gaps.md).
Scoping only. Ordered as the entry's plan; each sampler carries the ext4
entry's gates (measured refresh µs, a rate probe before hot hooks, the
bare-metal probe-cost bench for anything at request rate).

- **`memory_meminfo` dirty/writeback fields** — DONE (#1325), with 28 more
  of the file's lines. The dirty thresholds come from `/proc/vmstat` and are
  the vmstat follow-up's.
- **`writeback` sampler** — DONE as `memory_writeback`. Verified exact
  against tracefs on every counter; `writeback_pages_written` is the
  flusher's accounting only (an integrity sync's pages are not in it), so
  `memory_vmstat` carries the complete `memory_pages_written`. Refresh
  126–261 µs on a 56-vCPU guest.
- **`balance_dirty_pages` arity** — By design. 12 arguments on every kernel
  seen; the 8-argument form is written but untested until a kernel with it
  is on the rack. `kernel_btf_tracepoint_arg_count` picks; unknown disables.
- **`ext4_alloc` with metadata reads** — DONE (see the ext4 entry's phase 2
  results). Was: adds an allocated-extent-length histogram, preallocation discard
  counts, and `ext4_load_inode` / bitmap-load counters for synchronous
  metadata reads on the request path.
- **Slab gauges** — DONE as the `memory_slabinfo` sampler (its own sweep, 60 s
  default): eight caches, values exact against the file, sweep 366–409 µs
  read + 51–63 µs parse, 0 µs on the scrape path. See the gaps entry's
  "Results — C9".
- **Merged slab caches** — By design. A cache SLUB merges into a same-sized
  pool (`ext4_extent_status` on Debian 13's 6.12) is absent from
  `/proc/slabinfo` under its own name and so from `memory_slabinfo`. Reopen if
  its residency becomes the question; `/sys/kernel/slab/<cache>` resolves
  aliases at one directory walk per cache per sweep.
- **Per-filesystem counters** — DONE (phase 3 of the ext4 entry): every
  `ext4_journal` and `ext4_alloc` counter carries `mount`, `fstype`, `devnum`
  and `block_device`, plus `mount="other"`; exact against a three-mount VM.
- **Vacant filesystem slots are null columns** — By design. A slot freed by
  an unmount reads absent, which a V3 snapshot carries as a null-valued
  column until the slot is reused; a host churning loop or dm minors
  accumulates them, the same trade the `filesystem` sampler makes.
- **`ext4_ops` sampler** — DONE, off by default. fsync and unlink from the
  enter/exit tracepoints, write and rename via `fentry`/`fexit`, per
  filesystem and per cgroup; task local storage for the start state (floor
  5.12). See the gaps entry's "Results — C5 and C6" for the measured probe
  cost.
- **`ext4_ops` async direct-IO bytes** — By design. `ext4_file_write_iter`
  returns queued for an io_uring/libaio `O_DIRECT` write; its bytes land at
  completion, which the sampler does not hook, so `ext4_write_bytes` misses
  them and the write latency is submission time. Reopen with an
  `iomap_dio_complete`-side hook if a direct-IO workload needs the term.
- **`ext4_ops` on by default** — Reopened, decision pending. Measured end
  to end at +2.26 µs per write+fsync pair (four probes), 12% at 450 K ops/s.
  Per program with `kernel.bpf_stats_enabled`: begin hooks 270–325 ns, end
  hooks 531–537 ns; a variant without the per-cgroup path has end hooks of
  266–271 ns, so **the cgroup accounting is 265 ns, half the end hook**, and
  the bench ran 15% more fsyncs per second without it (one slot per cgroup,
  eight threads adding to the same cache lines). The per-filesystem counters
  cost under 15 ns and the histogram about 30 ns. Decided: per-cgroup
  attribution is the option `cgroup_attribution`, off by default, and folded
  out of the loaded program when off (both samplers). What remains of the
  default-on question is the 1.13 µs per pair that stays; making the cgroup
  path cheaper (per-CPU cgroup banks, or caching the cgroup id and serial in
  the task's start slot) is the way to have both. Gaps entry, Deferred,
  "`ext4_ops` probe cost". The 2026-10-01 entry below found most of the
  path's cost in `bpf_probe_read_kernel()` calls; try that first.
- **Syscall tracepoint type beside another syscall tracer** — Open,
  decision pending. `syscall_counts` and `syscall_latency` attach to
  `sys_enter`/`sys_exit` as raw tracepoints (#1392, #1400). That saves about
  82 ns per syscall where Rezolus is the only syscall tracer and costs about
  121 ns more (measured in a KVM guest, both samplers on) where another tool keeps classic programs on those tracepoints,
  because classic programs share one trace-record build and dispatch
  (`docs/journal/2026-10-01-cgroup-path-helper-calls.md`, "The tracepoint move
  depends on what else is attached"). Options: keep raw; a config choice;
  or pick at load by checking for classic programs already attached, which
  misses a tracer that arrives later.
- **Per-cgroup path: read the task group once, through BTF** — DONE.
  `syscall_counts` (#1392): its cgroup path went from 109–118 ns to 9–12 ns
  per syscall on bare metal. Every other sampler with a per-event cgroup
  path followed (`task_group_of()` in `src/agent/bpf/cgroup.h`);
  `scheduler_runqueue` went from about 300 to 138 ns per run and
  `cpu_tlb_flush` from about 150 to 40. `cpu_bandwidth` reads a css only on
  throttle events and keeps its helper-call path. Not run on a 5.8–5.10 kernel or one
  without BTF.
  Contention on the shared per-cgroup counters measured on delta: the
  attribution cost is 6.5–11 ns from 1 to 24 processes in one cgroup, and
  the change from 1 to 24 is within noise (+1.5 ns in one run, −0.7 ns in
  the other). Per-CPU cache-line-padded banks per cgroup, the layout of
  `FilesystemCounters` (768 KiB per CPU for `syscall_counts`: 24 MiB at 32
  possible CPUs, 768 MiB if sized by `MAX_CPUS`), are not needed there and
  were judged too expensive (2026-10-02). If a larger host shows contention,
  two layouts keep far fewer counters:
  - One pending bank per CPU for the cgroup that CPU last counted: the hot
    path adds to its own cache line, and when the next event's cgroup
    differs it first adds the bank into the shared array. About 200 B per
    CPU. The reader adds the pending banks to the shared array, and a flush
    racing a read needs a per-CPU sequence count or a tolerated transient.
  - Shared arrays per last-level cache instead of per CPU: atomics stay, but
    only CPUs sharing an L3 add to one line. 768 KiB per L3 domain for
    `syscall_counts`, with no flush and no race.
  `docs/journal/2026-10-01-cgroup-path-helper-calls.md`.
- **Write-amplification decomposition dashboard** — DONE as the ext4
  dashboard's Write Path group: application bytes (`ext4_write_bytes`),
  writeback bytes, journal bytes, device bytes on one axis, each term drawn
  when the recording has it.
- **XFS samplers** — Both steps done, in
  [XFS telemetry](journal/2026-09-29-xfs-samplers.md). `xfs_stats` (#1351):
  41 per-mount counters from `/sys/fs/xfs/<dev>/stats/stats`, exact against
  the file and `/proc/fs/xfs/stat`. `xfs_log` (#1352, opt-in): log-space
  and log-force blocked time per mount and per cgroup with host histograms;
  force counts exact against the stats file, 1.3 µs per force by
  `kernel.bpf_stats_enabled`. Left open there: the log-space latency has
  only ever read 0 (three attempts to fill a 64 MiB log failed; reopen on a
  host with nonzero `xfs_log_space_sleeps`), default-on (by design opt-in;
  the number is 1.3 µs per force), and CIL wait latency (count only).
- **Refresh-dispatched sweeps race the snapshot walk** — Open, from
  [XFS telemetry](journal/2026-09-29-xfs-samplers.md) Results — step 1. A
  `spawn_blocking` sweep dispatched by `refresh()` (`xfs_stats`,
  `memory_slabinfo`, `filesystem`) can finish mid-walk, and the builder then
  widens the group's window to the union of two sweeps
  (`resolve_walk_window`), an interval wide. Drive sweeps from a timer, or
  emit pre-pass values with the pre-pass window. Reopen when a consumer
  needs the band tight.
- **Page-cache hit ratio** — Built as `memory_pagecache` (opt-in) in the
  cheaper shape recorded in
  [Page-cache hit ratio](journal/2026-09-29-pagecache-hit-ratio.md): one
  `fentry` on `filemap_read` for calls and bytes, fills classified at fill
  rate by the filling task's syscall (read, write, fault, other), evictions,
  mmap faults, per mount; cgroup series behind `cgroup_attribution`. The
  bracket design (per-call hit/miss, latency by outcome) stays unbuilt;
  reopen if those are needed.
- **Per-cgroup writeback throttling** — Roadmap. `balance_dirty_pages` keys
  by `cgroup_ino`, not css id; needs an inode-keyed lookup in `bpf/cgroup.h`.

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
- **`GPU_ENERGY_CONSUMPTION` can publish a fabricated zero** — **Closed** by
  metriken 0.11. It is a `CounterGroup` (`stats.rs`), and `CounterGroup::value()`
  used to have **no** sentinel: it returned `Some(0)` for an unwritten slot as
  soon as *any* index in that group had been written. So on a mixed multi-GPU
  host where `total_energy_consumption()` succeeds for device 0 and fails for
  device 1, device 1 published a constant-zero energy counter that read as a real
  measurement. `GaugeGroup` never had the problem — it has always used an
  `i64::MIN` sentinel and correctly yielded `None` — and that asymmetry was the
  trap: which group type a metric happened to use silently decided whether a
  partially-populated group was a bug. metriken 0.11 gives an owned
  `CounterGroup` the same treatment with `u64::MAX`
  (iopsystems/metriken#143), so an unwritten entry now reads `None`.
  *Still open for externally backed groups:* a BPF mmap is kernel zero-filled
  and cannot hold a sentinel, so an unwritten slot there still reads `0`.
  Membership for those comes from the map's registered entries rather than from
  value presence, which is what `set_member_set` exists to declare.
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

## Agent — `cpu_perf` under virtualization

Source: [XFS telemetry](journal/2026-09-29-xfs-samplers.md), Results — step
2, "Also observed". Found by reading the kernel's per-program statistics on
the CI guest while profiling `ext4_ops` and `xfs_log`.

- **`cpu_perf`'s `sched_switch` program costs 20.5 µs per run on a KVM
  guest** — Open. rezolus 5.20.0 (the image's agent; the program is
  unchanged on `main`): 8,235,630 runs, 169 s of program time over a 247 s
  `perf bench sched pipe`, 0.68 of a core at 33 K switches/s, against 114 ns
  and 665 ns for the other two programs on the same tracepoint. The two
  `bpf_perf_event_read` calls in `cpu/linux/perf/mod.bpf.c` read the
  virtualized PMU through the hypervisor. Confirmed from inside the guest
  (systemslab `01a0edd1-f053-716d-7604-c6261d862311`, KVM on a Threadripper
  3970X, `perfctr_core` exposed, the AMD PMU driver loaded): a user-space
  `rdpmc` costs 1.05–11.3 µs per read and the `read(2)` path 1.7–12.7 µs,
  against tens of nanoseconds on bare metal, and the cost moves with which
  counter index the event landed on (11.3 µs on index 6 with the image's
  agent holding counters, 1.05 µs on index 1 with it stopped). So it is the
  hypervisor's trap-and-emulate for every counter read, which the vPMU
  being exposed to the guest makes reachable; the program's two reads per
  switch are the 20 µs. Infrastructure side: the anvil VMs expose the vPMU
  deliberately (perf works in the guest); with it off, `cpu_perf` and the
  other PMU samplers would report unsupported and cost nothing. Rezolus
  side, still open: detect a hypervisor at init (`hypervisor` CPU flag) and
  refuse or throttle `cpu_perf` there, and name the PMU holder in
  `rezolus status`. A bare-metal figure for the same program was not
  measured (no bare-metal host in this session).
- **A second agent's PMU reservations starve the first** — Observation.
  With the image's `cpu_perf` holding the counters, every agent started
  beside it reported `cpu_branch`, `cpu_dtlb` and `cpu_perf` pmu-starved
  ("needs 2/cpu, 1 free"). Expected, but the status line does not say who
  holds them; a hint ("another perf user holds N counters") would save the
  diagnosis. Stop the image's service before a VM bench.

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

## Viewer — investigation workflow (links, events, baselines, checks, MCP)

Five design entries opened 2026-09-28. They share one data model (events in
the manifest, capture ids, the decimated wire) and are ordered by dependency:
links and range events first, then event-anchored alignment, then the family
baseline, then checks, then the MCP tools that expose them.

Source: [Viewer links that carry the whole view](journal/2026-09-28-viewer-link-state.md).

- **Encode view state in the URL** — **DONE.** `ui/url_state.js` (both
  shells) carries `from`/`to`, `time`, `node`, `gpu`, `cgroup`, `instance`
  and `anchor.<id>` in `location.search`, NOT the hash as first designed:
  `m.route.get()` includes a hash query and five places parse that path by
  hand, while mithril's fragment-only `pushState` keeps the search across
  every navigation. Written by `replaceState` on change, read on load with
  the URL winning over `localStorage` per key; unsatisfiable values are
  dropped with a warning and the link rewritten. Documented in
  `docs/usage.md` ("Linking to a view").
- **Live-mode relative ranges** (`?last=5m`) — Open. Absolute `from`/`to`
  are ignored in live mode. *Reopen:* when live-agent links are requested.
- **`from`/`to` on the experiment side** — Open. The link sets the baseline's
  range override, as a drill-down does; experiment fetches use
  `experimentQueryRange` (`viewer_core.js`) and ignore it. The experiment
  window should be `[from − Δ, to − Δ]` with Δ the anchor difference; lands
  with event-anchored alignment.
- **`anchor.<named id>` for N-way** — **DONE** with event-anchored
  alignment; `setAnchor` accepts any id, and a link's named anchors are
  applied once the registry lists the arm (unknown arms are dropped with a
  warning).
- **`step` in the URL** — Open, one key. Already restored from localStorage.
- **Time bar ignores the range override** — Open (bug, pre-existing).
  `applyDisplayWindow` clears `globalZoom` and `TimeRangeBar`
  (`ui/controls.js`) labels from the full recording's `start_time`/`end_time`,
  so after any drill-down (and on every `from/to` link) the bar shows 0–100%
  while the charts show the window. The time-bar render assertion for links
  waits on this.
- **About link prefix** — Open (bug, trivial). `#!/overview` in `app.js` with
  `m.route.prefix = '#'` matches nothing and lands on the default route by
  fallback.
- **`uploadParquet` keeps `_rangeOverride` across a file swap** — **DONE**
  (found pre-existing, fixed in the same PR): both shells call
  `resetLinkedViewState()` before a new file is read.

Source: [Events as ranges, phases, and alignment anchors](journal/2026-09-28-events-ranges-and-alignment.md).

- **Render `duration_ns` events as bands** — **DONE.** `buildRangeSpans`
  beside `buildMarkLine` (`charts/event_markers.js`) and an HTML
  `div.event-range-band` drawn by `_renderEventBubbles`. The design called
  for an echarts `markArea`; the custom-series heatmaps never laid one out
  (see the entry), so the band is an overlay. An End field on the add-event
  form; Duration/End/Details in the info popover. The CLI already accepted
  `duration=`/`duration_ns=` (the item was wrong about that); it gained a
  test and help text. Fell out of it:
  compare-mode charts now place events on their relative axis through
  `spec.eventTimeOriginSec` (every marker used to land off-grid there).
  Per-capture event lists remain with event-anchored alignment below.
- **Recorder-emitted `run_start`/`run_end` events for `record -- cmd`** —
  **DONE.** The `.rez` writer gained `Msg::UpdateMetadata` so a recording's
  metadata can change after its seed, and the recording loop gained a
  `select!` arm on the child's exit so `run_end` is stamped when the exit
  happens rather than at the next tick. Events name the program by
  `argv[0]`'s basename; `--record-command-line` adds the full argument list.
- **Run events on each recording's own timeline** — Open. The events are
  stamped on the recorder's clock while a rezolus agent's rows are on the
  agent's, so a remote agent's markers sit off its rows by the host skew. On
  the scrape path the agent's `ts` and the tick's `anchored_ns` are both in
  hand where the tick is staged, so a per-recording conversion with
  round-trip precision is possible there; on the stream path a frame arrives
  up to an interval late, so no comparable pairing exists and the recorder's
  clock is the honest choice.
- **Event-anchored compare alignment** — **DONE.** `anchors[id]` is a
  number or `{ kind }` (still v3), resolved per capture by
  `events/capture_events.js` against each capture's file events and
  recording start; the "Align on" select in the compare badge is the first
  anchor UI; numeric anchors are now measured from the recording start, not
  the first fetched sample.
- **Numeric anchor editor** — Open, no demand. The `{ kind }` form covers
  the benchmark case; a typed ms offset has a stable base now (the
  recording start) but no control writes one.
- **`annotate --recording k=v`** — Open. `annotate` writes the same events
  into every recording of a multi-recording `.rez`, so a hand-written
  `run_start` on a combined archive lands at one instant in both and
  alignment is a no-op. Parse with `RecordingSelector::parse(pairs,
  "--recording")` (`src/mcp/recording_selector.rs`), resolve against the
  `(labels, reader)` pairs the loop in `src/parquet_tools/annotate.rs`
  already has, skip non-matching recordings. Recorder-emitted run events
  differ per run already, which is why this waits.
- **Compare-mode event markers per capture** — Open. The overlay and split
  charts place the baseline's events by the first capture's anchor; with
  per-capture event lists now in `captureContext`, each capture's events
  could be drawn by its own anchor through `resolveAnchor`.
- **Events from the agent's own status transitions** — Idea. *Reopen:* when
  `/status` carries a transition log rather than current state.

Source: [A baseline built from many recordings](journal/2026-09-28-baseline-from-many-recordings.md).

- **Family baseline** — **DONE**, as a viewer setting rather than a
  `--baseline` set selector: with three or more captures the compare
  badge's "Baseline" menu makes every capture but the experiment one band
  (`charts/util/family_math.js`, `compare.js::overlayLine`), mean ± kσ or
  min..max over the first member's grid, member count in the legend,
  `family=` in the link. Spread view only.
- **N-way extra captures are fetched one at a time per chart** — DONE.
  `data.js` memoizes the capture list and per-capture metadata per view
  (`listCaptures`, `captureMetadata`) and `fetchExtraCaptures` runs the
  per-capture fetches through a bounded pool (`mapLimit`). Metadata
  requests per 20-recording `#/cpu` load fell 347 → 23. The wall clock did
  not move on either build (debug 3.26 → 3.20 s, release 1.15 → 1.09 s),
  and the release build loads 20 recordings in 1.2× the two-capture time
  before and after: the 4.1× that raised this item was the debug query
  engine, not the loop. See the entry's "The fetch loop was not the cost".
- **Family named by label from the CLI** (`--baseline arm=nightly` matching
  many) — Open, no demand yet: today the family is every attached capture
  but the experiment, and `combine`/a multi-recording `.rez` decides what
  is attached.
- **Per-bucket member count in the tooltip** — Open. `familyBand(...).n` is
  computed; the tooltip does not show it.
- **Family over heatmaps and percentile charts** — Open. *Reopen:* after the
  line case.

Source: [Checks with verdicts, stored in the recording](journal/2026-09-28-checks-with-verdicts.md).

- **`check` on a KPI and `recording check`** — DONE. `Kpi.check`
  (`above`/`below`, `quantile`, `for`, `severity`) in
  `crates/dashboard/src/service_extension.rs`, the unread `slo` field
  removed; `rezolus recording check` in `src/parquet_tools/check.rs` evaluates
  through `metriken_query` on a grid of the recording's interval capped at
  1 s, exit 1 on fail and 2 on error, `--annotate` writes violations as
  `kind=check` range events carrying the check JSON (ids hash title, query,
  condition and start; a re-run rewrites a window that grew), band straddles
  and interpolated points report `INDETERMINATE` and one such point makes a
  whole run indeterminate, a data gap of more than 1.5 steps (the grid step
  or the series' own spacing, whichever is coarser) ends a run. The 9.6 h gate was not measured in this change; the
  evaluation is one `query_range` per check over the whole span, the same
  cost as one `mcp query` each.
- **Family checks** (`outside_family_sigma`) — Open, after the baseline lands.
- **Checks from inside the viewer** — Open. *Reopen:* after the CLI has users.

Source: [MCP write-back and tool tiers](journal/2026-09-28-mcp-write-back.md).

- **`add_event` and `run_checks` (additive tier)** — DONE.
  `src/mcp/server.rs`; `add_event` writes through
  `parquet_tools::events::add_events_selected` with `source=mcp` and refuses
  a multi-recording archive without a selector; `run_checks` calls the
  runner split out of `recording check` (`check::run_checks`).
- **Mutating tier behind `rezolus mcp --allow-mutating`** — DONE for
  `remove_events` (`ServerOptions`, tiered `tools/list`, flag-off call
  refused naming the flag, tested). `set_kpis` — Open, no producer of KPI
  sets on the agent side yet; the annotate KPI path is ready for it.
- **`export_query` and `viewer_link`** — DONE. `src/mcp/export.rs` writes
  a range query as long-form CSV/parquet under `--export-dir` only;
  `src/mcp/link.rs` formats the [viewer link](journal/2026-09-28-viewer-link-state.md)
  wire form, pinned to the JS parser by `tests/viewer_link_parity.test.mjs`,
  with a full URL from `--viewer-url` or a `viewer_url` argument.
- **`rezolus mcp install`** — DONE. `src/mcp/install.rs` registers through
  `claude mcp add` (project `.mcp.json` merged when the CLI is absent) and
  installs the embedded `rezolus-mcp` skill (`src/mcp/skill/SKILL.md`).
- **Skills for other clients** — Open, no demand. The install command's
  client list is the place; the skill text is client-neutral.
- **`annotate --recording`** — Open, now cheap: `events::add_events_selected`
  is the selector-scoped write; the CLI flag would call it instead of
  writing every recording.

Related ideas with no entry yet:

- **Section-wide shared crosshair** — Idea. Hover cursor sync exists only
  within a compare group (`ChartsState._compareCursorSubs`,
  `src/viewer/assets/lib/charts/chart.js`); zoom is already global. A
  section-wide crosshair behind a toggle is a different thing from the
  pin-sync fan-out rejected in #828 and does not reopen that decision.
- **One reference page for time semantics** — Idea. Null propagation in
  compare, cross-cadence evaluation at the slow sampler's rows, the
  acquisition band, Aligned/Raw time modes and decimation are each correct
  and each documented in a different journal entry or in `CLAUDE.md`. A
  single page under `docs/` that states the rules, plus an `llms.txt` index,
  so an agent or a new reader gets them without the archaeology.

## Agent — changes-only stream

Source: [Sending only what changed](journal/2026-09-28-changes-only-stream.md).
NO-GO on size, measured 2026-09-29: on arrival-ordered long tables under
zstd-3, removing unchanged readings saved 25.0% on the busy host and cost
16.5% on the quiet host (rows dropped), or 12.4% and −10.6% (values nulled).

- ~~**Measure unchanged readings per long table**~~ — Done 2026-09-29, in the
  entry's "Measured" section.
- **Agent: send only changed members; long layout v2** — NO-GO. Reopen when
  the `CompactSpec` sort key (below, "dendro archives (6.0)") lands and
  segments sorted by occupant still carry long runs of repeated values that
  their encoding does not already collapse.
- **Liveness without a lost event** — Open, useful without the rest. A
  dropped `task_exit` leaves a phantom member at 0 until PID reuse
  (`account__sched_process_exit`). Detect it at read time from
  `task_start_times`. Cgroups have no removal event at all.

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
