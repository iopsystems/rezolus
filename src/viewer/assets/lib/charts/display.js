// Display mode and the binary heatmap body: decoding what
// crates/dashboard/src/display_wire.rs encodes (served by the viewer, or by
// its WASM build), and what a chart needs to draw it.
//
// - decodeDisplayBinary and decodeHeatmapBinary read the binary bodies that
//   crates/dashboard/src/display_wire.rs writes.
// - displayBucketWidth reproduces the server's choice of bucket width.
// - plotUsesDisplay says which plots are fetched in display mode.
// - The tile cache keeps decoded responses per query, so a revisited window
//   is drawn without a fetch.
// - applyDisplayToPlot writes a decoded response into a plot spec.
//
// Fetching is the page's job: the rezolus viewer's data.js fetches, with a
// budget taken from the viewer's layout.

import { seriesNamer } from './util/labels.js';
import { resolveStyle } from './metric_types.js';

// Decode the display-mode binary response (see
// crates/dashboard/src/display_wire.rs `encode_display_binary`):
//
//   [u32 LE headerLen][JSON header][pad to 8B][f64 LE column blobs]
//
// The JSON header carries per-series labels + provenance + point count `n`;
// the blob is, per series in order, six columns t,min,lo,median,hi,max each
// `n` little-endian f64. Columns are returned as zero-copy Float64Array views
// over the buffer. Returns { resultType, budget, series: [{...meta, t, min,
// lo, median, hi, max }] }.
export const decodeDisplayBinary = (buf) => {
    const dv = new DataView(buf);
    const headerLen = dv.getUint32(0, true);
    const header = JSON.parse(
        new TextDecoder().decode(new Uint8Array(buf, 4, headerLen)),
    );
    // The encoder pads so the first f64 lands on an 8-byte boundary; each
    // column is n*8 bytes, so every subsequent column stays aligned too.
    let off = Math.ceil((4 + headerLen) / 8) * 8;
    const COLS = ['t', 'min', 'lo', 'median', 'hi', 'max'];
    const series = header.series.map((s) => {
        const out = { ...s };
        for (const name of COLS) {
            out[name] = new Float64Array(buf, off, s.n);
            off += s.n * 8;
        }
        // Series that carry a measurement-uncertainty band append two more
        // columns (uncLo, uncHi) after the six boxplot columns — the `unc`
        // header flag guards reading them so band and no-band series stay
        // byte-aligned in a mixed response.
        if (s.unc) {
            out.uncLo = new Float64Array(buf, off, s.n);
            off += s.n * 8;
            out.uncHi = new Float64Array(buf, off, s.n);
            off += s.n * 8;
        }
        // Then, if flagged, one more column marking the points that span a
        // stretch the producer never read. Read AFTER the unc columns because
        // the encoder writes it last; reading it earlier would silently
        // misalign every series that carries both.
        if (s.interp) {
            out.interpCol = new Float64Array(buf, off, s.n);
            off += s.n * 8;
        }
        return out;
    });
    return { resultType: header.resultType, budget: header.budget, series };
};

// Decode the binary histogram-heatmap body (see
// crates/dashboard/src/display_wire.rs `encode_heatmap_binary`).
// The win over the JSON path is skipping the multi-MB JSON string parse of the
// triples: columns arrive as typed arrays (zero-copy views), and we reconstruct
// the [timeIdx, bucketIdx, count] triples the render + compare mode already
// consume — so the returned shape is identical to the JSON path (no downstream
// changes) but there's no string parsing.
export const decodeHeatmapBinary = (buf) => {
    const dv = new DataView(buf);
    const headerLen = dv.getUint32(0, true);
    const header = JSON.parse(
        new TextDecoder().decode(new Uint8Array(buf, 4, headerLen)),
    );
    if (header.resultType !== 'histogram_heatmap') {
        throw new Error(`unexpected binary resultType ${header.resultType}`);
    }
    let off = Math.ceil((4 + headerLen) / 8) * 8;
    const nTs = header.nTimestamps;
    const nTr = header.nTriples;
    const timestamps = new Float64Array(buf, off, nTs); off += nTs * 8;
    const count = new Float64Array(buf, off, nTr); off += nTr * 8;
    const timeIdx = new Uint32Array(buf, off, nTr); off += nTr * 4;
    const bucketIdx = new Uint32Array(buf, off, nTr); off += nTr * 4;
    const data = new Array(nTr);
    for (let i = 0; i < nTr; i++) data[i] = [timeIdx[i], bucketIdx[i], count[i]];
    return {
        time_data: Array.from(timestamps),
        bucket_bounds: header.bucketBounds,
        data,
        min_value: header.minValue,
        max_value: header.maxValue,
    };
};

