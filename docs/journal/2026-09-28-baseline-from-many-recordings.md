# A baseline built from many recordings

- **Opened:** 2026-09-28
- **Status:** BUILT (#1331), as a viewer setting rather than a CLI
  selector; see the corrections under the design. The load gate was
  re-measured on a release build in the follow-up PR and holds at 1.2×.

## Problem

Compare mode answers "is B different from A". The question a perf engineer
asks after the fifth nightly run is "is tonight's run different from the last
twenty", and the viewer has no way to pose it. What exists:

- N-way overlay draws every recording of a multi-recording `.rez` as its own
  line (`overlayLine` in `src/viewer/assets/lib/charts/compare.js`, the
  backlog's "N-way overlay is functionally complete"). Twenty lines are not a
  baseline; they are twenty lines, and the eye picks the one that wandered.
- Diff, side-by-side, and spectrum views are gated to exactly two captures.
- Per-capture envelopes and the divergence band (`line.js`, PR #1006) exist
  for the two-capture case.
- The band-views entry decided that a spread band ("what happened") and a
  measurement band ("what we can claim") are distinct views and are never
  overlaid.

A regression that is one sigma outside twenty prior runs is invisible in a
twenty-line overlay and is exactly what a nightly job needs to flag.

## Goal

A compare mode where the baseline slot is a **set** of recordings and the
experiment is one. The baseline draws as a statistic band; the experiment
draws as a line over it. A later step lets a check say the experiment left
the band.

## Design

**The baseline is a set, selected the way a recording is selected today.**
`rezolus view fleet.rez --baseline arm=nightly --experiment run=2026-09-28`
already uses selector semantics (repeatable, ANDed, subset match). Today a
selector must match exactly one recording or the run is refused. The change:
`--baseline` may match many, and the viewer treats the match set as the
baseline family. `--experiment` keeps the exactly-one rule. A family of one is
today's compare mode, so nothing existing changes shape.

*Correction, when built:* the selector was left alone. The N-way overlay
already attaches every recording of a multi-recording archive as a named
capture on both backends, so the family needs no new backend state: it is
**every attached capture except the experiment**, chosen by a "Baseline"
menu in the compare badge (single capture, or family with the band shape),
and the CLI's exactly-one rule for `--baseline`/`--experiment` still holds.
That keeps the server and WASM viewers in parity by construction (nothing
derived from the archive changed) and drops the whole `.rez`-selection
surface from the change. A CLI form that names the family by label stays a
backlog item; today the family is every attached capture but the
experiment.

**Align every member before aggregating.** Each member is rebased to relative
time by its own anchor, which is why this depends on
[event-anchored alignment](2026-09-28-events-ranges-and-alignment.md):
twenty runs that started at twenty wall-clock instants only stack if each is
zeroed on its own `run_start`. Without an event anchor, first sample is the
fallback and the band will be smeared by start-up jitter. The entry records
that as expected, not as a bug.

**Bucket to a common grid, one value per member per bucket.** Each member is
decimated onto the same relative-time grid before aggregation, so a member
recorded at 100 ms does not outvote one recorded at 1 s. The display-mode
decimation path already computes per-bucket median and min/max per capture
(`crates/dashboard` boxplot wire, PR #1006); the family statistic runs over
those per-member bucket values. Per bucket: mean, standard deviation,
minimum, maximum, and the member count (members do not all cover every
bucket).

**Two band shapes, user-chosen.** Mean ± kσ (default k = 2) for a "how
unusual" reading, and min/max envelope for a "has this ever happened"
reading. Both are spread bands in the band-views entry's sense and go in the
spread view; the measurement band stays in its own view. This is the one
decision from that entry that transfers unchanged.

**Where it computes.** In the frontend over the per-capture decimated wire,
as `compare.js` stitches today. The backends stay per-capture and need no
cross-capture join, keeping the A/B entry's "two independent TSDBs, stitched
in the frontend" property. The cost is N fetches per chart; at N = 20 and the
viewer's query volume that is the same trade the A/B entry accepted at N = 2,
and it is the first thing to measure.

**Member count in the tooltip.** A band over three members and a band over
thirty look the same and mean different things. The tooltip shows n per
bucket.

*Built (this PR).* The member count is in the legend, on the mean line's
name (`family mean ± 2σ (20 members)`), not per bucket in the tooltip;
per-bucket `n` is computed (`familyBand(...).n`) and left for the tooltip
work in the backlog. The band itself: `charts/util/family_math.js` is the
pure part (`resampleLinear` onto the first member's grid, null outside a
member's range or across a hole in it; `familyBand` with `sigma` as sample
sd needing two members per bucket, or `envelope`), node-tested.
`compare.js::overlayLine` builds it when the setting is on and three or
more captures are attached: the members are the rebased entries other than
the experiment, so event-anchored alignment applies to each; the spec
carries `familyBand` plus a two-entry `multiSeries` (the family mean in a
neutral hue and the experiment), and `line.js` draws the band with the
same stacked-fill renderer the divergence band uses. The setting lives in
`notebookStore.family` (`{kind, k}` or null, v3-additive through
`normalizeFamily`), rides the notebook and report payloads, and in the
link as `family=sigma:2` / `family=envelope`. With fewer than three
captures the setting is ignored and the plain overlay stands. Diff and
side-by-side views are untouched (two captures only, as before).

## Not in scope

- Storing the family statistic in the archive. It is a view over recordings
  that are already there; recomputing is cheap and avoids a second source of
  truth.
- Automatic family membership (label rules that add new recordings). A
  selector does this already.
- Diff or side-by-side heatmaps against a family. Two-capture only until the
  line case proves out.

## GO / NO-GO

Measure first: dashboard load time for a 20-member family on a real
multi-recording `.rez` in both backends. GO if the section renders within
twice the two-capture time. NO-GO on that number means the family statistic
moves into the backend as one fetch per chart, which is a larger change and a
new entry.

*Measured (server viewer, developer-mode build, headless Chrome, `#/cpu`,
time to network idle with charts mounted).* Archives assembled with
`recording combine` from the A/B parquet fixtures: 2 recordings, 4
recordings, and 20 copies of one recording.

| archive | first load | switch to family |
|---|---|---|
| 2 recordings | 1.3 s | n/a |
| 4 recordings | 1.7 s | 2.0 s |
| 20 recordings | 5.4 s | 2.0 s |

The gate as written fails on this debug-build measurement: 20 recordings
load in 4.1× the two-capture time (the release build, measured in the
follow-up below, gives 1.2×). But the number is not the family's. The switch to the family view
costs the same 2.0 s at 4 and at 20 members (most of it the fixed wait for
network idle after the redraw), and the aggregation runs over data the
page already holds. The 5.4 s is the N-way overlay's first load, which
predates this entry: `viewer_core.js` fetched the extra captures one at a
time per chart (`for (const cap of extras)` with three awaited requests
each: metadata, the range query, the display query), so a 20-arm archive
issued 54 sequential round trips per chart. The
NO-GO branch (move the statistic to the backend) would not touch that
cost. Verdict: GO for the family band; the load cost went to the backlog
as an item on the N-way fetch loop. Correctness gates held: 20 identical
copies produced a zero-width band with `19 members` in the legend, and
the 4-recording archive drew a min..max band around its three members
with the fourth as the experiment line.

The WASM viewer was not measured; the frontend is the same and the
per-capture calls are local, so its number can only be lower on the fetch
loop.

### The fetch loop was not the cost

The follow-up PR did what the backlog item said: `data.js` memoizes the
capture list and each capture's metadata for the life of a view
(`listCaptures`, `captureMetadata`; cleared by `clearMetadataCache` and
`clearViewerCaches`, which attach, detach and a file swap all reach), and
`fetchExtraCaptures` runs the per-capture fetches through a bounded pool
(`mapLimit`, four captures in flight, each capture's range and display
queries issued together). Request counts per `#/cpu` load on the
20-recording archive fell as expected: `/api/v1/metadata` 347 → 23,
`/api/v1/captures` 19 → 1. The 561 range queries are unchanged, since each
chart still asks each capture for its data.

Wall clock did not follow. Same harness, three fresh page loads per cell,
median, developer mode, headless Chrome, `#/cpu`, with the same three
files swapped between their `main` and PR versions under one server
binary (developer mode serves them from disk):

| archive | debug before | debug after | release before | release after |
|---|---|---|---|---|
| 2 recordings | 1.08 s | 1.09 s | 0.94 s | 0.96 s |
| 4 recordings | 1.23 s | 1.19 s | 1.00 s | 0.96 s |
| 20 recordings | 3.26 s | 3.20 s | 1.15 s | 1.09 s |

The first load of the 20-recording archive after the server starts is
slower in both trees (debug: 4.9 s before, 3.3 s after, one sample each),
which is what the earlier single-run 5.4 s measured. Widening the pool
from 4 to 18 left the debug 20-recording median at 3.20 s, so the client
is not what paces the load. `range_query` in `src/viewer/routes.rs`
evaluates each query inline on a tokio worker, so the server is not
serializing them either; the debug load is the query engine's throughput
over 561 evaluations, about 34 ms each with six browser connections in
flight.

The release build answers the gate. Twenty recordings load in 1.2× the
two-capture time, before this change as well as after it, so the 4.1× in
the table above was the debug engine, not the fetch loop and not the
family. **Gate: held**, on the build that ships. The memo and pool stay
for what they do measurably, 15× fewer metadata requests per load, and
because the sequential loop was wrong on its own terms. The backend-side
family statistic (this entry's NO-GO branch) is not needed for the gate
and stays unbuilt.

Correctness gate: a family of identical copies of one recording produces a
zero-width band (held: the 20-copy archive above). The design's second
gate, "a family of one reproduces today's compare mode", does not apply as
built: below three captures the setting is ignored and the plain overlay
stands, which a node test pins.

## Deferred / Reopen

- **A verdict from the band.** "The experiment left mean ± 2σ for longer than
  T" is a check in the sense of
  [checks with verdicts](2026-09-28-checks-with-verdicts.md), and lands there.
- **Family over heatmaps and percentile charts.** Reopen after the line case.
- **Backend family aggregation.** Reopen if the 2× gate fails on a
  release build; the debug-build 4.1× was the engine, not the fetch shape
  (see "The fetch loop was not the cost").

## Cross-references

- [A/B compare mode](2026-04-21-ab-compare-mode.md)
- [Viewer band system](2026-07-21-viewer-band-views.md): spread versus
  measurement views.
- [Display-mode decimation](2026-07-13-viewer-display-decimation.md): the
  per-bucket wire this aggregates over.
- [Multi-endpoint `.rez`](2026-08-28-multi-endpoint-rez-record.md): how a
  family gets recorded in one archive.
