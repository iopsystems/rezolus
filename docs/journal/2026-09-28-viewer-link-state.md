# Viewer links that carry the whole view

- **Opened:** 2026-09-28
- **Status:** BUILT (this PR), with three corrections to the design below:
  the state lives in `location.search`, not the hash; the durable window is
  the range override, not `globalZoom`; the time-mode value is `raw`/`grid`.

## Problem

A viewer link does not reproduce what the sender was looking at. The hash
route carries the section and, for a single-chart view, the chart id
(`src/viewer/assets/lib/app.js`, the `'/:section/chart/:chartId'` route). The
rest of the view state lives in memory or in `localStorage`:

- the zoom range. *Correction:* the durable state is the query-range
  override (`_rangeOverride` in `data.js`, read by `defaultRangeFor` for
  every query and written by `applyDisplayWindow` in `app.js`); the visual
  `ChartsState.zoomLevel`/`globalZoom` is transient and cleared on every
  refetch. The link carries the override;
- the compare anchors (`notebookStore.anchors`, persisted by
  `src/viewer/assets/lib/selection/selection.js`);
- the pinned percentile set (`chart.pinnedSet`, per chart by decision, see the
  A/B compare entry);
- the time mode (Aligned/Raw, from #1023) and the display-mode band view;
- the cgroup, GPU and node filters (`src/viewer/assets/lib/features/`).

So the link a colleague pastes into a chat opens the right section at the
full recording span with default anchors. The time range that made the chart
interesting is exactly the part that is lost. The only page-level query-string
parameters today are `?compare=1` on the landing page
(`src/viewer/assets/lib/script.js`) and the static site's `?capture=` file
loader (the README's demo link; `capture=alias=path` since #832). Both say
which file to open, not what to show of it.

The same gap blocks two automation paths: the MCP server cannot hand back a
link to a finding, and a CI perf job cannot link a regression to the chart
that shows it.

## Goal

A link reproduces the view: section or chart, time range, compare anchors,
time mode, and active filters. A documented, stable parameter set that
scripts can construct without the frontend.

## Design

**Encode state in `location.search`, not the hash.** The design first said
"query parameters on the hash route". Building it showed why not:
`m.route.get()` returns the hash path *including* any query on it, and that
path is parsed by hand in several places (`route.replace(/^\//, '')` in
`app.js` and `script.js`, `split('/')` in `app.js` and the site's
`script.js`), so `#/cpu?from=` would become a section key and miss the
cache or request `/data/cpu?from=….json`. The query string avoids every one
of those: mithril's `setPath` (`mithril.js`) pushes a fragment-only URL
(`'#' + path`), which by URL resolution keeps the document's search, and its
`resolveRoute` reads only `location.hash`. So the search survives every
`m.route.set` and every sidebar `m.route.Link`, and no route parsing changes.
The static site's own canonicalisation (`site/viewer/lib/script.js`) builds
`new URL(window.location)` and touches only its keys (`capture`, `demo*`,
`category`), so the two compose.

```
http://127.0.0.1:4200/?from=2026-09-28T14:03:11.250Z&to=2026-09-28T14:05:40Z#/cpu
http://127.0.0.1:4200/?node=web-01&cgroup=/system.slice&cgroup=/user.slice#/cgroups
https://rezolus.com/viewer/?capture=demo.parquet&from=…&to=…&time=raw#/scheduler
```

The module is `src/viewer/assets/lib/ui/url_state.js` (symlinked into the
site), a pure core (`parseViewState`, `applyViewState`) plus `window`
wrappers (`readViewState`, `writeViewState`, `clearViewState`) that no-op
without a window so the modules importing it stay node-testable. It owns
only its keys and leaves every other parameter in place.

| key | written | read accepts | reads into |
|-----|---------|--------------|------------|
| `from`, `to` | RFC 3339 UTC with ms | that, or Unix seconds (fractional) | `setRangeOverride`, clamped to the recording via `baselineRange()` |
| `time` | `raw` only (grid is absent) | `raw`, `grid`, `aligned` | `currentTimeMode` + `setRateMode` |
| `node` | string | string | `selectedNode`, validated by `applyMultiNodeInfo` |
| `gpu` | repeated `gpu=vendor:id` | split at the first `:` | `selectedGpus`, validated when the GPU section's list arrives |
| `cgroup` | repeated `cgroup=<name>` | `getAll` | `seedSelectedCgroups`, validated when the cgroup list arrives |
| `instance` | id, scoped by `#/service/<name>` | string | `selectedInstances[svc]` |
| `anchor.<id>` | signed integer ms (`0` removed), or `kind:<event kind>` | both | `setAnchor` for the numeric form; `kind:` is parsed and left for the alignment work to apply |

Integer nanoseconds were rejected for `from`/`to`: an ns-since-epoch is
~1.7e18, past 2^53, and does not survive a JS `Number`. Capture ids are the
wire-stable `baseline`/`experiment`, never the alias, per the A/B entry.

**Write on change, `replaceState` only, never `m.route.set`.** The range
follows the override through one writer, `commitRangeOverride` in
`app.js`, which `applyDisplayWindow`, `changeGranularity` and
`changeTimeMode` all use, so a drag that never commits (debounced away, or
rejected as degenerate) never touches the URL. `changeNode`, `changeGpu`,
`changeInstance`, `changeTimeMode`, the cgroup selector's `transfer`, and
`setAnchor` write their own key. `m.route.set` would re-run the router's
`onmatch`, which clears the chart registry and scrolls to the top, for a
change that is not a navigation. Loading a different file
(`uploadParquet` in the server shell, `loadFile` in the site shell, both
per-shell copies) clears the owned keys.

**Read on load, in this order**, in `initDashboard` before `m.route(...)`:
time mode (set directly, not through `changeTimeMode`, which clears the
range; ignored in compare mode, which forces grid); the existing step
restore; the selectors; the range (set synchronously so the first section
fetch is already the window, then clamped once `baselineRange()` answers:
an empty intersection falls back to the full range, a partial one is
applied and rewrites the link); the compare anchors last, capped to the
experiment's duration as the attach-time clamp would. **A value the
recording cannot satisfy is dropped with one `console.warn` and the link
is rewritten without it**: a kept unknown node, GPU or cgroup renders every
chart empty with no explanation, which is worse than landing on the
default. Validation happens where each list arrives, since the GPU and
cgroup lists are only known after their section loads.

**Precedence on load.** A parameter in the URL wins over `localStorage` for
that key only. A link with only `from`/`to` still restores the recipient's own
persisted anchors. This is what keeps a shared link from silently resetting
state the recipient had set for a different reason.

**Absolute time, not relative.** A relative range (`last 5m`) means nothing in
a file. Live mode may accept a relative form later; file mode encodes
absolute instants.

**Document the parameters** in `docs/usage.md` as an interface, so
`rezolus mcp` and scripts can build a link. The `document-feature` skill's
test applies: an agent that has never seen the frontend must be able to
construct a link to a time range from the doc alone.

## Not in scope

- Encoding the notebook's pinned charts or notes. Those are the notebook's
  payload and already have an export format.
- Per-chart pin state. Pins are per chart by decision (#828) and a link that
  pins would reopen that.
- Server-side short links. The state is small enough for a hash.

## GO / NO-GO

GO when a link with `from`/`to` opens the viewer at that range in both
backends (server and WASM), with the `viewer-smoke` file-mode run extended to
fetch such a link and assert the display window request carries the range.
NO-GO condition: none foreseen; this is frontend state plumbing.

*Restated when built:* `viewer_smoke.sh` is curl-only and cannot observe
what the browser requests, so it only checks the module is served. The
assertion moved to the headless layer: `scripts/viewer_render.mjs` gained
`--requests <substring>` (every matching request URL in the JSON), and
opening `?from=<T0+10s>&to=<T0+20s>#/cpu` must produce `query_range`
requests whose `start`/`end` are exactly those seconds. See "Verified".

## Verified

Headless Chrome against a developer-mode build on `cachecannon.parquet`:
a `?from=&to=#/cpu` link produced range queries at exactly the link's
window and nothing at the full range; a `?node=nope&cgroup=/does/not/exist`
link warned once per key and the address bar was rewritten without them.
`node --test tests/*.mjs` (11 new tests on the module), the symlink check,
and `tests/viewer_smoke.sh` pass.

## Found, recorded, not fixed

- **The time bar ignores the range override.** `applyDisplayWindow` clears
  `globalZoom` and `TimeRangeBar` (`ui/controls.js`) derives its labels
  from the section payload's full-recording `start_time`/`end_time`, so
  after any drill-down, and therefore on every `from/to` link, the bar
  shows 0–100% of the recording while the charts show the window. The
  time-bar render assertion waits on this.
- **The About link uses the wrong prefix**: `#!/overview` (`app.js`) with
  `m.route.prefix = '#'` matches nothing and lands on the default route by
  fallback.

## Second pass (adversarial review before merge)

The review found the file-swap handling wrong in both shells and the
docs ahead of the code. Fixed in the same PR:

- **File swap.** The site shell cleared the URL keys *after* `loadParquet`,
  which re-runs `initDashboard`, so the previous file's `from/to` and
  `time=raw` were read and applied to the new file and then the keys were
  removed: the view said one thing and the address bar another. The server
  shell cleared the keys but kept the in-memory time mode, selections and
  range override, so the address bar stopped describing the view. Both now
  call one `resetLinkedViewState()` in `app.js` before the new file is
  read, which forgets the override and its cached extent, the time mode,
  the GPU and cgroup selections, and the URL keys. This also closes the
  "`uploadParquet` keeps `_rangeOverride`" bug the first pass had recorded.
  The server path also never re-derived the node and instance state from
  the uploaded file (only the initial load did), so `reapplyFileMetadata`
  now runs `applyMultiNodeInfo` after an upload; as a side effect the
  document title and the Metadata page, which read the same module
  variable, show the uploaded file instead of the previous one.
- **Clamp before the first load.** The async clamp raced the router's
  first `loadSection` (`refetchCurrentSectionInPlace` with no cached
  section falls back to a second concurrent load). Both shells now pass
  the recording's extent as `config.queryRange`, so the clamp is
  synchronous; when it is absent the clamp is deferred to the end of the
  first `loadSection` instead of running under it.
- **The address bar mirrors state the link did not name.** A compare anchor
  restored from localStorage is written back to the URL after the read, so
  a copied link reproduces the effective view. Granularity, pins and the
  heatmap toggle stay out, and the doc says so.
- **`kind:` anchors** were documented as working; they are parsed and not
  applied until the alignment work. The doc now says reserved.
- **GPU vendor.** A link's `nvidia:0` against a recording whose sampler set
  no vendor was kept with the vendor and filtered every chart to nothing;
  the selection is now resolved to the recording's own entry.
- **RFC 3339 offsets.** `URLSearchParams` decodes `+` as a space, so
  `...T14:03:11+02:00` typed into the address bar parsed to nothing; the
  sign is restored before `Date.parse`.
- Ignored keys (`time=raw` in compare mode, `from/to` in live mode) now
  warn and are removed from the link, as the doc promised; the baseline
  anchor is no longer capped to the experiment's duration; a cgroup
  update cancelled by a newer selection re-runs once instead of dropping
  the change; `scripts/viewer_render.mjs` collecting `console.warn` is
  noted in the `viewer-render` skill.

## Deferred / Reopen

- **Live-mode relative ranges** (`?last=5m`). Reopen when live-agent links are
  requested. Absolute `from`/`to` are ignored in live mode.
- **A `viewer_link` MCP tool** depends on this landing first; see
  [MCP write-back and tool tiers](2026-09-28-mcp-write-back.md).
- **`from`/`to` on the experiment side.** They set the baseline's range
  override, exactly what a drill-down does today; experiment fetches use
  `experimentQueryRange` (`viewer_core.js`) and ignore it. The experiment
  window should be `[from − Δ, to − Δ]` with Δ the anchor difference, which
  is the alignment work's concern.
- **Named N-way anchors** (`anchor.<named id>`). The parser accepts any id;
  `setAnchor` still takes only the two slots until the alignment work
  widens it.
- **`step` in the URL.** Already restored from localStorage; a one-key
  extension when someone needs it in a link.
- **The time-bar assertion**, blocked on the time-bar bug above.

## Cross-references

- [A/B compare mode](2026-04-21-ab-compare-mode.md): alias-versus-id, the
  pin-scope decision.
- [Selection → Notebook → Report](2026-05-10-selection-notebook-report.md):
  where anchors and pins are persisted today.
