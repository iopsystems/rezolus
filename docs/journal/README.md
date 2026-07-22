# Engineering Journal

An in-repo, code-grounded record of non-trivial efforts: what we set out to do,
the GO/NO-GO decision, what happened, and what was learned. Entries live here so
they are versioned with the code, greppable, and readable by the next engineer
(or agent) without leaving the tree.

Conventions:

- One markdown file per effort, named `YYYY-MM-DD-slug.md` (open date).
- Ground every claim in source: real commit SHAs, file paths, measured numbers.
  Never invent figures. If a detail isn't in the source, say so or omit it.
- NO-GOs and dead-ends are first-class entries — record the mechanism and the
  condition under which to reopen.
- Issues/PRs are the task layer; this journal is the narrative/decision layer.
  Link them together; don't let a PR be the only record of a non-trivial effort.

## Entries

Entries before 2026-07-06 were reconstructed retrospectively from design docs,
merged PRs, and project notes; each is grounded in real commits/PRs.

| Date | Effort | Status |
|------|--------|--------|
| 2026-04-18 | [Viewer performance and JS restructure](2026-04-18-viewer-perf-restructure.md) | Shipped (merged) |
| 2026-04-19 | [Viewer chart & heatmap UX](2026-04-19-viewer-chart-ux.md) | Shipped (merged) |
| 2026-04-21 | [A/B compare mode for the viewer](2026-04-21-ab-compare-mode.md) | Shipped (merged) |
| 2026-05-10 | [Selection → Notebook → Report](2026-05-10-selection-notebook-report.md) | Shipped (merged) |
| 2026-07-02 | [`document-feature` skill — agent-verified CLI help](2026-07-02-document-feature-skill.md) | Shipped (merged) |
| 2026-07-03 | [Viewer support for "simple capture" parquets](2026-07-03-simple-capture-viewer.md) | Shipped (PR #989) |
| 2026-07-04 | [Per-source metric descriptions in combined parquets](2026-07-04-per-source-descriptions.md) | Shipped (PR #989) |
| 2026-07-06 | [Drive health sampler — Phase 1: all-drive temperature (module-free)](2026-07-06-drive-health-sampler.md) | Phase 1 GO — shipped via pass-through ioctls, no module (SATA hw-verified; NVMe fixtures). Phases 2–3 open |
| 2026-07-08 | [Measurement uncertainty — acquisition windows, multi-timeline plotting, rate error bars](2026-07-08-measurement-uncertainty.md) | Open — vision/intent landed, pre-build. Cross-cutting arc (metriken core, rezolus first consumer); temporal-first; phased |
| 2026-07-10 | [Measurement uncertainty — Phase 1: observation acquisition windows](2026-07-10-measurement-uncertainty-phase-1.md) | Implemented (drivehealth pilot, hw-validated); folding into all-sampler windows below |
| 2026-07-10 | [Measurement uncertainty — all-sampler observation windows](2026-07-10-all-sampler-observation-windows.md) | IMPLEMENTED & VALIDATED — enforced windowed wrapper types (metriken `next`, 13 commits) + rezolus fleet migration; drivehealth hw-validated (22 SATA drives, per-device windows); live agent shows BPF windows ~1000× tighter than fleet. Pending PR/publish |
| 2026-07-13 | [Measurement uncertainty — per-sampler `.rez` archive (sampler grouping + recorder)](2026-07-13-per-sampler-rez-archive.md) | IMPLEMENTED & VALIDATED (sub-projects 1+2), incl. **label-set format revision landed**. Per-sampler tables in a `.rez` tar (windows first-class, window-advance dedup); module-path attribution resurrected as a `sampler` label. Container is a **bag of label-tagged recordings** (`<dir>/<sampler>.parquet` + `recordings[]` manifest with `source`/`host`/`--label` labels); live-validated (25 tables, drivehealth 1 row vs fast 7, labels from systeminfo + `--label`). First of 4 sub-projects to make windows usable (readers + query/rate-error-bars follow) |
| 2026-07-16 | [Measurement uncertainty — histogram value uncertainty (bucket resolution)](2026-07-16-histogram-value-uncertainty.md) | **LANDED** (metriken `034cde2`). Follow-on to sub-project (4): `histogram_quantile` carries a **value** band = the containing bucket `[start, end]` (from `grouping_power`/`max_value_power`, already in the parquet — no windows); added `(Materialized, Scalar)` binary support so `histogram_quantiles(...)/1e9` latency panels keep a scaled band (also closed a prior `Unsupported` gap). Reuses `QueryResult.intervals`/MCP/viewer infra. `histogram_sum`/`mean` **now banded too** (`36b4ba5`: `sum ∈ [Σ count·start, Σ count·end]`, `mean = sum/N`, `count` exact) |
| 2026-07-15 | [Measurement uncertainty — rate() error bars (query-engine leaf)](2026-07-15-rate-error-bars.md) | **LANDED** (leaf-only engine + MCP). Sub-project (4), the arc's culmination: `rate()`/`irate()` turn per-observation windows into honest uncertainty via interval arithmetic (`rate ∈ [Δv/(e_last−b_first), Δv/(b_last−e_first)]`, widened to contain the nominal). Live: `rate(blockio_bytes[1m]) = 587264 [587233.83, 587264.00]`; `rate()*60` drops the band (leaf-only). metriken `8622c8c`/`a77124b`/`4bd86e7`/`90f04ae` + rezolus `b6bb92f8`. Tier-1 propagation, viewer bands, correlation ceiling = later rounds |
| 2026-07-15 | [Measurement uncertainty — `.rez` reader ecosystem (viewer / MCP / parquet-tools)](2026-07-15-rez-reader-ecosystem.md) | In progress — **Phases A, B core, C landed**. Sub-project (3): make `.rez` readable everywhere `.parquet` is. Union `RezReader: MetricsSource`. A (metriken-query skips `:window_*` sidecars) ✓; B (viewer file-mode + MCP + `parquet metadata`) ✓; C (`combine` multi-recording + viewer 2-arm A/B + `filter --samplers` + `annotate --queries`) ✓. Deferred: viewer upload-mode, Prometheus guard, and simultaneous **N-way** faceting (own future effort — 2-way capture model + frontend) |
| 2026-07-17 | [Measurement uncertainty — correlation uncertainty range (interval r-band)](2026-07-17-correlation-uncertainty-ceiling.md) | **LANDED** (rezolus, MCP-only). Round 2 of the post-rate sequence: `analyze-correlation` reports `r [r_lo, r_hi]` — the Pearson `r` still achievable as each point varies within its rate/histogram band. Pure interval arithmetic (attenuation/disattenuation model *rejected* for breaking the no-distribution rule); greedy corner coordinate-search yields an **achievable subset** (never over-claims tightness), nominal always contained. Live: `sum(rate(network_bytes[1m]))` vs `…packets` → `0.8219 [0.8206, 0.8231]`; tight cpu windows collapse to `[r, r]`. Bands need `.rez`/live (windows) |
| 2026-07-17 | [Measurement uncertainty — viewer bands on compare & multi-series charts](2026-07-17-viewer-compare-multi-series-bands.md) | **LANDED** (rezolus viewer). Round 3 (final) of the post-rate sequence: uncertainty bands now render on **A/B compare** line overlays (each capture carries its band through `overlayLine`→`multiSeries`) and on **percentile multi-series** charts (`isPercentileChart`-gated; per-core/cgroup multis carry the data but stay undrawn to avoid a band wash). Root fix: the multi/compare data paths no longer drop `intervals`. Reuses `buildBandSeries`. Closes the viewer side of the arc |
| 2026-07-13 | [Viewer display-mode decimation (min/max envelope + drill-down)](2026-07-13-viewer-display-decimation.md) | In review (PR #1006) — metriken-query 0.12.0 published, `[patch]` dropped; browser verification + WASM-runtime parity test pending. 5 measured NO-GOs banked |
| 2026-07-21 | [Display-mode decimation — mean vs. median for the line](2026-07-21-decimation-mean-vs-median.md) | OPEN — discussion. Median line shipped by design (robustness, line-inside-band invariant, envelope carries extremes); mean's conservation argument (`mean × width = Σ samples`) recorded. Leaning: keep the median line, carry a per-bucket mean in the wire for the tooltip; decide after verifying whether notebook/compare stats inherit median semantics |
| 2026-07-21 | [Viewer band system — view split, decimation budget, worst-case hull](2026-07-21-viewer-band-views.md) | OPEN — design landed pre-build, resuming 2026-07-22. Decisions: split spread ("what happened") from measurement ("what we can claim") into distinct chart views, never overlaid; budget policy = `native ≤ px/5 → raw, else min(px, ⌈native/5⌉)` honest buckets (element-gating recorded as fallback; fixes the 48-floor-as-cap formula bug); interval-hull worst-case envelope for the measurement view (monotone-statistic extension). End-state: unsnapped timestamps turn the measurement view into an exception surface. Open: toggle scope |
