import { ViewerApi } from './viewer_api.js';
import { resolveStyle, buildHistogramQuery, isHistogramPlot } from './charts/metric_types.js';
import { collectGroupPlots, baseRoute } from './features/group_utils.js';
import { firstVisibleLabelValue, seriesNamer } from './charts/util/labels.js';
import { cgroupQueryVariants, runCgroupVariants } from './features/cgroup_selection.js';
import { setChartHost } from './charts/host.js';
import {
    decodeDisplayBinary, decodeHeatmapBinary, displayBucketWidth, isEntityQuery,
    plotUsesDisplay, clearDisplayTiles, clipDecoded, tileLookup, tileStore, applyDisplayToPlot,
} from './charts/display.js';
// Re-exported from charts/display.js for importers of data.js
// (viewer_core.js, and tests such as heatmap_step and display_wire_fixture).
export {
    decodeDisplayBinary, decodeHeatmapBinary, niceSecs, displayBucketWidth, isEntityQuery,
    displayIntervals, displayInterpolated,
} from './charts/display.js';

// Capture-id constants. Typos become grep-able; use these in place of
// raw 'baseline' / 'experiment' string literals.
export const CAPTURE_BASELINE = 'baseline';
export const CAPTURE_EXPERIMENT = 'experiment';

// ── Per-view memo of the capture list and per-capture metadata ──────────
//
// Every compare-mode chart asks which captures are attached and, for each
// extra capture, what its time range is. Neither answer changes while a
// view is open: attach, detach and a file swap clear the memo (via
// clearMetadataCache / app.js clearViewerCaches). Before this memo one
// `#/cpu` load of a 20-recording archive sent 347 metadata requests for
// 20 distinct answers. One promise per key is shared by
// concurrent callers; a rejection is evicted so the next caller retries
// rather than inheriting the failure.
let _capturesMemo = null;
const _captureMetaMemo = new Map(); // capture id -> Promise<metadata>

// The backend call, made now (not on a later microtask, so the backend in
// place when the caller asked is the one used) and always a promise, so a
// backend that throws synchronously rejects like one that rejects.
const asPromise = (fn) => {
    try {
        return Promise.resolve(fn());
    } catch (e) {
        return Promise.reject(e);
    }
};

export const listCaptures = () => {
    if (_capturesMemo) return _capturesMemo;
    const p = asPromise(() => ViewerApi.getCaptures());
    _capturesMemo = p;
    p.catch(() => { if (_capturesMemo === p) _capturesMemo = null; });
    return p;
};

export const captureMetadata = (captureId = CAPTURE_BASELINE) => {
    const have = _captureMetaMemo.get(captureId);
    if (have) return have;
    const p = asPromise(() => ViewerApi.getMetadata(captureId));
    _captureMetaMemo.set(captureId, p);
    p.catch(() => { if (_captureMetaMemo.get(captureId) === p) _captureMetaMemo.delete(captureId); });
    return p;
};

export const clearCaptureMemo = () => {
    _capturesMemo = null;
    _captureMetaMemo.clear();
};

// `fn` over `items` with at most `limit` in flight; results in input order.
// The N-way overlay fetches each extra capture through this rather than
// `Promise.all`: a browser allows about six connections per host, and a
// chart that fires forty requests at once starves the baseline fetches
// every other chart on the page is waiting on. A rejection is `fn`'s to
// catch; one that escapes rejects the whole map.
export const mapLimit = async (items, limit, fn) => {
    const out = new Array(items.length);
    let next = 0;
    const worker = async () => {
        while (next < items.length) {
            const i = next++;
            out[i] = await fn(items[i], i);
        }
    };
    const workers = Math.max(1, Math.min(Math.floor(limit) || 1, items.length));
    await Promise.all(Array.from({ length: workers }, worker));
    return out;
};

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
// once it fits the budget, at the recording's native cadence. Cleared (null)
// = full recording.
let _rangeOverride = null;
export const setRangeOverride = (range) => { _rangeOverride = range; };
export const getRangeOverride = () => _rangeOverride;

// Whether two `{start, end}` ranges cover the same span. A null range
// matches only null.
export const sameExtent = (a, b) => {
    if (!a || !b) return a === b;
    return a.start === b.start && a.end === b.end;
};

// The warning for a recording whose rows all sit at one timestamp, from its
// /api/v1/metadata response; null for any other recording. Such a recording
// opens with its rate and histogram charts empty, since a rate or a histogram
// quantile needs two samples. Both backends report a recording with no time
// range as 0/0, which is not a sample and gets no warning. `name` labels the
// recording in the message ("This recording" when absent).
export const singleInstantNotice = (meta, name = null) => {
    const data = meta?.data ?? meta;
    const start = Number(data?.minTime ?? data?.min_time);
    const end = Number(data?.maxTime ?? data?.max_time);
    if (!Number.isFinite(start) || !Number.isFinite(end) || end !== start || start <= 0) return null;
    const at = new Date(start * 1000).toISOString().replace(/\.\d+Z$/, 'Z');
    return `${name ?? 'This recording'} holds a single sample, at ${at}. `
        + 'Rates and histogram percentiles need at least two samples, so those '
        + 'charts are empty.';
};

