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
  the entry (n-way extension).
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

- **Backfill descriptions on a parquet lacking `# HELP`** — Open (optional). A
  Prometheus capture whose exporter emits no `# HELP` has blank descriptions
  (nothing to harvest at record time). A `parquet annotate --descriptions name=text`
  path could backfill the footer `descriptions` key after the fact. *Reopen:* if
  blank-description foreign captures become a recurring annoyance.
- Per-source-not-per-node descriptions, and "descriptions only exist if the origin
  supplied them," are **by design** — not backlog.

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
