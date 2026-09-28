# A baseline built from many recordings

- **Opened:** 2026-09-28
- **Status:** OPEN — design, nothing built.

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

Correctness gate: a family of identical copies of one recording produces a
zero-width band; a family with one member reproduces today's compare mode
pixel for pixel (the `viewer-render` skill).

## Deferred / Reopen

- **A verdict from the band.** "The experiment left mean ± 2σ for longer than
  T" is a check in the sense of
  [checks with verdicts](2026-09-28-checks-with-verdicts.md), and lands there.
- **Family over heatmaps and percentile charts.** Reopen after the line case.
- **Backend family aggregation.** Reopen on the NO-GO above.

## Cross-references

- [A/B compare mode](2026-04-21-ab-compare-mode.md)
- [Viewer band system](2026-07-21-viewer-band-views.md): spread versus
  measurement views.
- [Display-mode decimation](2026-07-13-viewer-display-decimation.md): the
  per-bucket wire this aggregates over.
- [Multi-endpoint `.rez`](2026-08-28-multi-endpoint-rez-record.md): how a
  family gets recorded in one archive.
