# A/B Compare Mode for the Viewer

- **Opened:** 2026-04-21
- **Status:** SHIPPED — merged across PRs #820, #821, #822, #825, #827, #828, #832, #833, #852, #854, #872, #881, #912, #917, #960 (iopsystems/rezolus)
## Problem

The viewer was a single-capture tool. An engineer with `baseline.parquet` and
`experiment.parquet` had to open two browser tabs, mentally diff them, and lose
context on every section switch. Per-chart type — overlay lines, side-by-side
heatmaps, split multi/scatter subgroups — needed distinct rendering logic, but the
frontend had no shared seam for it. Time alignment across captures of unequal
duration was a hard problem: the obvious shortcut (fabricating nulls or carrying
forward) poisons any diff math.

## Goal

Two parquets in, one dashboard out. Every chart type renders a compare-mode
variant: line overlays (blue baseline, green experiment), side-by-side heatmaps
with an optional diff toggle, split subgroup charts for multi/scatter. Relative-time
X axis with per-capture user-draggable anchors. Null propagates everywhere — no
interpolation, no zero-substitution. Full parity between the server viewer and the
WASM static-site viewer.

## Key Decisions

**Two independent TSDbs, stitched in the frontend** (`src/viewer/capture_registry.rs`).
Each capture slot holds its own `Tsdb`; every query carries an optional
`capture=baseline|experiment` param. No cross-capture joins in the engine.
`compare.js` (`src/viewer/assets/lib/charts/compare.js`, 679 lines) issues both
queries in parallel and stitches results. This keeps the backend trivially correct
at the cost of double the network traffic per chart — an acceptable trade at the
viewer's query volume.

**Dashboard JSON is immutable across modes.** The Rust `crates/dashboard/` crate
emits the same plot specs regardless of compare mode. All per-chart-type switching
(overlay vs side-by-side vs diff) lives in `compare.js`. This preserved all
existing callers and meant no dashboard format bump.

**Null-propagating comparisons as an invariant.** `src/viewer/assets/lib/charts/util/compare_math.js`
holds pure helpers; every comparison operation (`nullDiff`, `nullRatio`, etc.)
returns null if either operand is null. Codified as unit tests via `node --test`.
The diff heatmap uses a dedicated null cell color distinct from both the zero point
on the diverging scale and any per-capture null color — users cannot confuse "no
data" with "no difference."

**Alias = cosmetic, id = wire-stable.** PR #827 (`e30115b1`) added `alias=path`
positional CLI sugar (`rezolus view redis=a.parquet valkey=b.parquet`). Aliases
thread through `CaptureRegistry` slots → `/api/v1/metadata` → frontend `captureLabels`
map → chart series names, slot labels, diff tooltip headers. Internal identifiers
stay `baseline`/`experiment` permanently so anchors, API params, and persisted
selection state are never broken by a cosmetic rename. This design was driven
directly by user feedback rejecting `--baseline-name`/`--experiment-name` flag
pairs — see `feedback_compare_mode_n_way_extension.md`.

**Quantile heatmap (Full/Tail) in compare mode requires a shared color scale.**
PR #872 (`f08cbe20`). Both halves of a side-by-side pair must render with
`[unifiedMin, unifiedMax]` so equal colors mean equal absolute values. Without
this, side-by-side becomes two unrelated heatmaps sharing axes — a lie. The tradeoff:
a capture with a much wider range washes out the narrower one. That's correct for
A/B work; users read absolute values from the tooltip. The diff path (`renderDiffQuantileHeatmap`)
uses a separate diverging palette resampled so neutral always lands exactly on zero.

**Combined-A/B tarball for offline distribution.** PR #912 (`79990823`). A combined
`.parquet.ab.tar` embeds `baseline.parquet`, `experiment.parquet`, and an `ab.json`
manifest so recipients can open the bundle without carrying two files. The viewer
auto-detects `ab_containers` metadata on load and enters compare mode without any
user action. The `--ab baseline=<source> experiment=<source>` flag on
`parquet combine` writes the manifest and tags every output column with
`container=baseline|experiment` — the container axis is orthogonal to existing
labels (source, node, etc.).

**Manifest synthesis for Save as Report in two-file compare.** PR #960 (`c7149ca3`)
lifted the last explicit disable of the Save as Report button in compare mode
(`selection.js:911`). The gap: two-file compare had no pre-built `AbContainers`
manifest. Fix: `synthesize_ab_manifest()` in `src/parquet_metadata.rs` derives
the manifest from runtime state (aliases → filename basenames → slot literals as
fallback). Both `src/viewer/actions.rs` (server) and `crates/viewer/src/lib.rs`
(WASM) call through the same tarball writer. Default download filename became
`<baseline>_vs_<experiment>.parquet.ab.tar` to surface both sides.

## What Shipped / Acceptance

