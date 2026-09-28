# Events as ranges, phases, and alignment anchors

- **Opened:** 2026-09-28
- **Status:** OPEN — design, nothing built.

## Problem

Three things an event could do for a recording, and what stands in the way
of each, verified against the tree on 2026-09-28.

**1. An event cannot be drawn as a range.** `crates/dashboard/src/events.rs`
declares `duration_ns` on `Event` and its doc comment says a present value
renders as a band. Nothing renders it: `buildMarkLine` in
`src/viewer/assets/lib/charts/event_markers.js` reads only `timestamp` and
`description`, and `duration_ns` does not appear anywhere under
`src/viewer/assets/lib/`. The viewer's add-event form
(`src/viewer/assets/lib/events/event_form.js`) collects timestamp,
description, kind, source, node, instance, and never a duration. So the
field is a promise the storage makes and the viewer does not keep. A
benchmark's warm-up phase, a deploy that took forty seconds, or an incident
window all have to be two point events today.

**2. The recorder knows the run boundaries and records nothing.**
`rezolus record -- ./benchmark` records for exactly the wrapped command's
lifetime (`src/recorder/child.rs` maps the exit status), and `--duration`
bounds a run. Neither emits an event. The recording starts when the recorder
starts, which is before the command's first useful work, and the viewer has no
marker for where the command began or ended.

**3. Compare-mode alignment is manual.** In compare mode each capture's
anchor is a signed millisecond offset from that capture's first sample
(`anchorSecondsFor` in `src/viewer/assets/lib/charts/compare.js`). The user
drags the experiment's offset until the traces look aligned; the baseline
anchor is stored and never written by any UI (see the A/B compare entry,
"Dead baseline anchor plumbing"). There is no way to say "align both arms on
their `run_start` event", which is the alignment every benchmark comparison
actually wants, and the one a person cannot eyeball to better than a few
seconds.

## Goal

- A range event renders as a band, in the viewer and in the add-event form.
- `record` emits `run_start` and `run_end` events for a wrapped command.
- A compare anchor can be an event kind; each capture resolves its own event.

## Design

**Range rendering.** `buildMarkLine` gains a sibling `buildMarkArea` that
emits an echarts `markArea` for each event with `duration_ns`, in the same
color at lower opacity. The HTML bubble that carries the description
(`chart.js::_renderEventBubbles`) anchors at the range start. The form gains
an optional end timestamp; the CLI's `--event` inline syntax gains
`duration=` alongside `timestamp=`/`kind=`/`description=` (parsed in
`src/parquet_tools/annotate.rs`). No schema change: the field exists and is
already optional and serde-defaulted, so older readers keep working.

**Phase events are a kind convention, not a type.** `kind` is documented as a
free-form tag. `run_start`, `run_end`, `warmup`, `steady` join the documented
examples in the `Event` doc comment. No validation is added; the value of a
convention here is that the alignment feature below can name a kind.

**Recorder-emitted run events.** When `config.command` is set
(`src/recorder/mod.rs`), the recorder adds a `run_start` event at the
instant it spawned the child and a `run_end` event at the instant the child
exited, with `details` carrying the command line and, on `run_end`, the exit
status. Written through the same manifest key (`KEY_EVENTS`) the `annotate`
path uses, so the viewer needs no change to show them. Two questions to
settle before building:

- The command line may contain things the user does not want in a file they
  will share (paths, tokens in arguments). Default to recording `argv[0]`
  only, with the full line behind a flag.
- Clock: the event instant must come from the same clock as the row
  timestamps, which is the recorder's wall clock at the tick that observed
  the spawn, not the child's own start time.

**Event-anchored alignment.** Alongside the numeric offset, an anchor may be
`{ kind: "run_start" }`. At render time `anchorSecondsFor` resolves it per
capture: the first event of that kind in that capture's events becomes the
capture's zero. A capture with no such event falls back to its first sample
and the compare strip says so. The `--baseline`/`--experiment` selectors on
`rezolus view` are recording selectors and stay that way; the anchor is view
state, and rides in the link once
[viewer links carry state](2026-09-28-viewer-link-state.md).

This also gives the baseline anchor its first writer, closing the inert
`anchors.baseline` plumbing the A/B entry left standing.

## Not in scope

- Editing or deleting events in the viewer. Already an open backlog item;
  unchanged by this.
- Multiple events of the same kind per capture as alternative anchors. First
  match only until a case needs otherwise.

## GO / NO-GO

GO for each piece independently; they share a data model but not a critical
path. Range rendering and the CLI `duration=` key are the smallest and land
first. The recorder events need the two questions above answered in the PR.
Event-anchored alignment is gated on a browser check (the `viewer-render`
skill) that two captures with `run_start` events offset by a known amount
overlay to within one sample.

## Deferred / Reopen

- **Events from the agent itself** (sampler degraded, PMU lost, config
  reload). The agent's `/status` knows these transitions; a recorder could
  turn them into events. Reopen when the status endpoint carries a
  transition log rather than current state.
- **Events as query-visible series.** A rule engine or a query could want
  events as a series to join against. Out of scope until
  [checks with verdicts](2026-09-28-checks-with-verdicts.md) needs it.

## Cross-references

- [A/B compare mode](2026-04-21-ab-compare-mode.md): anchor semantics and the
  inert baseline anchor.
- [Retire the `.parquet.ab.tar` container](2026-08-27-retire-ab-tarball.md):
  events moved into the `.rez` manifest.