// The round bucket widths, in seconds, the server's display reducer chooses
// from (metriken-query's `nice_bucket_secs`): the smallest at least `raw`.
const NICE_SECS = [
    1, 2, 5, 10, 15, 20, 30,
    60, 120, 300, 600, 900, 1800,
    3600, 7200, 10800, 21600, 43200,
    86400,
];
export const niceSecs = (raw) => {
    for (const n of NICE_SECS) if (n >= raw) return n;
    return Math.ceil(raw / 86400) * 86400;
};

// The bucket width, in seconds, the server's display reducer uses for
// [start, end] at `step` with `budget` buckets (metriken-query's
// `bucket_width`): the round width at least (end - start) / budget, or at
// most `step` when that is no more than `step`. `null` for full resolution.
export const displayBucketWidth = (start, end, step, budget) => {
    const span = end - start;
    if (!(budget > 0) || !(span > 0)) return null;
    const raw = span / budget;
    const nice = niceSecs(raw);
    return step > 0 && raw <= step ? Math.min(nice, step) : nice;
};

// Which plots use display mode: line-ish charts (gauge / counter) and
// histogram *percentile* scatterplots (histogram_quantiles returns a
// per-percentile matrix the reducer decimates the same way). Histogram
// heatmaps (buckets / quantile_heatmap) keep the JSON path — they have their
// own server-side resolution handling.
// A query that groups `by (id)` with no outer aggregation (`isEntityQuery`)
// yields one series per entity (CPU/GPU/...), which renders as a per-entity
// HEATMAP (resolveStyle → 'heatmap' when the result carries an `id` label);
// it is fetched by the rezolus viewer's `fetchEntityHeatmap` (data.js), with
// one value per entity and column. `\bid\b` avoids matching substrings like
// `grid`/`width`.
const GROUPS_BY_ID = /\bby\s*\(\s*[^)]*\bid\b[^)]*\)/;

export const plotUsesDisplay = (plot) => {
    if (!plot?.promql_query) return false;
    if (plot.opts?.type !== 'histogram') return !isEntityQuery(plot.promql_query);
    return plot.opts?.subtype === 'percentiles';
};

