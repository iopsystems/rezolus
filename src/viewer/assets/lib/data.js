import { ViewerApi } from './viewer_api.js';
import { resolveStyle, buildHistogramQuery, isHistogramPlot } from './charts/metric_types.js';
import { collectGroupPlots } from './features/group_utils.js';

// Capture-id constants. Typos become grep-able; use these in place of
// raw 'baseline' / 'experiment' string literals.
export const CAPTURE_BASELINE = 'baseline';
export const CAPTURE_EXPERIMENT = 'experiment';

let _stepOverride = null;
const setStepOverride = (step) => { _stepOverride = step; };
const getStepOverride = () => _stepOverride;

// Default { start, end, step } for an overview range query: the WHOLE
// recording at its NATIVE sampling interval. Decimation for display is
// echarts' job (`sampling: 'lttb'` on line/scatter, the heatmap
// resolution stores for heatmaps) — a purely presentational pass that
// preserves extrema.
//
// We deliberately do NOT decimate by widening the PromQL step. `step` is
// a query-*semantics* parameter, not a decimation knob:
//   - Histograms ignore `step` outright — output resolution is set by
//     their `stride` arg — so a coarse step bounds nothing for them.
//   - Counters honor `step` but need their rate window rewritten to
//     match (rewriteCounterQuery), and any coarse step averages away the
//     spikes we care about.
// Widening step therefore corrupts the queries (mismatched per-type
// resolution, skipped rewrites) without reliably bounding payload. The
// real payload bound is server-side min/max decimation applied AFTER
// evaluation (the reducer / DisplayResult envelope) — tracked separately.
//
// `_stepOverride` (the Granularity selector) still wins; it drives the
// stride/rate rewrites via buildEffectiveQuery so a user-chosen coarse
// step stays self-consistent.
// Zoom drill-down: when set to { start, end } (seconds), range queries fetch
// that window instead of the whole recording. Display mode keeps the same
// point budget, so a narrower window comes back at higher resolution — and
// once it fits the budget, at native 1s. Cleared (null) = full recording.
let _rangeOverride = null;
export const setRangeOverride = (range) => { _rangeOverride = range; };
export const getRangeOverride = () => _rangeOverride;

export const defaultRangeFor = (meta) => {
    const start = _rangeOverride ? _rangeOverride.start : meta.minTime;
    const end = _rangeOverride ? _rangeOverride.end : meta.maxTime;
    const interval = (Number.isFinite(meta.interval) && meta.interval > 0) ? meta.interval : 1;
    const step = _stepOverride || Math.max(1, interval);
    return { start, end, step };
};

// Query rewriting for non-default granularity (step override).
// When the user picks a coarser step (e.g. 15s instead of auto ~1s), raw
// queries must be adjusted so that values are properly smoothed over the
// wider window rather than just down-sampled.
//
//   Counter:   irate(m[5m]) → rate(m[Ns])   (true average rate over window)
//   Gauge:     no rewrite needed (engine samples at step points)
//   Histogram: stride parameter passed to histogram_quantiles / histogram_heatmap

const rewriteCounterQuery = (query, stepSecs) => {
    const window = stepSecs + 's';
    return query.replace(/\birate\s*\(([^)]*?)\[\d+[smhd]\]/g, `rate($1[${window}]`);
};

// Gauge queries don't need rewriting — the PromQL engine samples the
// instantaneous value at each step point, which is correct for gauges.

const defaultGetMetadata = () => ViewerApi.getMetadata();
const defaultQueryRange = (query, start, end, step, captureId = 'baseline', signal = undefined) =>
    ViewerApi.queryRange(query, start, end, step, captureId, signal);

export const queryRangeForCapture = (captureId, query, start, end, step) =>
    defaultQueryRange(query, start, end, step, captureId);

// Display-mode variant for a specific capture: returns the decoded boxplot
// series array ({t,min,lo,median,hi,max}, …) or null on a non-series / empty
// result. Used by compare mode to draw each capture's min/max envelope.
export const queryRangeDisplayForCapture = async (captureId, query, start, end, step, points = 500) => {
    const res = await ViewerApi.queryRangeDisplay(query, start, end, step, { points, captureId });
    if (!res || !res.buffer) return null;
    return decodeDisplayBinary(res.buffer).series;
};

// Decode the display-mode binary response (see routes.rs
// `encode_display_binary`):
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
        return out;
    });
    return { resultType: header.resultType, budget: header.budget, series };
};

// Decode the binary histogram-heatmap body (see routes.rs encode_heatmap_binary).
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

// The JSON heatmap result already has the consumed shape — passthrough for the
// fallback path so both transports return the same object.
const heatmapFromJson = (hr) => ({
    time_data: hr.timestamps,
    bucket_bounds: hr.bucket_bounds,
    data: hr.data,
    min_value: hr.min_value,
    max_value: hr.max_value,
});

// Display (boxplot decimation) mode. Off by default so unit tests keep the
// JSON matrix path; app.js turns it on for the live viewer. When on,
// line-ish plots fetch the decimated boxplot binary instead of the full
// native-resolution JSON matrix.
let _displayMode = false;
export const setDisplayMode = (on) => { _displayMode = on; };
export const getDisplayMode = () => _displayMode;

