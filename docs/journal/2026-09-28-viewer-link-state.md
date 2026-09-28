# Viewer links that carry the whole view

- **Opened:** 2026-09-28
- **Status:** OPEN — design, nothing built.

## Problem

A viewer link does not reproduce what the sender was looking at. The hash
route carries the section and, for a single-chart view, the chart id
(`src/viewer/assets/lib/app.js`, the `'/:section/chart/:chartId'` route). The
rest of the view state lives in memory or in `localStorage`:

- the zoom range (`ChartsState.globalZoom` in
  `src/viewer/assets/lib/charts/chart.js`, applied through
  `applyDisplayWindow` in `app.js`);
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

**Encode state in the hash, not `localStorage`.** The hash already drives
routing through mithril (`m.route.prefix = '#'`), so state rides as query
parameters on the hash route:

```
#/cpu?from=2026-09-28T14:03:11.250Z&to=2026-09-28T14:05:40Z
#/cpu/chart/cpu_usage?from=...&to=...&anchor.experiment=-1500
#/scheduler?from=...&to=...&time=raw&cgroup=web.*
```

Parameters:

| key | value | reads into |
|-----|-------|-----------|
| `from`, `to` | RFC 3339 or integer ns | `applyDisplayWindow` |
| `anchor.<capture id>` | signed ms offset | `notebookStore.anchors` |
| `time` | `aligned` or `raw` | the #1023 time-mode control |
| `cgroup`, `gpu`, `node` | the selector's own value syntax | the feature selectors |

Capture ids are the wire-stable `baseline`/`experiment` (and the named ids a
multi-recording `.rez` assigns), never the cosmetic alias, per the A/B entry's
alias-versus-id decision.

**Write the hash on change, replace rather than push.** Every zoom and anchor
change calls `history.replaceState` so the back button is not filled with
zoom steps. Section navigation keeps `pushState` as today.

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

## Deferred / Reopen

- **Live-mode relative ranges** (`?last=5m`). Reopen when live-agent links are
  requested.
- **A `viewer_link` MCP tool** depends on this landing first; see
  [MCP write-back and tool tiers](2026-09-28-mcp-write-back.md).

## Cross-references

- [A/B compare mode](2026-04-21-ab-compare-mode.md): alias-versus-id, the
  pin-scope decision.
- [Selection → Notebook → Report](2026-05-10-selection-notebook-report.md):
  where anchors and pins are persisted today.