// A range override against the recording's current extent `{start, end}`
// (seconds), for a followed archive whose writer evicts old rows so its start
// moves forward. Returns the override unchanged when it starts inside the
// extent, the part from the extent's start when it begins before it, and
// null when it ends at or before the extent's start (none of it is left).
// The end is not clamped: a followed archive's end only grows.
export const clampRangeToExtent = (range, extent) => {
    if (!range || !extent) return range;
    if (!Number.isFinite(extent.start) || range.start >= extent.start) return range;
    if (range.end <= extent.start) return null;
    return { start: extent.start, end: range.end };
};

// The recording's sampling interval in seconds, defaulting to 1s only when
// metadata doesn't state one. There is no 1s floor here: a recording made at
// `--interval 100ms` has ten samples per second, and a 1s step throws nine of
// them away — a 2 Hz signal in a 100ms recording reads as a flat line.
export const nativeInterval = (meta) =>
    (Number.isFinite(meta?.interval) && meta.interval > 0) ? meta.interval : 1;

// A query step or histogram stride of at least `target` seconds, never finer
// than the recording's own cadence (asking for points the producer never read
// buys interpolation, not resolution) and quantized to whole multiples of it
// so a coarser step still lands on sample boundaries. `toFixed` trims the
// binary-float tail that multiplying a sub-second interval leaves behind
// (0.1 * 3 = 0.30000000000000004), which would otherwise reach the server in
// a query string.
export const stepAtLeast = (interval, target) => {
    const native = (Number.isFinite(interval) && interval > 0) ? interval : 1;
    if (!Number.isFinite(target) || target <= native) return native;
    return Number((Math.ceil(target / native) * native).toFixed(6));
};

export const defaultRangeFor = (meta) => {
    const start = _rangeOverride ? _rangeOverride.start : meta.minTime;
    const end = _rangeOverride ? _rangeOverride.end : meta.maxTime;
    const step = _stepOverride || nativeInterval(meta);
    return { start, end, step };
};

// Counter irate needs no query rewriting for coarse granularity: the engine
// computes it per step from the `step` we pass, so a coarser step yields a
// coarser, smoothed rate. rate(x[range]) is the average over its range at each
// step. Histograms still take a stride parameter (see
// buildHistogramQuery / histogram_heatmap); gauges are sampled at step points.

const defaultGetMetadata = () => ViewerApi.getMetadata();
const defaultQueryRange = (query, start, end, step, captureId = 'baseline', signal = undefined, rateMode = 'grid', scope = null) =>
    ViewerApi.queryRange(query, start, end, step, captureId, signal, rateMode, scope);

// Per-capture queries drive A/B compare, which is only meaningful on a shared
// grid.
export const queryRangeForCapture = (captureId, query, start, end, step) =>
    defaultQueryRange(query, start, end, step, captureId, undefined, 'grid');

