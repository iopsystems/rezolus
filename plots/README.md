# Plots

This is a proof of concept for rendering interactive charts with dynamic aggregation/sampling,
adaptively computed by the backend based on plot dimensions and in response to interactions.

The front end never holds the recording, only a sample, sized to the screen.
The backend downsamples and serves the requested window, resampled on every zoom and pan.

Two charts: a 150,000-sample line, a 150,000 × 128 density heatmap. One request
shape drives both.

## The contract: a view is a request

The JS renderer states its budget. The request is two rectangles:

- data window — `x0, x1, y0, y1`
- pixel budget — `width, height`

One struct, `ViewQuery`. Both endpoints take it.

The response is data already shaped to that budget. It carries the bounds it
covers (for axis labels) plus a payload: paths for the line, a tile image for the
heatmap. Payload coordinates are normalized to `[0, 1]`, not raw data units.
Browser SVG/CSS floats lose precision at large magnitudes; millisecond timestamps
are the worst case. Small numbers place correctly at any zoom.

The returned region _contains_ the requested window; it doesn't clip to it; that way
we can have tiles returned that only partly overlap the view.

The request is really a function signature: `(window, size) → a picture that
fits`. Transport is HTTP/JSON today. Incidental — see "Where this goes."

## Line: M4 downsampling

Input: a window and a width. Output: at most ~`4 × width` points.

- One bucket per pixel column.
- Per bucket, keep four samples — first, last, min, max. Drop the rest.

First and last keep the line connected across the bucket. Min and max keep every
spike; a one-sample peak survives. Naive stride sampling does not — it eventually
steps over a spike and lies about the data.

The result is pixel-identical to drawing the full series at that width. Nothing
dropped was drawable. Zoom in until a column holds one sample and M4 returns the
raw data. Output is always sized to the screen.

Reference: https://observablehq.com/@uwdata/m4-scalable-time-series-visualization

## Heatmap: map tiles

Every heatmap pixel is a 2D range query — total density over a rectangle. Asked at
any zoom, over any rectangle, at mouse speed.

Same model as Google Maps. Fixed power-of-two zoom levels. Snap the window to the
tile grid. Return one value per tile, only for the tiles the viewport covers, at
the current level. Power-of-two sizes make neighboring levels align, so small pans
reuse tiles.

In this demo, the "engine" is a summed-area table — a 2D prefix sum, built once at
startup. We can figure out an alternate strategy for doing this on histogram columns.

The total of any rectangle is four lookups, independent of how many cells are inside.
A tile query is four reads, not a loop. Cost tracks tiles on screen — i.e. screen
size — i.e. a constant. Field height does not matter.

Wire format is the tiles themselves: one byte per tile (a palette index), not
rendered pixels. The browser scales the small grid up with one transform,
nearest-neighbor. A zoomed-out view of a 19M-cell field is a few hundred bytes.

## The front end

Thin by design. It takes shaped data, places it with one transform, and draws.
It does not summarize. It does not hold the recording.

- Render classes (line, heatmap). Pure `render(data)`: a view plus a payload in,
  pixels out. No I/O.
- Gesture handler. Owns zoom, pan, and brush. Draws nothing. Emits semantic
  events — "view changed", "region selected".
- Rendering and interaction meet only through those events.

The wiring page adds four behaviors:

- **Over-fetch.** Request a window 20% larger than the viewport, so we have data to
- cover small pans and zooms with cached data and no round trip.
- **Debounce.** Collapse an event flurry into one request on settle. Hard ceiling
  so a long gesture still fires.
- **Cancel.** A new request aborts the one in flight. Stops a slow early response
  from overwriting a newer view.
- **Link.** Both plots share one coordinate frame. A zoom on either drives both.

## Running it

```
cargo run
```

Open http://localhost:3000. Drag to pan, scroll to zoom, Shift-drag to select.
Both plots move together.

- `src/main.rs` — server: the request contract, the two endpoints, demo data.
- `src/m4.rs` — line downsampler.
- `src/sat.rs` — summed-area table and tile queries.
- `static/index.html` — front-end wiring.
- `static/components/*.js` — render classes, axis, gesture handler.
