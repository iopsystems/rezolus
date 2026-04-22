// Compare-mode chart adapter.
//
// Strategies convert a normal single-capture plot spec plus two normalized
// per-capture payloads into a rendering. Each strategy returns one of:
//
//   1. A transformed plot spec (plain object) — the caller passes it to
//      `m(Chart, {spec, ...})` exactly like a single-capture spec. This is
//      the overlay path, used by line charts.
//
//   2. A Mithril vnode — the caller renders it directly. Used when a single
//      chart slot becomes multiple sibling charts (side-by-side heatmaps,
//      histogram heatmaps) or a single diff chart composed in-place.
//
//   3. An object `{ _splitSpecs: Spec[] }` — the caller iterates and wraps
//      each spec in its own `m(Chart, ...)` sibling. Used when multi-series
//      or percentile charts split into one sub-chart per intersected label.
//
// Timestamp translation, null propagation for diff math, and the label
// intersection rule for multi/scatter are owned here. No DOM, no echarts
// calls — only Mithril vnode construction and spec mutation.
//
// Inputs:
//   renderCompareChart({ spec, captures, anchors, toggles,
//                        chartsState, interval, Chart })
//
//   captures: [
//     { id: 'baseline',   timeData, valueData, seriesMap?, heatmapData? },
//     { id: 'experiment', timeData, valueData, seriesMap?, heatmapData? },
//   ]
//   anchors: { baseline: ms, experiment: ms }  — subtracted from each
//            capture's timestamps to produce a relative (`+Xs`) x-axis.

import { toRelative, nullDiff, intersectLabels, longerDuration } from './util/compare_math.js';
import { DIVERGING_BLUE_GREEN, nullCellColor } from './util/colormap.js';

export const BASELINE_COLOR = '#2E5BFF';
export const EXPERIMENT_COLOR = '#00C46A';

/**
 * Format a relative offset in milliseconds as `+Xs`, `+XmYs`, or `+XhYm`.
 */
export const relativeTimeFormatter = (ms) => {
    const totalSec = Math.round(ms / 1000);
    const sign = totalSec < 0 ? '-' : '+';
    const s = Math.abs(totalSec);
    const h = Math.floor(s / 3600);
    const m = Math.floor((s % 3600) / 60);
    const sec = s % 60;
    if (h > 0) return `${sign}${h}h${m}m`;
    if (m > 0) return `${sign}${m}m${sec}s`;
    return `${sign}${sec}s`;
};

/**
 * Dispatch on chart style and delegate to the matching strategy.
 * Returns a transformed spec, a Mithril vnode, a `{ _splitSpecs }` marker,
 * or `false` when the style is not handled (caller should fall back to
 * single-capture rendering).
 */
export const renderCompareChart = (opts) => {
    const style = opts.spec?.opts?.style || opts.spec?._resolvedStyle;
    switch (style) {
        case 'line':              return overlayLine(opts);
        case 'heatmap':           return sideBySideHeatmap(opts);
        case 'multi':             return splitMultiToSubgroup(opts);
        case 'scatter':           return splitScatterToSubgroup(opts);
        case 'histogram_heatmap': return sideBySideHistogramHeatmap(opts);
        default:
            return false;
    }
};

// ── Helpers ──────────────────────────────────────────────────────────

// Subtract an anchor (seconds) from every timestamp in an array.
// Returns a new array; never mutates the input.
const rebase = (timeDataSec, anchorSec) => timeDataSec.map((t) => t - anchorSec);

const anchorSecondsFor = (anchors, id) =>
    ((anchors && anchors[id]) || 0) / 1000;

// ── Strategies ───────────────────────────────────────────────────────

/**
 * Overlay baseline + experiment on the same line chart.
 * Returns a transformed spec with a `multiSeries` field that `line.js`
 * already understands. Falls back to returning `false` when either
 * capture is unusable — the caller can then render baseline-only.
 */