// Point/column budget for decimation (display-mode series and histogram bucket
// heatmaps): ~1 point per CSS pixel of a chart's PLOT AREA. Sending more points
// than the chart is wide adds no visible line detail — it only turns the min/max
// band into a noisy over-dense fill on high-variance signals (and bloats the
// payload). Charts render two per row, so measure an actual chart cell when one
// exists (exact on a zoom refetch — it handles full- vs half-width) and fall
// back to half the viewport on first load. devicePixelRatio is intentionally NOT
// applied: sub-CSS-pixel detail isn't distinguishable in the envelope, and
// multiplying by it was over-fetching ~4× on half-width retina charts. The upper
// clamp also caps the histogram bucket-heatmap columns (downsampled to 500 in
// histogram_heatmap.js regardless).
const BUDGET_MIN = 300;
const BUDGET_MAX = 1200;
const pixelBudget = () => {
    if (typeof window === 'undefined') return 500;
    let w = 0;
    try { w = document.querySelector('.chart-cell')?.clientWidth || 0; } catch (_) { /* no DOM */ }
    if (!w) w = (window.innerWidth || 1000) / 2;
    return Math.max(BUDGET_MIN, Math.min(BUDGET_MAX, Math.round(w)));
};

// Budget for a display-series fetch over [start,end]. Starts from the pixel
// budget, but ALSO caps it so each bucket aggregates at least
// MIN_SAMPLES_PER_BUCKET native samples once the window is wide enough — so the
// median+band engages and SMOOTHS jittery per-second signals (e.g. CPU%) at
// moderate windows instead of drawing every raw sample as a dense zigzag. The
// MIN_DISPLAY_BUCKETS floor keeps tight windows detailed: when the window has few
// native samples the cap sits above the native count, so the server returns
// native resolution (no aggregation) and you still get raw detail on a deep zoom.
const MIN_SAMPLES_PER_BUCKET = 5;
const MIN_DISPLAY_BUCKETS = 48;
const displayBudget = (meta, start, end) => {
    const px = pixelBudget();
    const interval = (Number.isFinite(meta?.interval) && meta.interval > 0) ? meta.interval : 1;
    const native = Math.max(1, Math.round((end - start) / interval));
    return Math.min(px, Math.max(MIN_DISPLAY_BUCKETS, Math.ceil(native / MIN_SAMPLES_PER_BUCKET)));
};

// Which plots use display mode: line-ish charts (gauge / counter) and
// histogram *percentile* scatterplots (histogram_quantiles returns a
// per-percentile matrix the reducer decimates the same way). Histogram
// heatmaps (buckets / quantile_heatmap) keep the JSON path — they have their
// own server-side resolution handling.
// A query that groups `by (id)` yields one series per entity (CPU/GPU/...),
// which the native path renders as a per-entity HEATMAP (resolveStyle → 'heatmap'
// when the result carries an `id` label). Display mode collapses each series to a
// median + bands, which cannot represent a heatmap — so those stay on the native
// render path. `\bid\b` avoids matching substrings like `grid`/`width`.
const GROUPS_BY_ID = /\bby\s*\(\s*[^)]*\bid\b[^)]*\)/;

const plotUsesDisplay = (plot) => {
    if (!plot?.promql_query) return false;
    if (plot.opts?.type !== 'histogram') return !GROUPS_BY_ID.test(plot.promql_query);
    return plot.opts?.subtype === 'percentiles';
};

// A short series label from its distinguishing labels (first non-__name__).
const displaySeriesName = (metric, i) => {
    if (metric) {
        for (const [k, v] of Object.entries(metric)) {
            if (k !== '__name__') return v;
        }
    }
    return `Series ${i + 1}`;
};

// Fetch + decode the display-mode boxplot response. Uses defaultRangeFor so
// it honors the zoom range override (drill-down) with the same point budget.
// ── LOD tile cache ──────────────────────────────────────────────────────────
// Decoded display results keyed by query, each tagged with its [start,end]
// extent (seconds). A drill-down is served from cache — no network — when a
// cached tile COVERS the window with at least the requested resolution
// (≈ budget points inside the window); the tile is clipped to the window via
// zero-copy Float64Array views. This makes revisited ranges (section switches,
// display/heatmap toggles, small pans, re-zooms) instant. Bounded LRU per query.
// Cleared whenever the recording's metadata is invalidated (see below) so a live
// viewer never serves stale tiles.
const TILE_MAX_PER_QUERY = 8;
const _displayTiles = new Map(); // query -> [{ start, end, decoded, seq }]
let _tileSeq = 0;
const clearDisplayTiles = () => { _displayTiles.clear(); };

// Index window [lo, hi) of the ascending `t` array covering [ns, ne] (seconds).
const windowIndices = (t, ns, ne) => {
    let lo = 0;
    while (lo < t.length && t[lo] < ns) lo++;
    let hi = t.length;
    while (hi > lo && t[hi - 1] > ne) hi--;
    return [lo, hi];
};

const clipDecoded = (decoded, ns, ne) => {
    const series = decoded.series.map((s) => {
        const [lo, hi] = windowIndices(s.t, ns, ne);
        const out = { ...s, n: hi - lo };
        for (const c of ['t', 'min', 'lo', 'median', 'hi', 'max']) {
            out[c] = s[c].subarray(lo, hi);
        }
        return out;
    });
    return { resultType: decoded.resultType, budget: decoded.budget, series };
};

const tileLookup = (query, ns, ne, budget) => {
    const tiles = _displayTiles.get(query);
    if (!tiles) return null;
    let best = null;
    for (const tile of tiles) {
        // Must cover the window and, once clipped, still carry ~budget points.
        if (tile.start > ns + 1e-6 || tile.end < ne - 1e-6) continue;
        const s0 = tile.decoded.series[0];
        if (!s0 || !s0.t || s0.t.length === 0) continue;
        const [lo, hi] = windowIndices(s0.t, ns, ne);
        if (hi - lo < budget * 0.9) continue;
        if (!best || (tile.end - tile.start) < (best.end - best.start)) best = tile;
    }
    if (!best) return null;
    best.seq = ++_tileSeq; // LRU touch
    return clipDecoded(best.decoded, ns, ne);
};