// A query grouped `by (id)` and not wrapped in an outer aggregation: one
// series per entity, drawn as a heatmap. `avg(sum by (id) (...))` is one
// series and is drawn as a line.
const OUTER_AGG = /^\s*(?:sum|avg|min|max|count)\s*\(/;
export const isEntityQuery = (query) => GROUPS_BY_ID.test(query) && !OUTER_AGG.test(query);

// ── LOD tile cache ──────────────────────────────────────────────────────────
// Decoded display results keyed by query, each tagged with its [start,end]
// extent (seconds). A drill-down is served from cache — no network — when a
// cached tile COVERS the window with at least the requested resolution
// (≈ budget points inside the window); the tile is clipped to the window via
// zero-copy Float64Array views. This makes revisited ranges (section switches,
// display/heatmap toggles, small pans, re-zooms) instant. Bounded LRU per query.
// The page clears it with clearDisplayTiles whenever the recording changes
// (data.js does so when the metadata is invalidated), so a live viewer never
// serves stale tiles.
const TILE_MAX_PER_QUERY = 8;
// query -> [{ start, end, width, decoded, seq }]. Exported so a page can
// bound the cache's total size across queries by reading and deleting
// entries.
export const _displayTiles = new Map();
let _tileSeq = 0;
export const clearDisplayTiles = () => { _displayTiles.clear(); };

// Index window [lo, hi) of the ascending `t` array covering [ns, ne] (seconds).
const windowIndices = (t, ns, ne) => {
    let lo = 0;
    while (lo < t.length && t[lo] < ns) lo++;
    let hi = t.length;
    while (hi > lo && t[hi - 1] > ne) hi--;
    return [lo, hi];
};

// Every per-point column, not just the six a series always carries: `uncLo`,
// `uncHi` and `interpCol` are optional but are indexed by the SAME point index
// as `t`, so clipping `t` and spreading those through at full length leaves
// them misaligned by `lo`. `zipMs` in boxplot.js pairs `t[i]` with `col[i]`
// and checks no lengths, so that renders the uncertainty ribbon at the wrong
// timestamps rather than failing; `buildBoxplotSeries`'s interpolation guard
// compares lengths and silently drops the unobserved-stretch overlay instead.
const CLIPPED_COLUMNS = ['t', 'min', 'lo', 'median', 'hi', 'max', 'uncLo', 'uncHi', 'interpCol'];

export const clipDecoded = (decoded, ns, ne) => {
    const series = decoded.series.map((s) => {
        const [lo, hi] = windowIndices(s.t, ns, ne);
        const out = { ...s, n: hi - lo };
        for (const c of CLIPPED_COLUMNS) {
            if (s[c]) out[c] = s[c].subarray(lo, hi);
        }
        return out;
    });
    return { resultType: decoded.resultType, budget: decoded.budget, series };
};

export const tileLookup = (query, ns, ne, step, budget) => {
    const tiles = _displayTiles.get(query);
    if (!tiles) return null;
    let best = null;
    for (const tile of tiles) {
        // Must cover the window and, once clipped, still carry ~budget points.
        if (tile.start > ns + 1e-6 || tile.end < ne - 1e-6) continue;
        const s0 = tile.decoded.series[0];
        if (!s0 || !s0.t || s0.t.length === 0) continue;
        const [lo, hi] = windowIndices(s0.t, ns, ne);
        // Only a DECIMATED tile can be too coarse. One that was never
        // decimated already holds every sample the recording has across its
        // range, so a window it covers is at native resolution and no refetch
        // can return more. Measuring it against a budget derived from the
        // window's LENGTH rejects it whenever the samples are sparser than the
        // step -- and, in a response composed from several recordings, whenever
        // series[0] spans less than the tile's full range, since that range is
        // the union across all series while this check reads only the first.
        if (s0.decimated && !(tile.width <= (displayBucketWidth(ns, ne, step, budget) ?? 0) + 1e-9)) {
            continue;
        }
        if (!best || (tile.end - tile.start) < (best.end - best.start)) best = tile;
    }
    if (!best) return null;
    best.seq = ++_tileSeq; // LRU touch
    return clipDecoded(best.decoded, ns, ne);
};

export const tileStore = (query, start, end, width, decoded) => {
    let tiles = _displayTiles.get(query);
    if (!tiles) { tiles = []; _displayTiles.set(query, tiles); }
    tiles.push({ start, end, width, decoded, seq: ++_tileSeq });
    if (tiles.length > TILE_MAX_PER_QUERY) {
        tiles.sort((a, b) => a.seq - b.seq);
        tiles.splice(0, tiles.length - TILE_MAX_PER_QUERY);
    }
};

// Convert a decoded display series' aggregated measurement-uncertainty columns
// (uncLo/uncHi, NaN = no band at that point) into the same `[[lo,hi]|null, …]`
// shape the matrix path's parseIntervals (data.js) produces, so
// buildBandSeries renders them identically. Returns null when the series
// carries no band at all.
export const displayIntervals = (s) => {
    if (!s || !s.uncLo || !s.uncHi) return null;
    const n = Math.min(s.uncLo.length, s.uncHi.length);
    const out = new Array(n);
    let any = false;
    for (let i = 0; i < n; i++) {
        const lo = s.uncLo[i];
        const hi = s.uncHi[i];
        if (Number.isFinite(lo) && Number.isFinite(hi)) {
            out[i] = lo <= hi ? [lo, hi] : [hi, lo];
            any = true;
        } else {
            out[i] = null;
        }
    }
    return any ? out : null;
};

// Convert a decoded display series' interpolated column into the boolean array
// parseInterpolated produces on the matrix path, so line.js consumes one shape
// regardless of which path fetched the data. Returns null when the series has
// no interpolated points — the renderer treats null and all-false alike, but
// returning null keeps `plot.interpolated` falsy so nothing downstream has to
// distinguish "no flag sent" from "flag sent, all clear".
export const displayInterpolated = (s) => {
    if (!s || !s.interpCol) return null;
    let any = false;
    const out = new Array(s.interpCol.length);
    for (let i = 0; i < s.interpCol.length; i++) {
        // The encoder writes exactly 1.0 or 0.0, but decimation upstream could
        // in principle produce a fraction; treat anything non-zero as set
        // rather than testing equality against 1.
        out[i] = s.interpCol[i] !== 0;
        if (out[i]) any = true;
    }
    return any ? out : null;
};

// Store a decoded display response on the plot. `data` carries the median
// line(s) so all existing chart machinery (axis extent, zoom, no-data
// detection, change-detection) works unchanged; `boxplot` carries the
// per-series columns { t,min,lo,median,hi,max } for the band render in line.js.
export const applyDisplayToPlot = (plot, decoded) => {
    const series = (decoded && decoded.series) || [];
    if (series.length === 0) {
        plot.data = [];
        plot.boxplot = null;
        plot.series_names = [];
        plot.series_metrics = [];
        plot.intervals = null;
        plot.series_intervals = [];
        // Cleared with the rest: an empty response after a series that had a
        // hole would otherwise leave the flag set, and line.js would index a
        // stale array against the next render's timestamps.
        plot.interpolated = null;
        return;
    }
    const timestamps = Array.from(series[0].t);
    plot.data = [timestamps, ...series.map((s) => Array.from(s.median))];
    plot.boxplot = series;
    // Whether the response is a downsample; drives band-vs-scatter in scatter.js
    // and (eventually) whether the line render draws collapsed identity bands.
    plot.boxplotDecimated = series.some((s) => s.decimated);
    plot.series_metrics = series.map((s) => s.metric || {});
    const nameOf = seriesNamer(series.map((s) => s.metric || {}));
    // A `multi` chart is drawn by multi.js even with one series, as on the
    // JSON path (data.js's applyResultToPlot). multi.js colours a line by its
    // name, so an unnamed one would be "Series 1" in another colour (#1289).
    const asMulti = series.length > 1 || plot.opts?.style === 'multi';
    plot.series_names = asMulti
        ? series.map((s, i) => nameOf(s.metric || {}) ?? `Series ${i + 1}`)
        : [];
    // Measurement-uncertainty bands from the aggregated uncLo/uncHi columns —
    // the median-of-interval-edges band computed server-side during decimation
    // (exact per-sample interval at native resolution). Mirror applyResultToPlot's
    // single vs multi split so line.js reads `intervals` and multi/scatter read
    // `series_intervals`, clearing the other to avoid a stale band ghosting.
    if (asMulti) {
        plot.series_intervals = series.map(displayIntervals);
        plot.intervals = null;
    } else {
        plot.intervals = displayIntervals(series[0]);
        plot.series_intervals = [];
    }
    // The dashed interpolated overlay is a single-series line affordance, the
    // same as `intervals` above: line.js reads `plot.interpolated`, and multi
    // and scatter have no overlay to draw. Cleared in the multi case so a
    // previous single-series render cannot leave a stale flag behind.
    plot.interpolated = asMulti ? null : displayInterpolated(series[0]);
    // Resolve the render style the same way the JSON path does: percentile
    // histograms → scatter, single line-ish → line, multi line-ish → multi.
    plot._resolvedStyle = plot.opts?.style
        || (plot.opts?.type === 'histogram'
            ? resolveStyle(plot.opts.type, plot.opts.subtype)
            : (series.length > 1 ? 'multi' : 'line'));
};