| PR | SHA | What landed |
|----|-----|-------------|
| #820 | `f4542d79` | Core: `CaptureRegistry`, compare endpoints, `compare.js` adapter, per-type strategies, relative-time axis |
| #821 | `8a357bef` | Bug bash: legends, zoom, layout, default route in compare mode |
| #822 | `f96defc8` | Simplify sweep: diff heatmap polish, compare-mode follow-ups |
| #825 | `ac65bb4d` | Observable `ChartsState` with `setZoom` / `subscribeZoom` — killed the datazoom cascade that clobbered zoom state on compare heatmap redraws |
| #827 | `e30115b1` | `alias=path` positional CLI, compact top-nav compare badge |
| #828 | (revert of #825 pin) | Percentile pins reverted to per-chart (`chart.pinnedSet`) — see dead-ends below |
| #832 | `c7fa9436` | WASM static site: URL params honor `capture=alias=path`, scaling toward N captures |
| #833 | `5bc1d3a5` | Fix experiment charts not refreshing when granularity changes (oninit caching bug) |
| #852 | `9b238be0` | Fix node filter pinning only to baseline, not experiment |
| #854 | `ca878ef5` | Follow-ups: preserve bootstrapped nav across cache clears; skip service reload on node change |
| #872 | `f08cbe20` | Quantile heatmap (Full/Tail) in compare mode: unified color scale, diff heatmap, `buildDeltaSpectrum`, parallel spectrum fetches |
| #881 | `dce4a0b4` | Fix diff heatmap silently falling back to percentile scatter when captures had different durations (mismatched time grids) |
| #912 | `79990823` | Combined-A/B tarball: `parquet combine --ab`, `ab_containers` metadata, `container=` column labels, viewer auto-detect |
| #917 | `4022bbbc` | Generate AB smoke fixtures on demand (Lit chart spike landed in the same PR; the smoke infrastructure is the relevant part) |
| #910 | `adadf552` | Fix compare-mode label matching for multi-dim percentile metrics in category (inference-library) compare |
| #960 | `c7149ca3` | Save as Report in two-file compare: `synthesize_ab_manifest`, unified tarball dispatch, `<a>_vs_<b>` default filename, Load Parquet accepts `.ab.tar` at runtime |

Smoke test coverage lives in `tests/viewer_smoke.sh` (two-file compare mode added
in the #960 wave).

## Learnings / Dead-Ends

**The zoom-sync revert (#828) was the right call.** PR #825 globalized percentile
pin selection via `ChartsState.pinnedLabels` + `subscribePins` fan-out alongside the
zoom refactor. The user rejected the UX explicitly: "pinning in one chart shouldn't
silently pin elsewhere." Reverted in #828; pins live on `chart.pinnedSet`. The zoom
half of #825 was sound and stayed. Lesson: cross-chart state fans out naturally into
unwanted synchronization; the burden of proof is on the feature, not the local
default. Do not re-globalize pins via `ChartsState`. (Memory: `project_viewer_pin_scope.md`)

**The spectrum fetch time-grid mismatch was non-obvious.** PR #881 (`dce4a0b4`).
When baseline was 3600 s and experiment was 1800 s, each picked an independent step
(window/500) so their time grids didn't align. `buildDeltaSpectrum` refused to diff
and `renderDiffQuantileHeatmap` silently fell back to the baseline single-capture
percentile scatter — the user saw the wrong chart type with zero error signal. Fix:
`kickOffSpectrumFetch` resolves both steps, takes `max`, passes explicit shared-step
ranges to both fetches. `buildDeltaSpectrum` still rejects step mismatches (returns
null) but now tolerates timestamp-count mismatches after relative-t rebasing by
truncating to the common prefix.

**Compare-mode label matching for multi-dim metrics required asymmetry.** PR #910
(`adadf552`). In category compare (e.g. inference-library: `baseline=vllm`,
`experiment=sglang`), the per-side queries produce series whose `source=` label
intentionally differs. The match-key logic had to strip "capture-identity dims" from
intersection — otherwise baseline `{source=vllm, quantile=0.5}` never matched
experiment `{source=sglang, quantile=0.5}`. Separate from this: the initial
`extractBaselineCapture` vs `extractExperimentCapture` paths used different label
extraction strategies (one read `spec.series_names`, the other read `item.metric`
via `canonicalQuantileLabel`), producing heterogeneous key sets even before the
identity-dim problem. Both bugs were latent until category-mode compare was tested.

**Post-launch simplify sweep caught non-trivial dead code.** After #820 landed, three
parallel review agents swept `compare.js` and adjacent files. The ~19 follow-up
commits on `refactor/compare-mode-followups` removed `scrubStaleLegend`,
`paletteSig`, and several redundant branches. The user verified heatmap legends
visually (they now mount inside `chart.domNode`) before the branch merged. PRs #821
and #822 absorbed the highest-priority items.

**Dead baseline anchor plumbing (Q7) remains inert.** `selectionStore.anchors.baseline`
is migrated, persisted, and ignored — no UI ever writes it. Dropping it would touch
5+ files and migration tests for zero behavior change. Leave it until a baseline
anchor drag UI ships. (Memory: `project_compare_mode_refactor_followups.md`)

## Deferred / Reopen

**N-way compare (N > 2).** The architecture (`CaptureRegistry`, `capture=` query
param, `alias=path` positional syntax, URL param schema) was designed to generalize.
v1 assumes two slots. When a third capture slot materializes, the wire-stable
`baseline`/`experiment` ids will need to become positional or named-but-open;
the alias design already handles the cosmetic side. See
`feedback_compare_mode_n_way_extension.md` for the design constraint.

**Hot-swap (replace one capture while keeping the other).** Out of scope for v1; no
architecture obstacle.

**Live-agent compare (file + live, or live + live).** Explicitly excluded. Would
require the capture slots to support a `Tsdb` updated by a running agent rather than
loaded once from parquet. No near-term demand.

**Alias collision in saved tarballs.** When both sides fall back to the same filename
basename, the compare badge shows two identical labels. `synthesize_ab_manifest`
doesn't deduplicate. Decided not to mitigate — let the user rename. Documented in
#960 release notes.

**Baseline anchor drag UI.** The selection state already carries `anchors.baseline`;
anchoring the baseline to something other than its first sample has no UI yet.

## Cross-References

- Base quantile-heatmap rendering (Full/Tail spectrum charts, single-capture) belongs to [viewer chart UX](2026-04-19-viewer-chart-ux.md).
- Notebook, Selection, and Report persistence machinery belongs to [selection-notebook-report](2026-05-10-selection-notebook-report.md).
