# Backlog

Deferred work and reopen conditions, consolidated from the [engineering
journal](journal/README.md). Each item is grounded in a journal entry (the
"why" and the mechanism) and, where relevant, a code path. This is the
*ordering* layer; the journal entries are the record. When you pick one up,
read its source entry first, and close it out there.

Status key: **Open** (actionable now), **Roadmap** (planned next phase),
**By design** (documented limitation, reopen only if the assumption changes).

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
  columns and query fan-out. *Reopen:* when touching dashboard chart generation or
  the histogram/percentile query paths.
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

Source: [drive health sampler — Phase 1](journal/2026-07-06-drive-health-sampler.md)
(this effort is itself Open — intent landed, pre-build).

- **Phase 2 — NVMe SMART-log health** — Roadmap. Wear (`percentage_used`),
  available spare, critical-warning bits, media errors, power-on hours via NVMe Get
  Log Page 0x02 (admin passthrough ioctl).
- **Phase 3 — ATA/SATA SMART attributes** — Roadmap. Vendor-specific attribute
  parsing (reallocated sectors, etc.).
- **Hotplug discovery** — Open. Phase 1 discovers drives once at startup; drives
  added later are missed. *Reopen:* if hotplug matters. (hwmon coverage — e.g.
  `drivetemp` must be loaded for SATA temperature — is the GO-check gate that
  decides the Phase-1/Phase-2 boundary, not a standalone backlog item.)

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