const overlayLine = ({ spec, captures, anchors }) => {
    const baseline = captures.find((c) => c.id === 'baseline');
    const experiment = captures.find((c) => c.id === 'experiment');
    if (!baseline || !experiment) return false;

    const baseSec = anchorSecondsFor(anchors, 'baseline');
    const expSec = anchorSecondsFor(anchors, 'experiment');

    const seriesList = [];
    if (Array.isArray(baseline.timeData) && baseline.timeData.length > 0) {
        seriesList.push({
            name: 'baseline',
            color: BASELINE_COLOR,
            timeData: rebase(baseline.timeData, baseSec),
            valueData: baseline.valueData || [],
            fill: false,
        });
    }
    if (Array.isArray(experiment.timeData) && experiment.timeData.length > 0) {
        seriesList.push({
            name: 'experiment',
            color: EXPERIMENT_COLOR,
            timeData: rebase(experiment.timeData, expSec),
            valueData: experiment.valueData || [],
            fill: false,
        });
    }
    if (seriesList.length === 0) return false;

    return {
        ...spec,
        multiSeries: seriesList,
        xAxisFormatter: relativeTimeFormatter,
    };
};

/**
 * Render baseline and experiment heatmaps as two siblings side-by-side.
 * When the per-chart `diff` toggle is on, renders a single diff heatmap
 * instead via `renderDiffHeatmap`.
 */
const sideBySideHeatmap = ({ spec, captures, anchors, toggles, chartsState, interval, Chart }) => {
    const chartId = spec?.opts?.id;
    const diffMode = !!(toggles && chartId && toggles[chartId] && toggles[chartId].diff);
    if (diffMode) {
        return renderDiffHeatmap({ spec, captures, anchors, chartsState, interval, Chart });
    }

    const [a, b] = captures;
    const makeSlotSpec = (cap) => {
        const anchorSec = anchorSecondsFor(anchors, cap.id);
        const timeData = cap.timeData || spec.time_data || [];
        return {
            ...spec,
            opts: { ...spec.opts, title: `${spec.opts.title} — ${cap.id}` },
            time_data: rebase(timeData, anchorSec),
            data: cap.heatmapData || spec.data,
            min_value: cap.minValue != null ? cap.minValue : spec.min_value,
            max_value: cap.maxValue != null ? cap.maxValue : spec.max_value,
            xAxisFormatter: relativeTimeFormatter,
        };
    };

    return m('div.compare-heatmap-pair', [
        m('div.compare-slot',
            m(Chart, { spec: makeSlotSpec(a), chartsState, interval })),
        m('div.compare-slot',
            m(Chart, { spec: makeSlotSpec(b), chartsState, interval })),
    ]);
};

/**
 * Render a single diff heatmap (experiment − baseline) using the
 * diverging palette and a symmetric visualMap. Null cells are painted
 * with a theme-aware neutral color instead of falling through to the
 * color scale at zero.
 *
 * heatmap.js ingests its `data` as a flat array of `[timeIdx, y, value]`
 * triples (not a 2D matrix), so we emit that shape directly — including
 * null-valued triples so the null-cell color path in heatmap.js can
 * paint them.
 */
const renderDiffHeatmap = ({ spec, captures, anchors, chartsState, interval, Chart }) => {
    const [a, b] = captures;
    const aMatrix = a.heatmapMatrix || null;
    const bMatrix = b.heatmapMatrix || null;

    // Guard: diff requires both captures to provide a normalized matrix
    // (rows × time bins). The normalization step lives in the caller
    // (viewer_core). Without it, bail and fall through to no-data.
    if (!aMatrix || !bMatrix) return false;

    const rows = Math.min(aMatrix.length, bMatrix.length);
    const bins = Math.min(
        (aMatrix[0] || []).length,
        (bMatrix[0] || []).length,
    );

    const triples = [];
    let absMax = 0;
    for (let r = 0; r < rows; r++) {
        for (let c = 0; c < bins; c++) {
            const av = aMatrix[r][c];
            const bv = bMatrix[r][c];
            const d = nullDiff(bv, av); // experiment − baseline
            if (d != null && Math.abs(d) > absMax) absMax = Math.abs(d);
            triples.push([c, r, d]);
        }
    }

    const baselineAnchorSec = anchorSecondsFor(anchors, 'baseline');
    const timeData = (a.timeData || spec.time_data || []).slice(0, bins);

    const isDark = typeof document !== 'undefined'
        && document.body
        && document.body.classList.contains('theme-dark');

    const diffSpec = {
        ...spec,
        opts: { ...spec.opts, title: `${spec.opts.title} (experiment − baseline)` },
        time_data: rebase(timeData, baselineAnchorSec),
        data: triples,
        min_value: -absMax,
        max_value: absMax,
        colormap: DIVERGING_BLUE_GREEN,
        symmetricBounds: true,
        nullCellColor: nullCellColor(isDark),
        xAxisFormatter: relativeTimeFormatter,
    };

    return m('div.compare-heatmap-diff',
        m(Chart, { spec: diffSpec, chartsState, interval }));
};