const tileStore = (query, start, end, decoded) => {
    let tiles = _displayTiles.get(query);
    if (!tiles) { tiles = []; _displayTiles.set(query, tiles); }
    tiles.push({ start, end, decoded, seq: ++_tileSeq });
    if (tiles.length > TILE_MAX_PER_QUERY) {
        tiles.sort((a, b) => a.seq - b.seq);
        tiles.splice(0, tiles.length - TILE_MAX_PER_QUERY);
    }
};

const fetchDisplaySeries = async (query, meta, signal) => {
    const { start, end, step } = defaultRangeFor(meta);
    const budget = displayBudget(meta, start, end);
    const cached = tileLookup(query, start, end, budget);
    if (cached) return cached; // covered at sufficient resolution — no network
    const res = await ViewerApi.queryRangeDisplay(query, start, end, step, { points: budget, signal });
    if (!res.buffer) {
        throw new Error(res.json?.error || 'display query returned a non-series result');
    }
    const decoded = decodeDisplayBinary(res.buffer);
    tileStore(query, start, end, decoded);
    return decoded;
};

// Convert a decoded display series' aggregated measurement-uncertainty columns
// (uncLo/uncHi, NaN = no band at that point) into the same `[[lo,hi]|null, …]`
// shape the matrix path's parseIntervals produces, so buildBandSeries renders
// them identically. Returns null when the series carries no band at all.
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

// Store a decoded display response on the plot. `data` carries the median
// line(s) so all existing chart machinery (axis extent, zoom, no-data
// detection, change-detection) works unchanged; `boxplot` carries the
// per-series columns { t,min,lo,median,hi,max } for the band render in line.js.
const applyDisplayToPlot = (plot, decoded) => {
    const series = (decoded && decoded.series) || [];
    if (series.length === 0) {
        plot.data = [];
        plot.boxplot = null;
        plot.series_names = [];
        plot.series_metrics = [];
        plot.intervals = null;
        plot.series_intervals = [];
        return;
    }
    const timestamps = Array.from(series[0].t);
    plot.data = [timestamps, ...series.map((s) => Array.from(s.median))];
    plot.boxplot = series;
    // Whether the response is a downsample; drives band-vs-scatter in scatter.js
    // and (eventually) whether the line render draws collapsed identity bands.
    plot.boxplotDecimated = series.some((s) => s.decimated);
    plot.series_metrics = series.map((s) => s.metric || {});
    plot.series_names = series.length > 1
        ? series.map((s, i) => displaySeriesName(s.metric, i))
        : [];
    // Measurement-uncertainty bands from the aggregated uncLo/uncHi columns —
    // the median-of-interval-edges band computed server-side during decimation
    // (exact per-sample interval at native resolution). Mirror applyResultToPlot's
    // single vs multi split so line.js reads `intervals` and multi/scatter read
    // `series_intervals`, clearing the other to avoid a stale band ghosting.
    if (series.length > 1) {
        plot.series_intervals = series.map(displayIntervals);
        plot.intervals = null;
    } else {
        plot.intervals = displayIntervals(series[0]);
        plot.series_intervals = [];
    }
    // Resolve the render style the same way the JSON path does: percentile
    // histograms → scatter, single line-ish → line, multi line-ish → multi.
    plot._resolvedStyle = plot.opts?.style
        || (plot.opts?.type === 'histogram'
            ? resolveStyle(plot.opts.type, plot.opts.subtype)
            : (series.length > 1 ? 'multi' : 'line'));
};

let _selectedNode = null;
let _selectedInstances = {};  // { serviceName: instanceId | null }
let _selectedGpus = [];        // GPU `id`s to filter the GPU section by; [] = all

const setSelectedNode = (node) => { _selectedNode = node; };
const getSelectedNode = () => _selectedNode;

// When non-empty, the GPU section's non-per-GPU charts are filtered to these
// GPU `id`s. Empty means show the aggregate (avg/sum across all GPUs). Per-GPU
// charts (those with `by (id)`) always show all GPUs and ignore this.
const setSelectedGpus = (ids) => {
    _selectedGpus = Array.isArray(ids) ? ids.map(String) : [];
};
const getSelectedGpus = () => _selectedGpus;

const setSelectedInstance = (serviceName, instanceId) => {
    _selectedInstances[serviceName] = instanceId;
};
const getSelectedInstance = (serviceName) => _selectedInstances[serviceName] || null;

const PROMQL_KEYWORDS = new Set([
    'by', 'without', 'on', 'ignoring', 'group_left', 'group_right',
    'bool', 'sum', 'avg', 'min', 'max', 'count', 'rate', 'irate', 'increase',
    'histogram_quantiles', 'histogram_heatmap', 'topk', 'bottomk', 'offset',
    'abs', 'absent', 'ceil', 'floor', 'round', 'sqrt', 'exp', 'ln', 'log2',
    'log10', 'clamp', 'clamp_max', 'clamp_min', 'delta', 'deriv', 'idelta',
    'predict_linear', 'resets', 'changes', 'label_replace', 'label_join',
    'sort', 'sort_desc', 'time', 'timestamp', 'vector', 'scalar', 'sgn',
    'stddev', 'stdvar', 'quantile', 'count_values', 'group',
]);

