# Measurement uncertainty — Phase 1: observation acquisition windows

- **Opened:** 2026-07-10
- **Status:** OPEN — design landed, pre-build. Phase 1 of the
  [measurement-uncertainty arc](2026-07-08-measurement-uncertainty.md).
- **Owner:** Brian Martin
- **Repos:** most of the change lands in **metriken**
  (`~/workspace/metriken`, branch `next`); rezolus is the first consumer and
  builds against the local checkout via a dev-only `[patch.crates-io]` override
  (uncommitted; lives on the Phase-1 branch only).

This entry is the Phase 1 design spec (absorbs the brainstorm; no separate doc).

## Goal

Validate the honest-window premise end-to-end on the pilot cohort: capture
`drivehealth`'s real **per-device acquisition windows** `[t_begin, t_end]`, carry
them through the metriken *format* as first-class **observations**, and make them
**visible on the live snapshot** (`/metrics/json`). This drops the
unified-timestamp myth for the one cohort that most needs it, and proves the
plumbing before Phase 2 (archive) and Phase 3 (rate error bars).

We design the **full observation shape** (extensible for `epoch`/`quantum`) but
**implement only the window** (+ derived `kind`).

## Scope

**In:** the acquisition-window primitive in the metriken format; an optional
per-index window store on metriken groups; `drivehealth` per-device window
capture; a `SnapshotV3` carrying optional per-observation windows, visible on
`/metrics/json`.

**Out (later phases):** parquet/archive columns for windows (**Phase 2**);
`start_epoch`, value-quantum, HZ discovery — the rate-error-bar machinery
(**Phase 3**); cross-host clock uncertainty (**Phase 4**); the full value-quantum
model (**Phase 5**). The *mechanism* here is designed so those slot in as
non-breaking additions (below), so we are not building them early.

## Layering decision (why the format, not the core)

Measurement provenance is a high-resolution *systems-telemetry* concern, not a
general application-metrics concern (a general metric is an in-memory read —
window irrelevant, value exact, correlation coarse). metriken is a general
library (Pelikan/cache-server lineage). So provenance must **not** enter
metriken-core's read API, or it taxes every metriken user. But putting it in the
metriken **format** is what makes producing/analyzing parquet uniform across the
whole stack — which is the reason metriken is worth using everywhere.

Resolution — the concern lives at the format layer, opt-in and additive:

- **metriken-core / metriken read API** — untouched, general, fast. No ripple to
  `metriken-query` or external users.
- **metriken-exposition = the common format** — a snapshot entry becomes an
  `Observation` (value + optional window). Value and provenance are one
  inseparable unit *at the recorded-measurement boundary*, where provenance
  matters.
- **metriken group types** — an optional, additive per-index window store.
  General group users never set it and pay nothing.

Payoff: any metriken-based system emits the same format; simple systems carry the
trivial case (fleet window, exact values), rezolus carries rich per-device
windows, and one set of tooling analyzes all of them.

## Design

### 1. Format / wire shape (metriken-exposition)

- `systemtime` + `duration` on the snapshot **is** the default **fleet window**
  `[systemtime, systemtime + duration]`. Every observation inherits it unless it
  overrides. So the ~95% co-sampled/general metrics cost nothing new and read
  exactly like today.
- Add an **optional per-observation window override** on the `Counter` / `Gauge`
  / `Histogram` entry structs — present *only* when it differs from the fleet
  window (drivehealth per-device reads). Absent = inherit fleet.
- A V2 file is just a V3 where nothing overrides → old files analyze uniformly
  (every V2 observation uses the fleet window). General producers can keep
  emitting today's bytes.
- **To pin in implementation:** the exact serde mechanism for back-compat — an
  optional field on the existing shape vs. a distinct `SnapshotV3` variant in the
  `#[serde(untagged)] enum Snapshot`. Untagged + optional fields has ordering
  subtleties; choose whichever cleanly lets a V2 reader still deserialize and a
  V2 producer still emit unchanged bytes. The *semantic* model above is fixed.

### 2. Group per-index window store (metriken-core + metriken)

- `metriken-core`: a `Window { begin_ns: u64, end_ns: u64 }` type, and a
  `window_snapshot() -> Vec<(usize, Window)>` method on the group metric traits
  (`CounterGroupMetric` / `GaugeGroupMetric` / `HistogramGroupMetric`) with a
  **default impl returning empty** → non-breaking for every existing implementor.
