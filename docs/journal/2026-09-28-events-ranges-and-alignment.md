# Events as ranges, phases, and alignment anchors

- **Opened:** 2026-09-28
- **Status:** OPEN — range rendering BUILT (#1322); recorder run events
  BUILT (this PR); event-anchored alignment not yet started.

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

*Correction, found while building:* the CLI half of this gap did not exist.
`parse_inline_event` (`src/parquet_tools/events.rs`) already accepted
`duration=<humantime>` and `duration_ns=<int>`, and JSON input accepted
both spellings; what was missing was a test and a mention in the `--event`
help. The original text below says the CLI "gains `duration=`"; it gained a
test and help text. Also found: compare-mode charts draw a relative axis
(`compare.js::rebase`) but `_applyEventMarkers` placed events at absolute
epoch ms, so every marker landed off-grid in compare mode; and Notebook
bubbles already offer Delete (`chart.js::_renderEventBubbles` →
`openEventInfo`), so the backlog's "read-only after creation" was stale.

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

*Built (this PR), with one change from the design.* The design called for
an echarts `markArea`. That was built first and it never rendered on the
viewer's heatmaps: they
are `custom` series (`heatmap.js`, `histogram_heatmap.js`,
`quantile_heatmap.js`), and on those echarts collapsed the area to the axis
line in every configuration tried in a headless browser (`z`, `zlevel`,
explicit `yAxis` bounds on a category axis, an `encode` declaring the x/y
dimensions), while the same option drew correctly on a `line` series. The
band is an HTML overlay instead: `buildRangeSpans(events, toAxisMs)` in
`charts/event_markers.js` is the pure part (one `{startMs, endMs, name}` per
event with a positive `duration_ns`), and `chart.js::_renderEventBubbles`
draws each span as a `div.event-range-band` sized to the plot grid through
`convertToPixel`, clipped to the grid, in the same layer as the description
tags and on the same zoom/resize/store re-render path. It works on every
chart type identically, which the markArea route never would have. A range
event keeps its start hairline from `buildMarkLine` (the bubble's anchor),
and the tag carries the humanized duration (`formatDuration`, e.g.
`warm-up (1m30s)`).

Both builders take a `toAxisMs` conversion, and the chart supplies
`_eventAxisMs`, which subtracts `spec.eventTimeOriginSec` when present:
`compare.js` sets that field on every relative-axis spec it builds
(overlay, side-by-side per slot for both the plain and the quantile
heatmap pairs, both diff heatmaps, split lines) to the capture anchor the
axis was rebased on, so events land where they belong in compare mode
instead of at absolute epoch ms. The first build missed the two quantile
builders. The add-event form gained an
optional End (RFC 3339, must be after Timestamp; `duration_ns` is derived),
and the info popover shows End, Duration and Details. Events remain one
baseline-scoped list; the overlay and split charts place them by the first
capture's anchor, which is right for that capture's events and is what the
per-capture context in the alignment work refines.

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

*Built (this PR).* Both questions were answered as proposed. `description`
is the basename of `argv[0]` (`bench.sh`), and the full argument list,
joined by spaces, goes into `run_start`'s `details` only with
`--record-command-line`. Both instants come from the recorder's own clock,
`anchored_at(clock_anchor_wall_ns, clock_anchor_mono.elapsed())`, the same
function that stamps the rows. The `.rez` writer needed a new message for
this: metadata was set once in the `ManifestSeed` at `add_recording`, and
`finalize` never touched it, so `Msg::UpdateMetadata` and
`RecordingWriter::update_metadata` (`crates/rez/src/rez_v3_writer.rs`) now
replace a recording's whole map through the writer thread, which owns the
only writing connection; `RezStream` keeps each recording's last map so it
can send it back with an event merged in. The recording loop gained a
`select!` arm on `child.wait()` so the child's exit wakes the loop and
`run_end` is stamped when the exit happened rather than at the next tick;
the loop top's `try_wait` still takes the decision, since tokio caches the
status once `wait` has completed. Ids are `run:<uuid>:start` and
`run:<uuid>:end` with one v4 uuid per `record` invocation (minted by the
same `epoch::mint` the producer epoch uses), so `combine`'s dedup by id
keeps one pair per run. Parquet output gets the same payload from
`build_parquet_converter` under `KEY_EVENTS`; raw output has no metadata and
cannot carry them, and the help says so. An endpoint that joins after the
spawn gets `run_start` in its seed through `build_rez_metadata`'s new
`run_events` argument. The path that discards a `.rez` when the command
exits before any sample is unchanged: `run_end` is merged only in the
finalize block, after that check.

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