// Inject a label selector into all metric references in a PromQL query.
// Handles three forms:
//   metric{existing}  → metric{existing,label="value"}
//   metric[5m]        → metric{label="value"}[5m]
//   metric            → metric{label="value"}   (bare, in expressions)
const injectLabel = (query, labelName, labelValue, op = '=') => {
    if (!labelName || !labelValue) return query;
    const selector = `${labelName}${op}"${labelValue}"`;

    // Single-pass regex that matches either:
    //   (1) identifier{...}  — metric with existing label selector
    //   (2) identifier       — bare identifier (metric, keyword, or other)
    // We handle both in one pass to avoid offset issues.
    return query.replace(/\b([a-z_]\w*)(\{[^}]*\})?/gi, (match, name, braces, offset) => {
        if (PROMQL_KEYWORDS.has(name)) return match;

        // Skip if starts with digit (not a valid metric name)
        if (/^\d/.test(name)) return match;

        if (braces) {
            return `${name}{${braces.slice(1, -1)},${selector}}`;
        }

        // Bare identifier — check context to decide if it's a metric name.

        // Skip short tokens without underscores — likely time units (m, s, h, d),
        // PromQL modifiers, or label fragments, not metric names
        if (name.length < 3 && !name.includes('_')) return match;

        // Look ahead: if followed by '(' it's a function call, skip
        const after = query.substring(offset + match.length);
        if (/^\s*\(/.test(after)) return match;

        const before = query.substring(0, offset);

        // Skip identifiers inside by(...) / without(...) grouping clauses —
        // these are label names, not metric names.
        if (/\b(?:by|without)\s*\([^)]*$/.test(before)) return match;

        // Check if inside braces (label name/value) or square brackets (duration)
        const lastOpenBrace = before.lastIndexOf('{');
        const lastCloseBrace = before.lastIndexOf('}');
        if (lastOpenBrace > lastCloseBrace) return match;
        const lastOpenBracket = before.lastIndexOf('[');
        const lastCloseBracket = before.lastIndexOf(']');
        if (lastOpenBracket > lastCloseBracket) return match;

        // Check if inside a string literal
        const quotesBefore = (before.match(/"/g) || []).length;
        if (quotesBefore % 2 !== 0) return match;

        return `${name}{${selector}}`;
    });
};

// Inject a regex label selector (label=~"a|b"). Used for selecting a subset of
// GPU ids.
const injectLabelRegex = (query, labelName, regexValue) =>
    injectLabel(query, labelName, regexValue, '=~');

const substituteCgroupPattern = (query, pattern) => {
    query = query.replace(/,?\s*name!~"[^"]*"/g, '');
    query = query.replace(/\{\s*\}/g, '');

    if (pattern) {
        query = query.replace(/__SELECTED_CGROUPS__/g, pattern);
    }
    return query;
};

// PromQL result → plot-data shape helpers. Same transforms the baseline
// path (applyResultToPlot) and the compare path (extractExperimentCapture
// in viewer_core) apply. Extracted so the two callers can't drift.

const parseNumeric = (v) => {
    if (v === null || v === undefined) return null;
    const n = typeof v === 'number' ? v : Number(v);
    return Number.isNaN(n) ? null : n;
};

// Convert `result.data.result` (a PromQL range-query series array) into
// a flat [timeIdx, y, value] triple table plus the sorted timestamps.
// `y` is parsed from `item.metric.id` when present, else the series
// index. Missing/NaN values are preserved as null so null-cell paths
// can paint them. Returns null-valued min/max when no numeric samples.
export const promqlResultToHeatmapTriples = (results) => {
    const timeSet = new Set();
    for (const item of results) {
        for (const [ts] of item.values || []) timeSet.add(ts);
    }
    const timestamps = Array.from(timeSet).sort((a, b) => a - b);
    const timestampToIndex = new Map();
    timestamps.forEach((ts, idx) => timestampToIndex.set(ts, idx));

    const triples = [];
    let minValue = Infinity;
    let maxValue = -Infinity;
    results.forEach((item, idx) => {
        let y = idx;
        if (item.metric && item.metric.id != null) {
            const parsed = parseInt(item.metric.id, 10);
            if (!Number.isNaN(parsed)) y = parsed;
        }
        for (const [ts, rawVal] of item.values || []) {
            const ti = timestampToIndex.get(ts);
            if (ti === undefined) continue;
            const v = parseNumeric(rawVal);
            if (v != null) {
                if (v < minValue) minValue = v;
                if (v > maxValue) maxValue = v;
            }
            triples.push([ti, y, v]);
        }
    });
    return {
        timestamps,
        triples,
        minValue: Number.isFinite(minValue) ? minValue : null,
        maxValue: Number.isFinite(maxValue) ? maxValue : null,
    };
};

// Parse a series' optional `intervals` field into a clean [[lo, hi], …]
// array parallel to `values`, or `null` when absent/unusable. The field
// is NEW and OPTIONAL — present only for rate()/irate() results, absent
// for older responses and non-rate queries — so parse defensively: only
// accept a well-formed array, coerce each pair to numbers, order lo≤hi,
// and drop malformed pairs to null. Returns null when nothing is usable
// so callers can treat "has a band" as a simple truthiness check.
export const parseIntervals = (sample) => {
    const iv = sample && sample.intervals;
    if (!Array.isArray(iv) || iv.length === 0) return null;
    const out = iv.map((pair) => {
        if (!Array.isArray(pair) || pair.length < 2) return null;
        const lo = parseNumeric(pair[0]);
        const hi = parseNumeric(pair[1]);
        if (lo === null || hi === null) return null;
        return lo <= hi ? [lo, hi] : [hi, lo];
    });
    return out.some((p) => p !== null) ? out : null;
};

// Convert the first series in a PromQL range-query result into a pair
// of parallel timeData / valueData arrays. Missing/NaN values are
// preserved as null.
export const promqlResultToLinePair = (results) => {
    const first = results[0];
    const values = Array.isArray(first?.values) ? first.values : [];
    return {
        timeData: values.map((pair) => Number(pair[0])),
        valueData: values.map((pair) => parseNumeric(pair[1])),
        // Optional rate()/histogram value band, parallel to valueData; null
        // for non-rate queries. Lets compare/experiment captures carry a band.
        intervals: parseIntervals(first),
    };
};

// Build a Map<label, {timeData, valueData}> from a PromQL range-query
// result. `labelFor(item, idx)` picks the series label; returning null
// skips the series.
export const promqlResultToSeriesMap = (results, labelFor) => {
    const map = new Map();
    results.forEach((item, idx) => {
        const label = labelFor(item, idx);
        if (label == null) return;
        const values = Array.isArray(item.values) ? item.values : [];
        map.set(String(label), {
            timeData: values.map((pair) => Number(pair[0])),
            valueData: values.map((pair) => parseNumeric(pair[1])),
        });
    });
    return map;
};

const applyResultToPlot = (plot, result) => {
    // JSON matrix path never carries boxplot columns; clear any left over from
    // a prior display-mode render so line.js doesn't draw stale bands.
    plot.boxplot = null;
    plot.boxplotDecimated = false;
    if (
        result.status === 'success' &&
        result.data &&
        result.data.result &&
        result.data.result.length > 0
    ) {
        // Explicit style (set by query explorer dynamic specs) wins;
        // otherwise resolve from metric type.
        const style = plot.opts.style || resolveStyle(
            plot.opts.type,
            plot.opts.subtype,
            result,
        );
        plot._resolvedStyle = style;

        const hasMultipleSeries =
            result.data.result.length > 1 ||
            (style === 'multi' ||
                style === 'scatter' ||
                style === 'heatmap');

        // Cleared here so a prior single-series band never ghosts onto a
        // subsequent multi-series / heatmap render of the same plot; the
        // single-series branch below repopulates it when present.
        plot.intervals = null;

        if (hasMultipleSeries) {
            if (style === 'heatmap') {
                const { timestamps, triples, minValue, maxValue } =
                    promqlResultToHeatmapTriples(result.data.result);
                plot.data = triples;
                plot.time_data = timestamps;
                plot.min_value = minValue != null ? minValue : Infinity;
                plot.max_value = maxValue != null ? maxValue : -Infinity;
            } else {
                const allData = [];
                const seriesNames = [];
                // Parallel to seriesNames; the raw metrics let compare-
                // mode's baseline path re-derive labels symmetrically
                // with the experiment path (composeScatterLabel needs
                // the full label set, not the lossy series_names string).
                const seriesMetrics = [];
                // Parallel to seriesNames: each series' optional value band
                // (rate/histogram uncertainty), or null. multi.js renders these
                // only for percentile charts (few lines); high-cardinality
                // categorical multis carry them but leave them undrawn.
                const seriesIntervals = [];
                let timestamps = null;

                result.data.result.forEach((item, idx) => {
                    if (item.values && Array.isArray(item.values)) {
                        let seriesName = 'Series ' + (idx + 1);
                        if (item.metric) {
                            for (const [key, value] of Object.entries(item.metric)) {
                                if (key !== '__name__') {
                                    seriesName = value;
                                    break;
                                }
                            }
                        }

                        if (item.values.length > 0) {
                            seriesNames.push(seriesName);
                            seriesMetrics.push(item.metric || {});
                            seriesIntervals.push(parseIntervals(item));

                            if (!timestamps) {
                                timestamps = item.values.map(([ts, _]) => ts);
                                allData.push(timestamps);
                            }

                            const values = item.values.map(([_, val]) => parseFloat(val));
                            allData.push(values);
                        }
                    }
                });

                if (allData.length > 1) {
                    plot.data = allData;
                    plot.series_names = seriesNames;
                    plot.series_metrics = seriesMetrics;
                    plot.series_intervals = seriesIntervals;
                } else {
                    plot.data = [];
                    plot.series_names = [];
                    plot.series_metrics = [];
                    plot.series_intervals = [];
                }
            }
        } else {
            const sample = result.data.result[0];
            if (sample.values && Array.isArray(sample.values)) {
                const timestamps = sample.values.map(([ts, _]) => ts);
                const values = sample.values.map(([_, val]) => parseFloat(val));
                plot.data = [timestamps, values];
                // Optional rate() uncertainty band, parallel to values.
                plot.intervals = parseIntervals(sample);
            } else {
                plot.data = [];
            }
            // Line-style plots have no series legend; clear any stale entries
            // from a prior multi-series render so legends don't "ghost".
            plot.series_names = [];
            plot.series_intervals = [];
        }
    } else {
        plot.data = [];
        plot.series_names = [];
        plot.series_intervals = [];
        plot.intervals = null;
    }
};

const createDataApi = ({
    getMetadata = defaultGetMetadata,
    queryRange = defaultQueryRange,
    logHeatmapErrors = true,
} = {}) => {
    let cachedMetadata = null;

    const fetchMetadata = async () => {
        const metadataResponse = await getMetadata();

        if (metadataResponse.status !== 'success') {
            throw new Error('Failed to get metadata');
        }

        return metadataResponse.data;
    };

    const executePromQLRangeQuery = async (query, metadata, signal) => {
        const meta = metadata || cachedMetadata || await fetchMetadata();

        // Whole recording at native step; echarts LTTB decimates for
        // display. See defaultRangeFor for why we don't decimate via step.
        const { start, end, step } = defaultRangeFor(meta);

        return queryRange(query, start, end, step, 'baseline', signal);
    };

    // Apply the same per-plot query transforms the baseline path applies.
    // Returns the query to actually execute, or `null` when the plot should
    // be skipped (e.g. cgroup pattern without a resolved selector).
    //
    // `opts`:
    //   sectionRoute       — route string, used for the service/node rule.
    //   activeCgroupPattern — resolved cgroup selector, if any.
    //   serviceName        — section's service_name, if any.
    //   crossCapture       — default false. When true, skip per-service
    //                        instance injection because service KPIs are
    //                        composable (e.g. sum across instances) and
    //                        an unfiltered aggregate is the correct A/B
    //                        baseline. Node injection still applies on
    //                        both sides so a pinned-node compare stays
    //                        symmetric across captures; if the selected
    //                        node isn't present on a capture, that side
    //                        renders empty rather than silently fanning
    //                        out across all nodes.
    //   stepOverride       — nullable; when > 1 triggers histogram-stride /
    //                        counter-rate rewriting. Defaults to the
    //                        module-level _stepOverride.
    const buildEffectiveQuery = (plot, opts = {}) => {
        if (!plot.promql_query) return null;
        const {
            sectionRoute = null,
            activeCgroupPattern = null,
            serviceName = null,
            crossCapture = false,
            stepOverride = _stepOverride,
        } = opts;
        const injectTopologyLabels = !crossCapture;

        let q = plot.promql_query;
        const stepActive = stepOverride && stepOverride > 1;

        if (plot.opts.type === 'histogram') {
            q = buildHistogramQuery(
                q, plot.opts.subtype, plot.opts.percentiles,
                stepActive ? stepOverride : undefined,
            );
        }
        if (stepActive && plot.opts.type === 'delta_counter') {
            q = rewriteCounterQuery(q, stepOverride);
        }
        if (q.includes('__SELECTED_CGROUPS__')) {
            if (activeCgroupPattern) {
                q = substituteCgroupPattern(q, activeCgroupPattern);
            } else if (q.includes('!~')) {
                q = substituteCgroupPattern(q, null);
            } else {
                return null;
            }
        }
        if (_selectedNode && sectionRoute && !sectionRoute.startsWith('/service/')) {
            q = injectLabel(q, 'node', _selectedNode);
        }
        // On the GPU section, filter the non-per-GPU charts to the selected GPU
        // `id`s. Per-GPU charts group `by (id)` to draw one line per GPU and
        // must always show all GPUs, so they are exempt. Empty selection = all.
        if (_selectedGpus.length > 0 && sectionRoute === '/gpu' && !/by\s*\(\s*id\s*\)/.test(q)) {
            if (_selectedGpus.length === 1) {
                q = injectLabel(q, 'id', _selectedGpus[0]);
            } else {
                // Match any of the selected ids via a regex label selector.
                q = injectLabelRegex(q, 'id', _selectedGpus.join('|'));
            }
        }
        if (injectTopologyLabels && serviceName) {
            const inst = _selectedInstances[serviceName];
            if (inst) q = injectLabel(q, 'instance', inst);
        }
        return q;
    };

    // freshMetadata: bypass the module-level metadata cache (forces the
    // query window's maxTime to track the live TSDB instead of freezing
    // at first-fetch). Live-mode auto-refresh sets this; file-mode and
    // initial loads leave it off so the cache still saves a round-trip.
    const processDashboardData = async (data, activeCgroupPattern, sectionRoute, { freshMetadata = false, isStale = null, signal = null } = {}) => {
        if (freshMetadata) { cachedMetadata = null; clearDisplayTiles(); }
        const metadata = cachedMetadata || await fetchMetadata();
        cachedMetadata = metadata;

        const queryPlots = [];
        for (const group of data.groups || []) {
            for (const plot of collectGroupPlots(group)) {
                if (plot.promql_query) {
                    const queryToRun = buildEffectiveQuery(plot, {
                        sectionRoute,
                        activeCgroupPattern,
                        serviceName: data.metadata?.service_name,
                    });
                    if (queryToRun == null) continue;
                    queryPlots.push({ plot, query: queryToRun });
                }
            }
        }

        // Render each chart as soon as its own query lands, rather than
        // waiting for the whole section — the page fills in progressively.
        // (Guarded so the node test env, which has no mithril, is unaffected.)
        const redraw = () => {
            if (typeof m !== 'undefined' && m && typeof m.redraw === 'function') {
                m.redraw();
            }
        };
        // A drill-down refetch is superseded when a newer zoom has started
        // (isStale) or its request was aborted (signal). A superseded fetch must
        // NOT apply its result or blank the chart — the newer refetch owns it.
        const superseded = () => (signal && signal.aborted) || (isStale && isStale());
        await Promise.allSettled(
            queryPlots.map(async ({ plot, query }) => {
                try {
                    // Display mode: line-ish plots fetch the decimated boxplot
                    // binary, which now carries the aggregated measurement-
                    // uncertainty band (median of interval edges per bucket; the
                    // exact per-sample interval at native resolution), so the
                    // uncertainty bands render at every zoom level. On any failure
                    // (e.g. a non-Series result), fall back to the JSON matrix so a
                    // hiccup never blanks a chart.
                    if (_displayMode && plotUsesDisplay(plot)) {
                        try {
                            const decoded = await fetchDisplaySeries(query, metadata, signal);
                            if (superseded()) return;
                            applyDisplayToPlot(plot, decoded);
                        } catch (_) {
                            if (superseded()) return;
                            const res = await executePromQLRangeQuery(query, metadata, signal);
                            if (superseded()) return;
                            applyResultToPlot(plot, res);
                        }
                    } else {
                        const res = await executePromQLRangeQuery(query, metadata, signal);
                        if (superseded()) return;
                        applyResultToPlot(plot, res);
                    }
                } catch (e) {
                    if (superseded()) return;
                    console.error(`Failed to execute PromQL query "${plot.promql_query}":`, e);
                    plot.data = [];
                    plot.boxplot = null;
                }
                if (superseded()) return;
                redraw();
            }),
        );

        // Surface no-data plots at the bottom (mirrors service KPI UX)
        // instead of leaving silent empty chart cards mid-section.
        // Plots whose original query carries the `__SELECTED_CGROUPS__`
        // placeholder are deferred — they intentionally have no data
        // until the user picks a cgroup, at which point cgroup_selector
        // refetches them in place. Stripping them here would remove the
        // right-side "Individual Cgroups" group entirely on first load
        // and the cgroup selector would have nothing to repopulate.
        const unavailable = [];
        const plotHasData = (plot) =>
            Array.isArray(plot.data) && plot.data.some((s) => Array.isArray(s) && s.length > 0);
        const isDeferredCgroupPlot = (plot) =>
            typeof plot.promql_query === 'string'
            && plot.promql_query.includes('__SELECTED_CGROUPS__');
        for (const group of data.groups || []) {
            for (const sg of group.subgroups || []) {
                const surviving = [];
                for (const plot of (sg.plots || [])) {
                    if (!plot.promql_query
                        || plotHasData(plot)
                        || isDeferredCgroupPlot(plot)) {
                        surviving.push(plot);
                    } else {
                        unavailable.push({
                            group: group.name,
                            subgroup: sg.name || null,
                            title: plot.opts?.title || '(unnamed chart)',
                            query: plot.promql_query,
                        });
                    }
                }
                sg.plots = surviving;
            }
            group.subgroups = (group.subgroups || []).filter((sg) => (sg.plots || []).length > 0);
        }
        data.groups = (data.groups || []).filter((g) => (g.subgroups || []).length > 0);
        if (unavailable.length > 0) {
            data.metadata = data.metadata || {};
            data.metadata.unavailable_charts = unavailable;
        }

        return data;
    };

    const fetchHeatmapForPlot = async (plot) => {
        const query = plot.promql_query;
        if (!query) return null;

        // For typed histogram specs, promql_query is already the base metric selector
        let metricSelector;
        if (plot.opts.type === 'histogram') {
            metricSelector = query;
        } else if (query.includes('histogram_quantiles')) {
            // Legacy fallback: extract base metric from wrapped query
            const match = query.match(/histogram_quantiles\s*\(\s*\[[^\]]*\]\s*,\s*(.+)\)$/);
            if (!match) return null;
            metricSelector = match[1].trim();
        } else {
            return null;
        }

        // Stride the heatmap to ~pixelBudget() columns over the current range
        // (full recording, or the zoom window via _rangeOverride). executePromQL-
        // RangeQuery evaluates over that same range, so a drill-down refetches a
        // finer stride for the narrower window and sharpens it. A manual step
        // override still wins.
        const meta = cachedMetadata || await fetchMetadata();
        const { start, end } = defaultRangeFor(meta);
        const span = Math.max(1, end - start);
        const stride = (_stepOverride && _stepOverride > 1)
            ? _stepOverride
            : Math.max(1, Math.ceil(span / pixelBudget()));
        const strideSuffix = stride > 1 ? `, ${stride}` : '';
        const q = `histogram_heatmap(${metricSelector}${strideSuffix})`;

        // Prefer the binary body (zero-copy typed arrays, no JSON parse of the
        // triples). queryRangeDisplay returns { buffer } for the octet-stream
        // response; { json } (older server / error) falls through to JSON.
        try {
            const res = await ViewerApi.queryRangeDisplay(q, start, end, defaultRangeFor(meta).step, {});
            if (res.buffer) return decodeHeatmapBinary(res.buffer);
            const r = res.json;
            if (r?.data?.resultType === 'histogram_heatmap') return heatmapFromJson(r.data.result);
        } catch (_) { /* fall through to the plain JSON query */ }

        const result = await executePromQLRangeQuery(q);
        if (result.status === 'success' && result.data && result.data.resultType === 'histogram_heatmap') {
            return heatmapFromJson(result.data.result);
        }
        return null;
    };

    // Fetch a custom set of histogram quantiles for a percentile
    // (scatter) plot. Issues `histogram_quantiles([q1, q2, …], <metric>)`
    // and returns the result shaped like a percentile fetch: parallel
    // [times, qA, qB, …] columns plus matching `pXX[.YY]` series names
    // sorted by quantile. Caller picks the quantile set:
    //   - Full spectrum (default): [0.01, 0.02, …, 1.00]
    //   - Tail spectrum:           [0.9901, 0.9902, …, 1.0000]
    const formatPercentileLabel = (q) => {
        const trimmed = (q * 100).toFixed(2).replace(/\.?0+$/, '');
        return `p${trimmed}`;
    };

    const fetchSpectrumViaCapture = async (wrappedQuery, captureId, range) => {
        if (captureId === CAPTURE_BASELINE && !range) {
            return executePromQLRangeQuery(wrappedQuery);
        }
        let r = range;
        if (!r) {
            const meta = cachedMetadata || await fetchMetadata();
            r = defaultRangeFor(meta);
        }
        return queryRange(wrappedQuery, r.start, r.end, r.step, captureId);
    };

    const fetchQuantileSpectrumForPlot = async (
        plot,
        quantiles,
        captureId = CAPTURE_BASELINE,
        range = null,  // { start, end, step } — required when captureId !== baseline and the caller has the experiment's range; otherwise computed from cached metadata
    ) => {
        const query = plot.promql_query;
        if (!query) return null;

        let metricSelector;
        if (plot.opts.type === 'histogram') {
            metricSelector = query;
        } else if (query.includes('histogram_quantiles')) {
            const match = query.match(/histogram_quantiles\s*\(\s*\[[^\]]*\]\s*,\s*(.+)\)$/);
            if (!match) return null;
            metricSelector = match[1].trim();
        } else {
            return null;
        }

        if (!Array.isArray(quantiles) || quantiles.length === 0) {
            // Default: 100-quantile full spectrum.
            quantiles = [];
            for (let i = 1; i <= 100; i++) quantiles.push(i / 100);
        }
        // Always include q=0 (p0) to anchor the color scale's lower
        // bound across spectrum kinds. p0's row is hidden from the
        // heatmap rendering — it's only used to compute color_min so
        // the Full and Tail views share the same color scale (p0..p100).
        const queryQuantiles = quantiles.includes(0) ? quantiles : [0, ...quantiles];
        // Budget-stride the spectrum to ~pixelBudget() time columns over the
        // current range (full recording, or the zoom window via _rangeOverride /
        // the passed `range`) — same decimate-then-refetch model as the bucket
        // heatmap, so the full-range Full/Tail fetch is light and a drill-down
        // sharpens the window. A manual step override still wins.
        let eff = range;
        if (!eff) {
            const meta = cachedMetadata || await fetchMetadata();
            eff = defaultRangeFor(meta);
        }
        const span = Math.max(1, eff.end - eff.start);
        const stride = (_stepOverride && _stepOverride > 1)
            ? _stepOverride
            : Math.max(1, Math.ceil(span / pixelBudget()));
        const strideSuffix = stride > 1 ? `, ${stride}` : '';
        const wrapped = `histogram_quantiles([${queryQuantiles.join(', ')}], ${metricSelector}${strideSuffix})`;
        const result = await fetchSpectrumViaCapture(wrapped, captureId, range);

        if (result.status !== 'success' || !result.data?.result?.length) return null;

        let timestamps = null;
        const collected = [];
        for (const item of result.data.result) {
            if (!item.values || !item.values.length) continue;
            let name = null;
            if (item.metric) {
                for (const [k, v] of Object.entries(item.metric)) {
                    if (k !== '__name__') { name = v; break; }
                }
            }
            if (name == null) continue;
            if (!timestamps) timestamps = item.values.map(([ts]) => ts);
            const values = item.values.map(([, val]) => parseFloat(val));
            collected.push({ name, values, q: parseFloat(name) });
        }
        if (collected.length === 0 || !timestamps) return null;

        collected.sort((a, b) => a.q - b.q);

        // Pop the p0 row (if present) and derive a color-scale lower
        // bound from it. Done before building dataCols/seriesNames so
        // the heatmap never sees p0.
        let colorMinAnchor = null;
        if (collected.length > 0 && collected[0].q === 0) {
            const p0 = collected.shift();
            let m = Infinity;
            for (const v of p0.values) {
                if (v != null && !Number.isNaN(v) && v > 0 && v < m) m = v;
            }
            if (Number.isFinite(m)) colorMinAnchor = m;
        }

        const dataCols = [timestamps, ...collected.map((c) => c.values)];
        // The quantile-heatmap renderer uses these directly as y-axis
        // tick labels — `pXX` for whole percents (e.g. p10, p100) and
        // `pXX.YY` for fractional ones (e.g. p99.01).
        const seriesNames = collected.map((c) => formatPercentileLabel(c.q));

        return {
            time_data: timestamps,
            data: dataCols,
            series_names: seriesNames,
            color_min_anchor: colorMinAnchor,
        };
    };

    const fetchHeatmapsForGroups = async (groups) => {
        const plots = [];
        for (const group of groups || []) {
            for (const plot of collectGroupPlots(group)) {
                if (plot.promql_query && isHistogramPlot(plot)) {
                    plots.push(plot);
                }
            }
        }

        const results = await Promise.allSettled(plots.map((p) => fetchHeatmapForPlot(p)));

        const heatmapData = new Map();
        for (let i = 0; i < plots.length; i++) {
            if (results[i].status === 'fulfilled' && results[i].value) {
                heatmapData.set(plots[i].opts.id, results[i].value);
            } else if (results[i].status === 'rejected' && logHeatmapErrors) {
                console.error('Failed to fetch histogram heatmap:', results[i].reason);
            }
        }
        return heatmapData;
    };

    const clearMetadataCache = () => {
        cachedMetadata = null;
        clearDisplayTiles(); // decoded tiles are keyed to the current recording
    };

    return {
        executePromQLRangeQuery,
        applyResultToPlot,
        fetchHeatmapForPlot,
        fetchQuantileSpectrumForPlot,
        fetchHeatmapsForGroups,
        substituteCgroupPattern,
        processDashboardData,
        clearMetadataCache,
        buildEffectiveQuery,
    };
};

const defaultDataApi = createDataApi();

const {
    executePromQLRangeQuery,
    fetchHeatmapForPlot,
    fetchQuantileSpectrumForPlot,
    fetchHeatmapsForGroups,
    processDashboardData,
    clearMetadataCache,
    buildEffectiveQuery,
} = defaultDataApi;

export {
    executePromQLRangeQuery,
    applyResultToPlot,
    fetchHeatmapForPlot,
    fetchQuantileSpectrumForPlot,
    fetchHeatmapsForGroups,
    substituteCgroupPattern,
    processDashboardData,
    clearMetadataCache,
    createDataApi,
    setStepOverride,
    getStepOverride,
    setSelectedNode,
    getSelectedNode,
    setSelectedGpus,
    getSelectedGpus,
    setSelectedInstance,
    getSelectedInstance,
    injectLabel,
    injectLabelRegex,
    buildEffectiveQuery,
    plotUsesDisplay,
    // LOD tile cache internals (exported for tests)
    clipDecoded,
    tileLookup,
    tileStore,
    clearDisplayTiles,
};