- `metriken`: `CounterGroup` / `GaugeGroup` store per-index windows (a lock+map
  keyed by index, mirroring the existing `GroupMetadata`; drivehealth writes ~23
  entries per 60 s, so contention is a non-issue), exposed via
  `set_window(idx, begin, end)` / `load_window(idx)` and the trait's
  `window_snapshot()`.
- Ordering: the producer sets the window then the value; a racing snapshot sees
  at worst a one-refresh skew between them — benign (both change together, 60 s
  apart) and commented as such (principle 14).
- Representation note: lock+map is right for Phase 1's low write rate; a dense
  atomic representation is a later refinement if a high-rate windowed group ever
  appears (YAGNI now).

### 3. The `Observation` shape (honoring the wider scope)

A `metriken-exposition` snapshot entry becomes `Observation = value +
window: Option<Window>`.
- **`kind` is derived, not stored** — Gauge → point, Counter/Histogram →
  cumulative — since the reader already knows `metric_type`. No new field.
- **`epoch` / `quantum` are NOT pre-added as empty fields.** Getting the shape
  right means the *mechanism* generalizes, not that we reserve unused fields:
  entry fields are optional and additive, and groups have a general per-index
  annotation store, so `epoch`/`quantum` land in their phase as non-breaking
  additions with zero rework. This also avoids pre-committing `quantum`'s
  representation, which is genuinely unsettled (histograms carry their quantum in
  the H2 config, not as a scalar).

### 4. drivehealth capture + snapshot read path (rezolus)

- **Capture:** wrap each device read with `t_begin = SystemTime::now()` before the
  ioctl and measure width with a monotonic `Instant`; `t_end = t_begin + elapsed`.
  This places the window on the wall clock (comparable to the fleet window) while
  being immune to an NTP step mid-read (width from the monotonic clock). All of a
  device's metrics (temperature + throttle counters) come from one ioctl → **one
  window per device**, written to each of that device's group indices via
  `set_window`.
- **Read:** rezolus `src/agent/exposition/http/snapshot.rs::create()` builds
  `SnapshotV3` — for each group it reads value + `window_snapshot()`; a present
  window emits the override, absent inherits fleet. The window then appears on
  `/metrics/json`.
- Note: rezolus hand-rolls the snapshot rather than using metriken-exposition's
  `Snapshotter`; decide in implementation whether to also teach the `Snapshotter`
  about windows (for other metriken consumers) or leave that to Phase 2.

## Change set

- **metriken-core:** `Window` type; `window_snapshot()` on the three group traits
  (default empty).
- **metriken:** per-index window storage + `set_window`/`load_window` on
  `CounterGroup`/`GaugeGroup`; impl `window_snapshot()`.
- **metriken-exposition:** `SnapshotV3` (or optional window fields) on the entry
  structs; (de)serialization; accessors; V2 back-compat.
- **rezolus:** drivehealth per-device window capture; `snapshot.rs::create()`
  emits `SnapshotV3` reading `window_snapshot()`. Dev `[patch.crates-io]` override
  already wired.

## Validation / GO criteria

On a host with drivehealth-visible drives (NVMe, or SATA via the pass-through
path from #992), running the agent:
- `/metrics/json` shows a per-device window on each `drive_temperature` /
  `drive_thermal_*` observation, with `t_end − t_begin` ≈ the measured read
  latency (ms–seconds) and **distinct per device**.
- Co-sampled metrics carry **no** override (inherit the fleet window); their bytes
  are unchanged.
- A V2 consumer still deserializes the snapshot (back-compat holds).
- **Overhead measured** (principle 16): the fleet path adds nothing per-metric;
  the only new cost is drivehealth's per-index windows (~23 × a few u64 per 60 s).
  Report the number.

## Testing

- metriken-exposition: `SnapshotV3` msgpack + json round-trips, with and without
  window overrides; a V2 fixture still deserializes.
- metriken groups: `set_window` / `load_window` / `window_snapshot` unit tests.
- rezolus: extend the ignored drivehealth hardware test to assert per-device
  windows are captured; an integration test asserting windows appear on the
  built snapshot.

## Fit with the arc / principles

- Implements the arc's Phase 1 (temporal-first), scoped to windows + derived
  `kind`; the shape is extensible for the deferred `epoch`/`quantum`.
- Principle 10 holds (consumers drive read cadence; the window is recorded when
  the sampler reads, not by an agent-side clock); drivehealth is the sanctioned
  principle-17 case.
- Principle 16: overhead is measured, not asserted; the general fleet is untaxed
  by construction.
