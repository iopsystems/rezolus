# Viewer performance and JS restructure

- **Opened:** 2026-04-18
- **Status:** SHIPPED — merged (see PRs)
- **PRs:** #848, #851, #876, #892, #913, #915, #932, #933

## Problem

The viewer had two compounding problems: loading cost and maintenance cost.

On the loading side, the server and WASM producers materialized every dashboard
section body at startup, regardless of which section the user would actually
visit. Every section payload also re-embedded the full `sections` navigation
list, so the backend duplicated that metadata on every fetch. The frontend
fetched section bodies on demand (that change had already landed), but the
producer was still doing all the work up front, making the lazy client fetch a
hollow win.

On the maintenance side, `script.js` was duplicated: 747 lines in
`src/viewer/assets/lib/script.js` and a parallel 646-line copy in
`site/viewer/lib/script.js` sharing ~95% logic. `src/viewer/mod.rs` had grown
to 2610 lines mixing CLI entry, HTTP routing, app state, parquet metadata
extraction, and a dozen action handlers with no structural seam between them.
The JS module tree was a flat file dump — no domain grouping.

## Design

The consolidation design extracted shared dashboard logic from both `script.js`
copies into a new `app.js` exporting `initDashboard(config)`, leaving each
viewer with a thin bootstrap stub (~80–100 lines) that handles its own transport
and calls `initDashboard`. The stubs switch the viewer to hash-based routing;
the shared `app.js` mounts Mithril on `document.body`.

The chart-loading optimization was staged in three further layers: (1) split
navigation metadata from section content, (2) make section generation lazy on
the producer side, (3) a bounded route cache and heatmap payload improvements.
The staging was deliberate — the first two layers deliver load-time wins
independently; heatmap work was deferred.

## Delivery shape

### Phase 1 — metadata/content split (#848, merged 2026-04-29)

`GET /api/v1/sections` now serves the navigation list and global metadata
independently. Each section body response no longer repeats the `sections`
array. The frontend bootstraps shared navigation from this endpoint, then
fetches only the active section body (`overview`) at startup.

The `LazySectionStore` struct in `src/viewer/state.rs` (lines 37–83) holds a
`DashboardContext` (nav list + generation inputs) and an initially-empty
`HashMap<String, serde_json::Value>` of cached bodies. Handlers call
`get_or_generate(route, data)` which generates a section body on first request
and memoizes it thereafter. The nav endpoint reads `.sections()` directly from
the context without touching `cached_bodies`.

### Phase 2 — lazy section generation (#851, merged 2026-04-30)

The `dashboard` crate's `generate()` function, which returned a
`HashMap<String, String>` of every section up front, was replaced with two
narrower functions:

- `build_dashboard_context(...)` → `DashboardContext` — produces the nav list
  and the shared inputs needed for any section generation.
- `generate_section(data, route, ctx)` → `Option<View>` — generates one section
  on demand; returns `None` for unknown routes instead of panicking.

The server's `AppState` now holds a `RwLock<LazySectionStore>` instead of a
pre-materialized map of all section JSON. The WASM viewer mirrors the same model
in `crates/viewer/src/lib.rs`. Both paths stop paying the full build cost at
startup.

### mod.rs decomposition (#892, merged 2026-05-10)

The 2610-line `src/viewer/mod.rs` was split along its natural seams:

- `mod.rs` (~949 lines now): entry, `command()`, `Config`, `run()`, `serve()`
- `state.rs` (~364 lines): `AppState`, `LazySectionStore`, `ProxyState`,
  `ApiResponse`, `CaptureParam`
- `metadata.rs` (~576 lines): parquet metadata extraction, multi-node info,
  checksum, service-extension validation, dashboard regeneration
- `routes.rs` (~472 lines): `app()` router + read-side handlers
- `actions.rs` (also present): mutating handlers and `ingest_loop`
- `capture_registry.rs` (~200 lines): capture registry

A `run_query` helper consolidated duplicate capture-lookup + `QueryEngine` setup
that had been copy-pasted across query handlers.

### JS consolidation into app.js

`src/viewer/assets/lib/script.js` was rewritten from 747 lines to 418 lines
(current) as a server bootstrap stub. The shared dashboard logic moved into
`src/viewer/assets/lib/app.js` (currently 1131 lines, grown with subsequent
features). `site/viewer/lib/app.js` is a symlink to the shared file; the site's
`script.js` is a standalone WASM bootstrap stub. Both viewers now use hash-based
routing (`m.route.prefix = '#'` set inside `initDashboard`).