/**
 * Split a `multi` chart (e.g. per-CPU, per-cgroup line) into one overlay
 * line chart per shared label.
 */
const splitMultiToSubgroup = ({ spec, captures, anchors }) =>
    splitIntoOverlayLines({ spec, captures, anchors, labelFor: multiLabel });

/**
 * Split a `scatter` chart (histogram percentiles) into one overlay line
 * chart per shared quantile label.
 */
const splitScatterToSubgroup = ({ spec, captures, anchors }) =>
    splitIntoOverlayLines({ spec, captures, anchors, labelFor: percentileLabel });

const splitIntoOverlayLines = ({ spec, captures, anchors, labelFor: _labelFor }) => {
    const baseline = captures.find((c) => c.id === 'baseline');
    const experiment = captures.find((c) => c.id === 'experiment');
    if (!baseline || !experiment) return false;

    const mapA = baseline.seriesMap || new Map();
    const mapB = experiment.seriesMap || new Map();
    const labelsA = new Set(mapA.keys());
    const labelsB = new Set(mapB.keys());
    const shared = [...intersectLabels(labelsA, labelsB)].sort();

    const baseSec = anchorSecondsFor(anchors, 'baseline');
    const expSec = anchorSecondsFor(anchors, 'experiment');

    const specs = shared.map((label) => {
        const a = mapA.get(label);
        const b = mapB.get(label);
        return {
            ...spec,
            opts: {
                ...spec.opts,
                id: `${spec.opts.id || 'chart'}::${label}`,
                title: `${spec.opts.title} — ${label}`,
                style: 'line',
            },
            multiSeries: [
                {
                    name: 'baseline',
                    color: BASELINE_COLOR,
                    timeData: rebase(a.timeData, baseSec),
                    valueData: a.valueData,
                    fill: false,
                },
                {
                    name: 'experiment',
                    color: EXPERIMENT_COLOR,
                    timeData: rebase(b.timeData, expSec),
                    valueData: b.valueData,
                    fill: false,
                },
            ],
            xAxisFormatter: relativeTimeFormatter,
        };
    });

    return { _splitSpecs: specs };
};

const multiLabel = (r) => {
    const mm = r.metric || {};
    return Object.keys(mm).sort().filter((k) => k !== '__name__')
        .map((k) => `${k}=${mm[k]}`).join(',');
};

const percentileLabel = (r) => {
    const q = Number(r.metric && r.metric.quantile);
    if (!Number.isFinite(q)) return 'unknown';
    return `p${(q * 100).toFixed(2).replace(/\.?0+$/, '')}`;
};

/**
 * Render baseline and experiment histogram heatmaps side-by-side. No
 * diff variant — a meaningful diff would need a per-bucket log-scale
 * divergence metric that's out of scope for this iteration.
 */
const sideBySideHistogramHeatmap = ({ spec, captures, anchors, chartsState, interval, Chart }) => {
    const [a, b] = captures;
    const makeSlotSpec = (cap) => {
        const anchorSec = anchorSecondsFor(anchors, cap.id);
        const timeData = cap.timeData || spec.time_data || [];
        return {
            ...spec,
            opts: {
                ...spec.opts,
                title: `${spec.opts.title} — ${cap.id}`,
                style: 'histogram_heatmap',
            },
            time_data: rebase(timeData, anchorSec),
            data: cap.heatmapData || spec.data,
            bucket_bounds: cap.bucketBounds || spec.bucket_bounds,
            min_value: cap.minValue != null ? cap.minValue : spec.min_value,
            max_value: cap.maxValue != null ? cap.maxValue : spec.max_value,
            xAxisFormatter: relativeTimeFormatter,
        };
    };

    return m('div.compare-heatmap-pair', [
        m('div.compare-slot',
            m(Chart, { spec: makeSlotSpec(a), chartsState, interval })),
        m('div.compare-slot',
            m(Chart, { spec: makeSlotSpec(b), chartsState, interval })),
    ]);
};

// Re-export utilities consumed by strategies.
export { toRelative, nullDiff, intersectLabels, longerDuration, DIVERGING_BLUE_GREEN, nullCellColor };
