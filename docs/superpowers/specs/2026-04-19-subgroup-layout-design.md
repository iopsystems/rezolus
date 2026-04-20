# Subgroups and Plot Widths in Dashboard Layout

**Status:** Design
**Date:** 2026-04-19

## Problem

Within a section group, charts flow into a 2-column CSS grid
([style.css:947-949](../../../src/viewer/assets/lib/style.css#L947-L949)). The
layout has no way to express:

1. "These charts belong together; wrap them as a visual cluster."
2. "Start this chart on a new row."
3. "If the per-device view would only show one device, collapse the pair to a
   single full-width chart."

Authors currently have no option short of injecting CSS, which couples
dashboard authorship to viewer internals.

## Goals

- Let dashboard authors express semantic sub-clustering within a group.
- Let authors declare a chart's width (half or full) so single charts can span
  a row.
- Let authors write cardinality-conditional layouts at dashboard generation
  time, using the materialized Tsdb they already hold.

## Non-goals

- No JS-side reactive layout. Layout decisions are baked into the JSON when
  `dashboard::generate()` runs.
- No new column counts beyond 2 (today's grid). A subgroup uses the same
  2-column grid as its enclosing group. Future subgroups could parameterize
  this; not now.
- No collapsible/interactive subgroups. A subgroup is a layout cluster only.
- No dedicated `plot_pair()` helper. The primitives compose; if a call-site
  pattern recurs, a helper can be added later.

## Data model

### Rust (`crates/dashboard/src/plot.rs`)

```rust
pub enum PlotWidth {
    Half,   // default; spans 1 of 2 columns
    Full,   // spans both columns (and on narrow screens, still full width)
}

pub struct SubGroup {
    // Optional header, rendered above the subgroup's grid.
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,

    // Optional prose explanation of what the subgroup is showing — e.g.,
    // "Summary aggregates across all devices; per-device breaks it down
    // when more than one device is present." Rendered under the name as
    // secondary-weight text. Exists now because subgroups are intended to
    // evolve into self-contained "cards" with their own interactivity and
    // layout (collapsibility, column-count overrides, inline controls);
    // explanatory text is one of the first affordances that kind of card
    // will need, and putting it in the data model now avoids a schema
    // break later.
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,

    plots: Vec<Plot>,
}

pub struct Group {
    name: String,
    id: String,
    subgroups: Vec<SubGroup>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    #[serde(default)]
    metadata: HashMap<String, serde_json::Value>,
}
```

`Plot` gains:
```rust
pub struct Plot {
    // ... existing fields
    #[serde(skip_serializing_if = "is_half", default = "PlotWidth::default")]
    width: PlotWidth,
    // ...
}
```

`PlotWidth::Half` is the default and is elided from JSON (via a serde
skip predicate) to keep existing dashboard output byte-identical for
unchanged layouts.

### JSON shape

```json
{
  "name": "Block IO",
  "id": "blockio",
  "subgroups": [
    {
      "name": "Throughput",
      "description": "Bytes/sec across all block devices, aggregated.",
      "plots": [
        { "opts": { ... }, "promql_query": "...", "width": "full" }
      ]
    },
    {
      "plots": [
        { "opts": { "title": "Operations (summary)" }, ... },
        { "opts": { "title": "Operations (per device)" }, ... }
      ]
    }
  ]
}
```

A subgroup with a single plot whose `width: "full"` renders as one wide chart.
A subgroup without a `name` omits the subgroup header but still occupies its
own vertical band — subgroups are stacked as block-level `div.subgroup`s, so
a new subgroup always starts a new row regardless of whether it has a name.

### Authoring API

`Group` exposes `subgroup(name: impl Into<String>) -> &mut SubGroup` and
`subgroup_unnamed() -> &mut SubGroup`. `SubGroup` exposes `plot_promql` /
`plot_promql_with_descriptions` (mirroring today's `Group` methods) plus
`plot_promql_full` for full-width plots, and `describe(text)` to set the
optional description.

```rust
let sg = group.subgroup("Operations");
let devices = data.counter_labels("blockio_operations")
    .and_then(|ls| ls.get("device"))
    .map_or(0, |v| v.len());
if devices == 1 {
    // Known single device — per-device chart would be identical to summary.
    sg.plot_promql_full(summary_opts, summary_q);
} else {
    // 0 (unknown / no data yet) or ≥2 — show both.
    sg.plot_promql(summary_opts,    summary_q);
    sg.plot_promql(per_device_opts, per_device_q);
}
```

The idiom is deliberately asymmetric: only the *exactly-known-to-be-one* case
collapses. Any other state — empty Tsdb, not-yet-populated, or multiple
devices — falls through to the expanded two-chart layout. This means debug
dumps (empty Tsdb) and live-mode first-connect (Tsdb still warming up) both
show the richer layout, and the collapse is reserved for the narrowly-scoped
"only one device ever observed" case.

The label-index lookup
(`Tsdb::counter_labels(name) -> Option<&LabelIndex>`) is already available and
is a cheap HashMap lookup — no query evaluation.

### Backward compatibility

`Group::plot_promql` and related methods keep working. Semantics:

1. The first call to `Group::plot_promql` (without a prior `subgroup()` call)
   lazily creates a default unnamed subgroup and appends the plot to it.
2. Subsequent `plot_promql` calls on the same `Group` append to that same
   default subgroup, so a group with only legacy calls emits one unnamed
   subgroup containing all the plots — visually identical to today.
3. Calling `group.subgroup(name)` or `group.subgroup_unnamed()` opens a new
   subgroup. After that, any further `plot_promql` call on the `Group`
   appends to the most recently opened subgroup. (Authors who want to mix
   legacy flat calls with new subgroup calls within the same group should
   prefer the subgroup API throughout for readability; the legacy fallback
   exists for not-yet-migrated call sites.)

No existing dashboard code needs to change.

## Viewer changes

### JSON consumption (`src/viewer/assets/lib/viewer_core.js`)

`createGroupComponent` currently iterates `attrs.plots.map(...)` inside a
single `div.charts` grid. It is updated to iterate `attrs.subgroups`, emitting
one `div.subgroup` per subgroup:

```js
m('div.group', { id: attrs.id }, [
  m('h2', attrs.name),
  attrs.subgroups.map((sg) =>
    m('div.subgroup', [
      sg.name && m('h3.subgroup-title', sg.name),
      sg.description && m('p.subgroup-description', sg.description),
      m('div.charts', sg.plots.map(renderChart)),
    ])
  ),
])
```

A compatibility shim in the same component accepts the legacy
`attrs.plots` shape (an array of plots at the group level) and promotes it to
a single unnamed subgroup, so any consumer not yet regenerated keeps working.
This shim is removed once the Rust side has been through one release.

`renderChart` reads `spec.width` and applies a `full-width` class when set.

### CSS (`src/viewer/assets/lib/style.css`)

```css
.group .subgroup { margin-bottom: 1rem; }
.group .subgroup-title {
  font-size: 0.95rem;
  font-weight: 500;
  color: var(--fg-secondary);
  margin: 0 0 0.25rem 0;
}
.group .subgroup-description {
  font-size: 0.85rem;
  color: var(--fg-secondary);
  line-height: 1.4;
  margin: 0 0 0.5rem 0;
}
/* .charts already has grid-template-columns: repeat(2, 1fr) */
.group .charts .chart-wrapper.full-width { grid-column: 1 / -1; }

/* Below 1200px, .group .charts already collapses to 1 column
   (style.css:2732-2736 and 2893-2896). `full-width` is a no-op there because
   `grid-column: 1 / -1` on a 1-column grid is identical to the default. */
```

### Other consumers

- `src/viewer/assets/lib/layout.js:142` counts plots via
  `group.plots` — updated to walk `group.subgroups[*].plots`.
- `src/viewer/assets/lib/section_views.js` renders groups via the shared Group
  component, so no direct changes apart from the few places that touch
  `group.plots` directly (a grep sweep during implementation).

### WASM viewer

The WASM viewer (`crates/viewer`) consumes the same JSON and uses the same JS
Group component, so no extra WASM code changes.

## Migration of existing dashboards

Existing dashboards in `crates/dashboard/src/dashboard/*.rs` keep compiling
without modification thanks to the backward-compatible `Group::plot_promql`
shim. Callers are migrated opportunistically as new subgroup layouts are
adopted (e.g., blockio, network, GPU). No mass rewrite is scheduled as part
of this change.

## Empty-Tsdb behavior

The debug binary (`cargo run -p dashboard`) calls `generate(&Tsdb::default(),
...)`. Under the new API, any `data.counter_labels(...)` lookup returns
`None` and `devices == 0`. With the recommended `== 1` idiom, that falls to
the expanded two-chart branch — so the debug dump shows the full layout,
which is what anyone inspecting dashboard JSON actually wants to see.

The collapse branch is reserved for *known-exactly-one-device* cases, which
are by definition observable only after the Tsdb has been populated. No
cardinality lookup should panic on an empty Tsdb — Option handling in the
example pattern covers this.

## Live-mode caveat

In live-agent mode, the Tsdb grows as new snapshots arrive. If a new device
label value appears after dashboard generation, the static layout decision
is not re-evaluated. With the `== 1` idiom, this is largely self-correcting:
live-mode dashboards generated before the first snapshot arrives see
`devices == 0` and fall to the expanded layout, so new devices appearing
later still populate the per-device chart. The only mis-step would be
regenerating the dashboard at the precise moment exactly one device had
been observed, then a second device appearing later — that would leave the
layout in the collapsed state with a second device hidden from the summary
view. Accepted as a known limitation — pair metrics in practice have
label sets that are stable within a session (NICs, block devices, GPUs).

## Testing

- Rust unit tests in `crates/dashboard/src/plot.rs`: subgroup serialization
  with and without names, `PlotWidth::Half` elision, backward-compat shim.
- Rust integration test: one dashboard built against an empty Tsdb and one
  built against a Tsdb with ≥2 label values, verifying the expected
  full-width vs two-half-width JSON output.
- Viewer JS: no runtime test harness exists today; manual verification:
  - Existing dashboards render unchanged in both server viewer and WASM
    viewer.
  - A dashboard using subgroups renders subgroup headers, full-width plots
    span both columns on wide viewports, and collapse to single column on
    narrow viewports.

## Out of scope / future

This spec lands the minimum viable subgroup: a header, a description, a flat
2-column grid of plots, and width overrides. The intent is that subgroups
will evolve into self-contained "cards" or "islands" with their own
interactivity and layout, built on top of the same data model:

- `plot_pair(summary, per_device)` helper that wraps the conditional pattern.
- Per-subgroup column count (`cols: 1 | 2 | 3`) or fully flexible per-card
  layout.
- Collapsible / expandable subgroups (header click toggles).
- Inline controls scoped to a subgroup (time-range override, unit toggle,
  filter by label value).
- Linked interactivity across plots within a subgroup (shared crosshair,
  synchronized zoom).
- JS-side reactive collapse for live mode.

The `description` field is part of the baseline data model precisely
because those future affordances benefit from having explanatory text
colocated with the card's metadata, and bolting it on later would be a
schema break.