One implementation detail worth noting: ES module named exports are live bindings
for `let` declarations, but the server stub's live-refresh callback needed to
read the current value of `activeCgroupPattern` and `heatmapEnabled` from
`app.js`. The plan addressed this by exporting getter functions
(`getHeatmapEnabled`, `getActiveCgroupPattern`) rather than raw variables to
avoid aliasing surprises.

### Comment hygiene (#876, merged 2026-05-09; #933, merged 2026-05-16)

#876 swept 63 Rust files, removing ~476 lines of comments that restated what the
adjacent code already said (e.g., `// load config from file` above a `let config
= ...` line). Section headers with rationale were kept. #933 did the same pass
over viewer JS files, removing decorative dividers and narrating comments while
keeping browser/echarts quirk explanations and invariant comments.

### Lit chart spike + CSS tokens + charts.css extraction (#913, merged 2026-05-12)

Chart CSS was extracted from `style.css` into a standalone `charts.css` served
separately in both viewers. CSS custom properties (tokens) were introduced for
chart sizing and color values. A minimal `<rezolus-chart>` web component was
sketched using Lit 3.2.1 (16 KB self-contained ESM, vendored into
`src/viewer/assets/lib/lit/`), proving the path to embeddable charts that
consume the `crates/dashboard/` Plot descriptor shape with inline series data.

### `<rezolus-chart>` embed component (#915, merged 2026-05-17)

`src/viewer/assets/lib/embed/rezolus-chart.js` (138 lines) implements
`<rezolus-chart>` as a shadow-DOM custom element that calls the viewer's real
`Chart` via `configureChartByType`, so it renders identically to an in-viewer
chart. The component links `charts.css` into the shadow root and overrides only
the collapse-on-no-data rule (keeping a visible placeholder for embeds). It
needs `echarts` and `window.m` (Mithril) as host globals. A `demo.html` in
`embed/` exercises it standalone.

### JS domain subdirectory restructure (#932, merged 2026-05-16)

The flat `src/viewer/assets/lib/` module tree was reorganized into domain
subdirectories: `charts/`, `embed/`, `lit/`, `sections/`, `selection/`,
`events/`, `ui/`, and `features/`. Core orchestration files (`app.js`,
`data.js`, `viewer_core.js`, `viewer_api.js`, `script.js`) stayed at the root
because `index.html` references them directly and they have standalone
site-viewer counterparts. The `site/viewer/lib/` symlink tree was regenerated to
mirror the new layout; all relative imports were rewritten by path resolution.

## Known remaining bug / deferred work

**`LazySectionStore` never invalidates in live mode** (recorded 2026-06-21,
`project_server_section_cache_in_live_mode.md`).
`get_or_generate` in `src/viewer/state.rs:66–82` memoizes section bodies in
`cached_bodies` on first request. The cache is only cleared by replacing the
entire `LazySectionStore` via `LazySectionStore::new(...)` — which happens at
startup, upload, agent-connect, and dashboard regenerate, but never during the
live-mode ingest loop. In practice the section *structure* (groups, subgroups,
plot definitions, KPI lists) doesn't change mid-session, so users rarely see
impact. Chart *data* comes from live PromQL queries that bypass the cache
entirely, so data updates are unaffected.

The cleanest fix is a per-route bypass in the `routes.rs` data handler keyed on
`state.live.load(...)`: live → read lock + a new `generate_fresh` method that
does the generation work without writing into `cached_bodies`; file → write lock
+ `get_or_generate` (unchanged). Reopen when the live-view no-update bug is
being addressed or a session observably freezes section structure mid-flight.

## Heatmap payload improvements

Phases 3–4 from the optimization design (bounded route-cache eviction, dedicated
heatmap-cell response type, optional producer-side binning, compare diff
responses) were not implemented in this arc. The intended frontend cache model
was: one long-lived navigation-metadata object; a bounded LRU section-body cache
(keep active route + overview + last 2–3 visited); a heatmap cache keyed by
section + granularity state, evicted alongside section eviction. The heatmap
payloads were to evolve from generic triples → explicit `heatmap_cells` shape →
optional producer-side binned response → compare diff response (experiment −
baseline cells, avoiding retaining two full-resolution sides in the browser).
Reopen when heatmap performance is the next bottleneck or when the section-cache
eviction bound matters (today the cache grows with visited sections and is
cleared only on file/agent change).