// Display-mode variant for a specific capture: returns the decoded boxplot
// series array ({t,min,lo,median,hi,max}, …) or null on a non-series / empty
// result. Used by compare mode to draw each capture's min/max envelope.
export const queryRangeDisplayForCapture = async (captureId, query, start, end, step, points = 500) => {
    const res = await ViewerApi.queryRangeDisplay(query, start, end, step, { points, captureId, rateMode: 'grid' });
    if (!res || !res.buffer) return null;
    return decodeDisplayBinary(res.buffer).series;
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

// Column budget for histogram bucket heatmaps, about one per CSS pixel of a
// chart cell; `displayBudget` divides it by `BUCKET_PX` for display series.
// Charts render two per row, so measure an actual chart cell when one exists
// (exact on a zoom refetch — it handles full- vs half-width) and fall back to
// half the viewport on first load. devicePixelRatio is intentionally NOT
// applied: sub-CSS-pixel detail isn't distinguishable in the envelope, and
// multiplying by it was over-fetching ~4× on half-width retina charts. The
// upper clamp also caps the histogram bucket-heatmap columns (downsampled to
// 500 in histogram_heatmap.js regardless).
const BUDGET_MIN = 300;
const BUDGET_MAX = 1200;
const pixelBudget = () => {
    if (typeof window === 'undefined') return 500;
    let w = 0;
    try { w = document.querySelector('.chart-cell')?.clientWidth || 0; } catch (_) { /* no DOM */ }
    if (!w) w = (window.innerWidth || 1000) / 2;
    return Math.max(BUDGET_MIN, Math.min(BUDGET_MAX, Math.round(w)));
};

// Buckets per chart for a display fetch: about one per BUCKET_PX of the
// chart's width (measured width clamped to BUDGET_MIN..BUDGET_MAX px), so 75
// to 300. The server rounds the bucket width up to a round width (see
// `niceSecs`), so a whole 9.6-hour recording on a half-width chart (a budget
// of about 200) is drawn in 5-minute buckets, each a median with its band and
// envelope. A window holding no more samples than the budget comes back at
// the query step, one point per sample with no min/max envelope.
const BUCKET_PX = 4;
const displayBudget = () => Math.round(pixelBudget() / BUCKET_PX);

// The query step a display fetch over the current range sends, as
// `defaultRangeFor` gives it: the Granularity override, else the
// recording's interval.
const displayStep = (meta) => _stepOverride || nativeInterval(meta);

// The step of a per-entity rate heatmap over [start, end]: the display
// bucket width (`displayBucketWidth`) rounded up to a whole multiple of the
// recording's interval. It equals the line charts' bucket width when the
// interval divides that width.
export const heatmapStep = (meta, start, end, budget = displayBudget()) => {
    const native = nativeInterval(meta);
    const step = displayStep(meta);
    const width = displayBucketWidth(start, end, step, budget) ?? step;
    return stepAtLeast(native, Math.max(width, step));
};

// A heatmap of a rate is fetched at the coarse step, where an irate is the
// average rate over each step (a rate(x[r]) is the average over r). A
// heatmap of a gauge is fetched in display mode and drawn from bucket
// medians, because a gauge at a coarse step is one sample, not an average.
const RATE_FN = /\b(?:i?rate)\s*\(/;
export const isRateQuery = (query) => RATE_FN.test(query);

// A rate fetched at `step`, each value moved from the end of the step it
// averages to the start, where a heatmap draws its column and a display
// bucket is stamped.
export const restampRateColumns = (res, step) => {
    for (const s of res?.data?.result || []) {
        s.values = s.values.map(([t, v]) => [t - step, v]);
    }
    return res;
};

// Decoded display series as a matrix result: each series' bucket medians at
// their bucket starts, as `applyResultToPlot` takes a JSON range query. A
// bucket holding one point is stamped at the point's own time, so every time
// is moved to its bucket's start (allowing 1e-4 of a bucket, as the server
// does) to give every series the same columns.
export const displayAsMatrix = (series, width) => ({
    status: 'success',
    data: {
        resultType: 'matrix',
        result: series.map((s) => ({
            metric: s.metric,
            values: Array.from(s.t, (t, i) => [
                width > 0 ? Math.floor(t / width + 1e-4) * width : t,
                String(s.median[i]),
            ]),
        })),
    },
});

// A per-entity heatmap of `query` for `captureId` over [start, end]; see
// `fetchEntityHeatmap`.
// For a rate, `columnStep` (another capture's column spacing) is used when
// given, rounded up to a whole multiple of this capture's interval, so the
// two heatmaps have the same columns.
export const fetchEntityHeatmapForCapture = async (
    captureId, query, meta, start, end, columnStep = null,
) => {
    if (isRateQuery(query)) {
        const step = columnStep > 0
            ? stepAtLeast(nativeInterval(meta), columnStep)
            : heatmapStep(meta, start, end);
        return restampRateColumns(
            await queryRangeForCapture(captureId, query, start, end, step),
            step,
        );
    }
    const step = displayStep(meta);
    const budget = displayBudget();
    const series = await queryRangeDisplayForCapture(captureId, query, start, end, step, budget);
    if (!series) return null;
    return displayAsMatrix(series, displayBucketWidth(start, end, step, budget));
};


// The display tile cache's key for `query` read within `scope`: the same
// query text reads different series in different scopes.
const tileKey = (query, scope) =>
    (Array.isArray(scope) && scope.length > 0 ? `${scope.join(',')}\u0000${query}` : query);

// Fetch + decode the display-mode boxplot response. Uses defaultRangeFor so
// it honors the zoom range override (drill-down) with the same point budget.
// `scope`, the uuids of the sources to read (a plot's `scope`), reads every
// source when null.
const fetchDisplaySeries = async (query, meta, signal, scope = null) => {
    const { start, end, step } = defaultRangeFor(meta);
    const budget = displayBudget();
    const key = tileKey(query, scope);
    const cached = tileLookup(key, start, end, step, budget);
    if (cached) return cached; // covered at sufficient resolution — no network
    const res = await ViewerApi.queryRangeDisplay(query, start, end, step, { points: budget, signal, scope });
    if (!res.buffer) {
        throw new Error(res.json?.error || 'display query returned a non-series result');
    }
    const decoded = decodeDisplayBinary(res.buffer);
    tileStore(key, start, end, displayBucketWidth(start, end, step, budget), decoded);
    return decoded;
};

let _selectedNode = null;
let _selectedInstances = {};  // { serviceName: instanceId | null }
// GPUs to filter the GPU section by; [] = all. Each entry is a
// {vendor, id} pair — see setSelectedGpus.
let _selectedGpus = [];

const setSelectedNode = (node) => { _selectedNode = node; };
const getSelectedNode = () => _selectedNode;

// When non-empty, the GPU section's non-per-GPU charts are filtered to these
// GPUs. Empty means show the aggregate (avg/sum across all GPUs). Per-GPU
// charts (those with `by (id)`) always show all GPUs and ignore this.
//
// A GPU is identified by `(vendor, id)`, not `id` alone: every vendor's sampler
// numbers its own devices from 0, so a host with an NVIDIA card and an Intel
// iGPU has two different GPUs both labelled `id="0"`. Filtering on the id alone
// would silently pull in the other vendor's series.
//
// Accepts either the {vendor, id} objects the selector passes, or bare ids
// (older callers, and recordings whose sampler sets no vendor — the Apple GPU
// sampler declares none). A bare id becomes {id} with no vendor, which filters
// on the id alone exactly as before.
const setSelectedGpus = (gpus) => {
    _selectedGpus = Array.isArray(gpus)
        ? gpus.map((g) => (g !== null && typeof g === 'object')
            ? { vendor: g.vendor != null ? String(g.vendor) : null, id: String(g.id) }
            : { vendor: null, id: String(g) })
        : [];
};
const getSelectedGpus = () => _selectedGpus;

/// Build the label selector for the current GPU selection and apply it to `q`.
///
/// Selections within one vendor collapse to a single `id=~"a|b"` matcher. A
/// selection spanning vendors cannot: `vendor=~"a|b", id=~"0|1"` is a cross
/// product that would also match (vendor a, id 1) — a GPU the user did not
/// pick. PromQL has no OR over label matchers, so the query is instead widened
/// per vendor only when that is exact, and otherwise left to the id matcher
/// alone (the pre-existing behaviour, which over-matches rather than dropping
/// data).
const applyGpuSelection = (q, selected) => {
    const vendors = new Set(selected.map((g) => g.vendor));
    const ids = [...new Set(selected.map((g) => g.id))];

    // One vendor (or none recorded): vendor pins the family, ids pick within it.
    if (vendors.size === 1) {
        const vendor = [...vendors][0];
        if (vendor) q = injectLabel(q, 'vendor', vendor);
        return ids.length === 1
            ? injectLabel(q, 'id', ids[0])
            : injectLabelRegex(q, 'id', ids.join('|'));
    }

    // Spanning vendors. If every selected vendor is fully represented at every
    // selected id, the cross product is exactly the selection and both matchers
    // are safe.
    const known = [...vendors].filter(Boolean);
    const exact = known.length === vendors.size
        && known.every((v) => ids.every((id) =>
            selected.some((g) => g.vendor === v && g.id === id)));

    if (exact) {
        q = injectLabelRegex(q, 'vendor', known.join('|'));
    }

    return ids.length === 1
        ? injectLabel(q, 'id', ids[0])
        : injectLabelRegex(q, 'id', ids.join('|'));
};

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
// Handles four forms:
//   metric{existing}  → metric{existing,label="value"}
//   {__name__="m"}    → {__name__="m",label="value"}  (a name that is not
//                       a PromQL identifier, such as `redis/commands`)
//   metric[5m]        → metric{label="value"}[5m]
//   metric            → metric{label="value"}   (bare, in expressions)
const injectLabel = (query, labelName, labelValue, op = '=') => {
    if (!labelName || !labelValue) return query;
    const selector = `${labelName}${op}"${labelValue}"`;

    // Single-pass regex that matches one of:
    //   (1) identifier{...}  — metric with existing label selector
    //   (2) identifier       — bare identifier (metric, keyword, or other)
    //   (3) {...}            — a selector with no name
    // One pass handles all three, to avoid offset issues.
    return query.replace(/\b([a-z_]\w*)(\{[^}]*\})?|\{([^}]*)\}/gi, (match, name, braces, nameless, offset) => {
        // A selector with no name: `{__name__="m", ...}`. Braces inside a
        // string literal (`label_replace(.., "{$1}", ..)`) are no selector.
        if (name === undefined) {
            const quotes = (query.substring(0, offset).match(/"/g) || []).length;
            return quotes % 2 ? match : `{${nameless},${selector}}`;
        }
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

// Fill a cgroup query's `__SELECTED_CGROUPS__` placeholder from the
// selection. The aggregate plots exclude every selected name and group member
// (`name!~"__SELECTED_CGROUPS__"`, the selection's `excluded` pattern) and the
// individual plots select the individually selected names (`name=~`, its
// `leaf` pattern). A string is one pattern for both. With nothing to exclude
// the exclusion is removed, leaving the aggregate over every cgroup; other
// matchers in the selector are kept. With no `leaf` pattern a `name=~`
// placeholder is left in place. The patterns come from cgroup_selection.js's
// `cgroupPattern`, which quotes each name for the PromQL string and leaves out
// names the engine cannot match.
const substituteCgroupPattern = (query, selection) => {
    const { leaf = null, excluded = null } = typeof selection === 'string'
        ? { leaf: selection, excluded: selection }
        : (selection || {});
    let q = excluded
        ? query.replace(/name!~"__SELECTED_CGROUPS__"/g, () => `name!~"${excluded}"`)
        : query
            .replace(/,?\s*name!~"__SELECTED_CGROUPS__"/g, '')
            .replace(/\{\s*,\s*/g, '{')
            .replace(/\{\s*\}/g, '');
    if (leaf) q = q.replace(/__SELECTED_CGROUPS__/g, () => leaf);
    return q;
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

    // Row labels, parallel to the row indices below. An `id` alone does not
    // identify a GPU — every vendor's sampler numbers its devices from 0, so a
    // host with an NVIDIA card and an Intel iGPU has two series both at id=0.
    // Indexing rows by parseInt(id) would stack them on the same row, one
    // silently overwriting the other.
    //
    // So rows are laid out by result order whenever the ids do not uniquely
    // identify the series, and each row carries its own label.
    const ids = results.map((item) => item.metric && item.metric.id);
    const numericIds = ids.map((v) => parseInt(v, 10));
    // Ids index rows only when they are unique AND dense from 0. A sparse set
    // leaves empty rows: sky publishes VRAM for its discrete GPU (id=1) and not
    // its integrated one, so indexing by id drew a blank row 0 above the only
    // real row. Falling back to result order packs the rows that exist.
    const idsUsable = numericIds.every((v) => !Number.isNaN(v))
        && new Set(numericIds).size === numericIds.length
        && Math.max(...numericIds) === numericIds.length - 1;

    // The vendor is always shown when a series carries one. An id is only
    // meaningful within a vendor — every vendor's sampler numbers its devices
    // from 0 — so "nvidia 0" identifies a GPU where "0" merely indexes one.
    // Showing it unconditionally also keeps a chart's labels stable when the
    // set of GPUs in a result changes.

    // Indexed by ROW, not by result order: when ids are usable a row is the id
    // value itself, and rows for absent ids stay unlabelled.
    //
    // Only returned when a label actually differs from its row index. A CPU
    // heatmap's labels are "0", "1", "2" — identical to the indices they sit
    // at — and handing those to the renderer made it treat the rows as
    // self-describing, dropping the "CPU" y-axis title and rendering tooltips
    // as "2" instead of "CPU 2". That regressed every per-CPU, softirq and
    // scheduler heatmap on every host, GPU or not.
    const rowLabels = [];
    results.forEach((item, idx) => {
        const m = item.metric || {};
        const row = idsUsable ? numericIds[idx] : idx;
        rowLabels[row] = m.id == null
            ? `${row}`
            : (m.vendor ? `${m.vendor} ${m.id}` : `${m.id}`);
    });

    results.forEach((item, idx) => {
        const y = idsUsable ? numericIds[idx] : idx;
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
    // A label that equals its own row index tells the renderer nothing, so the
    // whole array is withheld and the entity-titled default stands.
    const rowLabelsAreInformative = rowLabels.some(
        (label, row) => label != null && label !== String(row),
    );

    return {
        rowLabels: rowLabelsAreInformative ? rowLabels : null,
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
    // `bands` is the lossless form: present when ANY value has a band, with a
    // null entry where one does not (a point interpolated across a stretch the
    // producer never read). `intervals` is all-or-nothing and goes absent for
    // the whole series as soon as one point lacks a band, so prefer `bands`
    // where the producer offers it and fall back for older responses. Per-entry
    // nulls need no special handling — the loop below already maps a malformed
    // or missing pair to null, and `buildBandSeries` already draws a gap there.
    const iv = (sample && (sample.bands || sample.intervals)) || null;
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

// Parse a series' optional `interpolated` field into a boolean array parallel
// to `values`, or `null` when absent.
//
// A true entry marks a value the producer did not observe the interval of: the
// series was null across a stretch, and rate() spanned it because the total is
// known even though its distribution inside is not. Such a point carries no
// band — an uncertainty interval answers "how precisely do we know the average
// over THIS interval", and for an interval nobody watched that is not a number
// — so this is the only thing distinguishing it from a measured point. Charts
// render it differently rather than letting it pass as observed.
export const parseInterpolated = (sample) => {
    const flags = sample && sample.interpolated;
    if (!Array.isArray(flags) || flags.length === 0) return null;
    return flags.some(Boolean) ? flags.map(Boolean) : null;
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
        // Likewise the interpolated flags. This function rebuilds a series from
        // a hand-picked field list, so anything not named here is dropped even
        // though the wire carried it — and the drop is invisible to every check
        // except rendering the chart. That is exactly how compare mode ended up
        // with no dashed overlay while the response was perfectly correct.
        interpolated: parseInterpolated(first),
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
                const { timestamps, triples, minValue, maxValue, rowLabels } =
                    promqlResultToHeatmapTriples(result.data.result);
                plot.data = triples;
                plot.time_data = timestamps;
                // Per-row names, so a GPU heatmap can say "nvidia 0" / "intel 0"
                // where the bare id would be ambiguous. `null` for rows that
                // merely restate their index (a CPU heatmap), which leaves the
                // renderer's entity title and "CPU 2" tooltips intact.
                plot.row_labels = rowLabels;
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

                // A series with both `id` and `vendor` is named `<vendor> <id>`:
                // every vendor's sampler numbers its devices from 0, so a host
                // with an NVIDIA card and an Intel iGPU has two GPUs labelled
                // id="0". Any other series is named by the labels that differ
                // between the result's series (seriesNamer).

                const nameOf = seriesNamer(result.data.result.map((item) => item.metric || {}));
                result.data.result.forEach((item, idx) => {
                    if (item.values && Array.isArray(item.values)) {
                        let seriesName = 'Series ' + (idx + 1);
                        if (item.metric) {
                            const { id, vendor } = item.metric;
                            if (id !== undefined && vendor !== undefined) {
                                seriesName = `${vendor} ${id}`;
                            } else {
                                seriesName = nameOf(item.metric) ?? seriesName;
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
                // Which of those values the producer never observed the
                // interval of; see parseInterpolated.
                plot.interpolated = parseInterpolated(sample);
            } else {
                plot.data = [];
                plot.interpolated = null;
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

// Remove the plots of `data` that have no data, adding them to
// `unavailable`, and the subgroups and groups left empty. A plot `keep`
// returns true for stays.
//
// Plots whose original query carries the `__SELECTED_CGROUPS__`
// placeholder stay too: they intentionally have no data until the user
// picks a cgroup, at which point cgroup_selector refetches them in place.
// Stripping them would remove the right-side "Individual Cgroups" group
// entirely on first load and the cgroup selector would have nothing to
// repopulate.
const dropUnavailable = (data, keep, unavailable) => {
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
                    || keep(plot)
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

    // `rateMode` 'raw' places each point at a sample's own timestamp (the
    // single-chart view's samples toggle); the default is the step grid.
    // `scope`, the uuids of the sources to read (a plot's `scope`, a
    // section's `metadata.scope`), reads every source when null.
    const executePromQLRangeQuery = async (query, metadata, signal, rateMode = 'grid', scope = null) => {
        const meta = metadata || cachedMetadata || await fetchMetadata();

        // Whole recording at native step; echarts LTTB decimates for
        // display. See defaultRangeFor for why we don't decimate via step.
        const { start, end, step } = defaultRangeFor(meta);

        return queryRange(query, start, end, step, 'baseline', signal, rateMode, scope);
    };

    // A per-entity heatmap over the current range, as a matrix result with
    // one value per entity and column. A rate is fetched in grid mode at the
    // heatmap step (`heatmapStep`); each value is the average rate over its
    // column, stamped at the column's start, and a column the recording does
    // not cover in full is left out. Anything else is fetched in display
    // mode, each column a bucket's median, falling back to the full matrix if
    // that fails.
    const fetchEntityHeatmap = async (query, metadata, signal, scope = null) => {
        const meta = metadata || cachedMetadata || await fetchMetadata();
        const { start, end } = defaultRangeFor(meta);
        if (isRateQuery(query)) {
            const step = heatmapStep(meta, start, end);
            return restampRateColumns(
                await queryRange(query, start, end, step, 'baseline', signal, 'grid', scope),
                step,
            );
        }
        try {
            const decoded = await fetchDisplaySeries(query, meta, signal, scope);
            const width = displayBucketWidth(start, end, displayStep(meta), displayBudget());
            return displayAsMatrix(decoded.series, width);
        } catch (_) {
            if (signal && signal.aborted) throw _;
            return executePromQLRangeQuery(query, meta, signal, 'grid', scope);
        }
    };

    // Apply the same per-plot query transforms the baseline path applies.
    // Returns the query to actually execute, or `null` when the plot should
    // be skipped (e.g. cgroup pattern without a resolved selector).
    //
    // `opts`:
    //   sectionRoute       — route string, used for the service/node rule.
    //   activeCgroupPattern — resolved cgroup selector, if any.
    //   serviceName        — section's service_name, if any.
    //   intervalSec        — the queried capture's sampling interval, below
    //                        which a step override is not a stride; the
    //                        baseline's when omitted.
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
    //   stepOverride       — nullable; when coarser than the recording's own
    //                        cadence, triggers histogram-stride rewriting.
    //                        Defaults to the module-level _stepOverride.
    const buildEffectiveQuery = (plot, opts = {}) => {
        if (!plot.promql_query) return null;
        const {
            sectionRoute = null,
            activeCgroupPattern = null,
            serviceName = null,
            crossCapture = false,
            intervalSec = null,
            stepOverride = _stepOverride,
        } = opts;
        const injectTopologyLabels = !crossCapture;

        let q = plot.promql_query;
        // Coarser than native, not coarser than one second: on a 100ms
        // recording a 1s override is a real 10x stride and must reach the
        // query, and on a 5s recording a 1s override is not a stride at all.
        const stepActive = stepOverride
            && stepOverride > (intervalSec ?? nativeInterval(cachedMetadata));

        if (plot.opts.type === 'histogram') {
            q = buildHistogramQuery(
                q, plot.opts.subtype, plot.opts.percentiles,
                stepActive ? stepOverride : undefined,
            );
        }
        // A counter's irate needs no rewrite: the engine computes it per step
        // from the `step` we pass. rate(x[r]) averages over r at any step.
        if (q.includes('__SELECTED_CGROUPS__')) {
            q = substituteCgroupPattern(q, activeCgroupPattern);
            // An individual plot with nothing individually selected.
            if (q.includes('__SELECTED_CGROUPS__')) return null;
        }
        if (_selectedNode && sectionRoute && !sectionRoute.startsWith('/service/')) {
            q = injectLabel(q, 'node', _selectedNode);
        }
        // On the GPU section, filter the non-per-GPU charts to the selected GPU
        // `id`s. Per-GPU charts group `by (id)` to draw one line per GPU and
        // must always show all GPUs, so they are exempt. Empty selection = all.
        // Per-GPU charts group by id (and by vendor, since an id is only
        // unique within one vendor) to draw one line per GPU; they must show
        // every GPU regardless of the selection, so they are exempt.
        if (_selectedGpus.length > 0 && baseRoute(sectionRoute) === '/gpu'
            && !/by\s*\(\s*id\s*[,)]/.test(q)) {
            q = applyGpuSelection(q, _selectedGpus);
        }
        if (injectTopologyLabels && serviceName) {
            const inst = _selectedInstances[serviceName];
            if (inst) q = injectLabel(q, 'instance', inst);
        }
        return q;
    };

    // The queries a plot runs, as `{ query, label }`: one per cgroup query
    // variant (cgroupQueryVariants) when `activeCgroupPattern` is a selection
    // with groups, otherwise buildEffectiveQuery's one query with a null
    // label. A plot the cgroup selector has already substituted carries its
    // queries in `cgroup_queries`, with the first as its `promql_query`; those
    // are run while the query is unchanged. null when there is nothing to run.
    const buildEffectiveQueries = (plot, opts = {}) => {
        if (!plot.promql_query) return null;
        const selection = opts.activeCgroupPattern ?? null;
        const variants = plot.cgroup_queries && plot.promql_query === plot.cgroup_queries[0]?.query
            ? plot.cgroup_queries.map(({ query, label }) => ({ query, label, selection: null }))
            : cgroupQueryVariants(plot.promql_query, selection);
        const out = [];
        for (const v of variants) {
            const query = buildEffectiveQuery(
                { ...plot, promql_query: v.query },
                { ...opts, activeCgroupPattern: v.selection },
            );
            if (query != null) out.push({ query, label: v.label });
        }
        return out.length > 0 ? out : null;
    };

    // freshMetadata: bypass the module-level metadata cache (forces the
    // query window's maxTime to track the live TSDB instead of freezing
    // at first-fetch). Live-mode auto-refresh sets this; file-mode and
    // initial loads leave it off so the cache still saves a round-trip.
    // onPrimary: when given, the counter and gauge queries are sent first
    // and `onPrimary(data)` is called once they have answered, before the
    // histogram queries are sent, so a section can render while those are
    // in flight. Histogram charts with no data yet are marked
    // `plot.pending` until theirs land: true for a placeholder, 'hidden'
    // for one not drawn. Without it every query is sent at
    // once, as for a refetch of a section already on screen.
    const processDashboardData = async (data, activeCgroupPattern, sectionRoute, { freshMetadata = false, isStale = null, signal = null, onPrimary = null } = {}) => {
        if (freshMetadata) { cachedMetadata = null; clearDisplayTiles(); }
        const metadata = cachedMetadata || await fetchMetadata();
        // A superseded fetch's metadata may describe a recording that has
        // since been replaced, so it is not cached.
        if (!((signal && signal.aborted) || (isStale && isStale()))) cachedMetadata = metadata;

        const queryPlots = [];
        for (const group of data.groups || []) {
            for (const plot of collectGroupPlots(group)) {
                if (plot.promql_query) {
                    const queries = buildEffectiveQueries(plot, {
                        sectionRoute,
                        activeCgroupPattern,
                        serviceName: data.metadata?.service_name,
                    });
                    if (queries == null) continue;
                    queryPlots.push({ plot, query: queries[0].query, queries });
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
        const plotHasData = (plot) =>
            Array.isArray(plot.data) && plot.data.some((s) => Array.isArray(s) && s.length > 0);
        // Counter and gauge queries go first: they are cheap and most of a
        // section. Histogram queries read every bucket of every row, and
        // sent alongside they hold the server's query slots while the rest
        // wait.
        const isSecondary = ({ plot, query }) => isHistogramPlot(plot) || query.includes('histogram_');
        const primary = onPrimary ? queryPlots.filter((q) => !isSecondary(q)) : queryPlots;
        const secondary = onPrimary ? queryPlots.filter(isSecondary) : [];
        for (const { plot } of secondary) {
            if (!plotHasData(plot)) plot.pending = true;
        }
        const run = async ({ plot, queries }) => {
            const matrix = (fetch) => runCgroupVariants(queries, fetch);
            const scope = plot.scope ?? null;
            // A chart of several cgroup queries is fetched as matrices: each
            // display query is bucketed on its own and may be served from a
            // cached tile of another width, so their timestamps can differ.
            // So is a group's query alone, which the matrix path names.
            const display = _displayMode && queries.length === 1 && queries[0].label == null;
            try {
                // Display mode: line-ish plots fetch the decimated boxplot
                // binary, which now carries the aggregated measurement-
                // uncertainty band (median of interval edges per bucket; the
                // exact per-sample interval at native resolution), so the
                // uncertainty bands render at every zoom level. On any failure
                // (e.g. a non-Series result), fall back to the JSON matrix so a
                // hiccup never blanks a chart.
                if (display && plotUsesDisplay(plot)) {
                    try {
                        const decoded = await fetchDisplaySeries(queries[0].query, metadata, signal, scope);
                        if (superseded()) return;
                        applyDisplayToPlot(plot, decoded);
                    } catch (_) {
                        if (superseded()) return;
                        const res = await matrix((q) => executePromQLRangeQuery(q, metadata, signal, 'grid', scope));
                        if (superseded()) return;
                        applyResultToPlot(plot, res);
                    }
                } else if (display && isEntityQuery(plot.promql_query)
                    && plot.opts?.type !== 'histogram') {
                    const res = await matrix((q) => fetchEntityHeatmap(q, metadata, signal, scope));
                    if (superseded()) return;
                    applyResultToPlot(plot, res);
                } else {
                    const res = await matrix((q) => executePromQLRangeQuery(q, metadata, signal, 'grid', scope));
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
            // A held-back chart that came back empty stays hidden until the
            // final drop removes it.
            if (!(plot.pending === 'hidden' && !plotHasData(plot))) plot.pending = false;
            redraw();
        };
        // Surface no-data plots at the bottom (mirrors service KPI UX)
        // instead of leaving silent empty chart cards mid-section.
        const unavailable = [];
        await Promise.allSettled(primary.map(run));
        if (secondary.length > 0 && !superseded()) {
            // A subgroup whose counters and gauges all came back empty is
            // likely a sampler the recording lacks; its histogram charts
            // are held back (`pending` 'hidden') until they land rather
            // than holding places that may be dropped.
            const primaryPlots = new Set(primary.map(({ plot }) => plot));
            for (const group of data.groups || []) {
                for (const sg of group.subgroups || []) {
                    const plots = sg.plots || [];
                    const own = plots.filter((plot) => primaryPlots.has(plot));
                    if (own.length > 0 && !own.some(plotHasData)) {
                        for (const plot of plots) {
                            if (plot.pending === true) plot.pending = 'hidden';
                        }
                    }
                }
            }
            const inFlight = new Set(secondary.map(({ plot }) => plot));
            dropUnavailable(data, (plot) => inFlight.has(plot), unavailable);
            if (unavailable.length > 0) {
                data.metadata = data.metadata || {};
                data.metadata.unavailable_charts = unavailable.slice();
            }
            onPrimary(data);
        }
        await Promise.allSettled(secondary.map(run));

        dropUnavailable(data, () => false, unavailable);
        for (const { plot } of secondary) {
            if (!superseded()) plot.pending = false;
        }
        if (unavailable.length > 0) {
            data.metadata = data.metadata || {};
            data.metadata.unavailable_charts = unavailable;
        }

        return data;
    };

    const fetchHeatmapForPlot = async (plot) => {
        const query = plot.promql_query;
        if (!query) return null;
        const scope = plot.scope ?? null;

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
        const span = Math.max(0, end - start);
        const native = nativeInterval(meta);
        const stride = (_stepOverride && _stepOverride > native)
            ? _stepOverride
            : stepAtLeast(native, span / pixelBudget());
        // Omit the argument at native resolution: the engine then emits one
        // column per tick, which is finer than any stride we could name.
        const strideSuffix = stride > native ? `, ${stride}` : '';
        const q = `histogram_heatmap(${metricSelector}${strideSuffix})`;

        // Prefer the binary body (zero-copy typed arrays, no JSON parse of the
        // triples). queryRangeDisplay returns { buffer } for the octet-stream
        // response; { json } (older server / error) falls through to JSON.
        try {
            const res = await ViewerApi.queryRangeDisplay(q, start, end, defaultRangeFor(meta).step, { scope });
            if (res.buffer) return decodeHeatmapBinary(res.buffer);
            const r = res.json;
            if (r?.data?.resultType === 'histogram_heatmap') return heatmapFromJson(r.data.result);
        } catch (_) { /* fall through to the plain JSON query */ }

        const result = await executePromQLRangeQuery(q, meta, undefined, 'grid', scope);
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

    const fetchSpectrumViaCapture = async (wrappedQuery, captureId, range, scope = null) => {
        if (captureId === CAPTURE_BASELINE && !range) {
            return executePromQLRangeQuery(wrappedQuery, undefined, undefined, 'grid', scope);
        }
        let r = range;
        if (!r) {
            const meta = cachedMetadata || await fetchMetadata();
            r = defaultRangeFor(meta);
        }
        return queryRange(wrappedQuery, r.start, r.end, r.step, captureId, undefined, 'grid', scope);
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
        // Metadata is needed either way: for the range when the caller passed
        // none, and for the recording's cadence — which is the stride floor —
        // in both cases.
        const meta = cachedMetadata || await fetchMetadata();
        const eff = range || defaultRangeFor(meta);
        const span = Math.max(0, eff.end - eff.start);
        // The stride floor is the queried capture's own cadence: another
        // capture may be read more slowly than the baseline.
        let native = nativeInterval(meta);
        if (captureId !== CAPTURE_BASELINE) {
            const own = range?.interval
                ? range
                : await captureMetadata(captureId).then((r) => r?.data ?? r, () => null);
            if (Number.isFinite(own?.interval) && own.interval > 0) native = own.interval;
        }
        const stride = (_stepOverride && _stepOverride > native)
            ? _stepOverride
            : stepAtLeast(native, span / pixelBudget());
        const strideSuffix = stride > native ? `, ${stride}` : '';
        const wrapped = `histogram_quantiles([${queryQuantiles.join(', ')}], ${metricSelector}${strideSuffix})`;
        const result = await fetchSpectrumViaCapture(wrapped, captureId, range, plot.scope ?? null);

        if (result.status !== 'success' || !result.data?.result?.length) return null;

        let timestamps = null;
        const collected = [];
        for (const item of result.data.result) {
            if (!item.values || !item.values.length) continue;
            const name = firstVisibleLabelValue(item.metric);
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
        clearCaptureMemo(); // the capture list and every capture's range too
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
        buildEffectiveQueries,
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
    buildEffectiveQueries,
} = defaultDataApi;

// line.js, scatter.js and charts/util/utils.js call these four through
// charts/host.js. Registering them when this module loads gives every page
// that loads data.js (the viewer, the static site, the embed demo) the total
// toggle, the quantile spectrum and the step override.
setChartHost({
    executePromQLRangeQuery,
    applyResultToPlot,
    fetchQuantileSpectrumForPlot,
    getStepOverride,
});

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
    applyGpuSelection,
    setSelectedInstance,
    getSelectedInstance,
    injectLabel,
    injectLabelRegex,
    buildEffectiveQuery,
    buildEffectiveQueries,
    plotUsesDisplay,
    applyDisplayToPlot,
    // LOD tile cache internals (exported for tests)
    clipDecoded,
    tileLookup,
    tileStore,
    clearDisplayTiles,
};
