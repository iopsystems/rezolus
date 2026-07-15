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
| 2026-07-15 | [Measurement uncertainty — `.rez` reader ecosystem (viewer / MCP / parquet-tools)](2026-07-15-rez-reader-ecosystem.md) | Open — design landed, pre-build. Sub-project (3): make `.rez` readable everywhere `.parquet` is. Union `RezReader: MetricsSource` (N per-sampler readers, clear error on cross-table); phased A (metriken-query skips `:window_*` sidecars) → B (single-recording read: viewer/MCP/`parquet metadata`) → C (multi-recording assembly + label faceting + `combine`/`filter`/`annotate`) |
