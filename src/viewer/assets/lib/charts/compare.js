// Compare-mode chart adapter.
//
// Strategies convert a normal single-capture plot spec plus two normalized
// per-capture payloads into a tagged-union rendering result:
//
//   { kind: 'spec',     spec }    — caller renders `m(Chart, {spec, ...})`.
//                                   Overlay path, used by line charts.
//
//   { kind: 'vnode',    vnode }   — caller renders the vnode directly.
//                                   Used when a single chart slot becomes
//                                   multiple sibling charts (side-by-side
//                                   heatmaps, histogram heatmaps) or a
//                                   single diff chart composed in-place.
//
//   { kind: 'split',    specs }   — caller iterates and wraps each spec
//                                   in its own `m(Chart, ...)` sibling.
//                                   Used when multi-series or percentile
//                                   charts split into one sub-chart per
//                                   intersected label.
//
//   { kind: 'fallback' }          — style not handled, or the captures
//                                   can't be combined (missing data).
//                                   Caller falls back to single-capture
//                                   baseline-only rendering.
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

import { nullDiff, intersectLabels, canonicalQuantileLabel } from './util/compare_math.js';
import { DIVERGING_BLUE_GREEN, DIVERGING_BLUE_GREEN_DARK, nullCellColor, resampleDivergingForRange } from './util/colormap.js';
import { resolvedStyle } from './metric_types.js';
import { isDarkTheme } from './base.js';
import { CAPTURE_BASELINE, CAPTURE_EXPERIMENT } from '../data.js';

// Colors sourced from --compare-baseline / --compare-experiment in
// style.css. The getter reads CSS custom properties lazily so a theme
// swap (light/dark) would pick up new values without a full reload.
const cssColor = (name, fallback) => {
    if (typeof getComputedStyle === 'undefined' || typeof document === 'undefined') return fallback;
    const v = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
    return v || fallback;
};
export const BASELINE_COLOR = cssColor('--compare-baseline', '#2E5BFF');
export const EXPERIMENT_COLOR = cssColor('--compare-experiment', '#00C46A');

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

// Shared fallback sentinel — tells the caller to render the baseline
// single-capture spec instead. Frozen so no strategy can mutate it.
const FALLBACK = Object.freeze({ kind: 'fallback' });

/**
 * Dispatch on chart style and delegate to the matching strategy.
 * Returns a tagged-union result; see the module docstring above for
 * the four kinds.
 */
export const renderCompareChart = (opts) => {
    const style = resolvedStyle(opts.spec);
    switch (style) {
        case 'line':              return overlayLine(opts);
        case 'heatmap':           return sideBySideHeatmap(opts);
        case 'multi':             return splitMultiToSubgroup(opts);
        case 'scatter':           return splitScatterToSubgroup(opts);
        case 'histogram_heatmap': return sideBySideHistogramHeatmap(opts);
        default:
            return FALLBACK;
    }
};

// ── Helpers ──────────────────────────────────────────────────────────

// Subtract an anchor (seconds) from every timestamp in an array.
// Returns a new array; never mutates the input.
const rebase = (timeDataSec, anchorSec) => timeDataSec.map((t) => t - anchorSec);

// The per-capture anchor in seconds. Each capture's effective anchor is
// the capture's natural start (first sample) plus a user-configured
// offset. `anchors[id]` is stored as a signed ms offset from that start
// (0 = "no user shift"). This keeps `rebase` producing small relative
// offsets even when the raw timestamps are absolute-epoch seconds.
const anchorSecondsFor = (anchors, id, timeDataSec) => {
    const naturalStart = Array.isArray(timeDataSec) && timeDataSec.length > 0
        ? timeDataSec[0]
        : 0;
    const userOffsetMs = (anchors && anchors[id]) || 0;
    return naturalStart + userOffsetMs / 1000;
};

// ── Strategies ───────────────────────────────────────────────────────

/**
 * Overlay baseline + experiment on the same line chart.
 * Returns a transformed spec with a `multiSeries` field that `line.js`
 * already understands. Falls back to returning `false` when either
 * capture is unusable — the caller can then render baseline-only.
 */
const overlayLine = ({ spec, captures, anchors }) => {
    const baseline = captures.find((c) => c.id === CAPTURE_BASELINE);
    const experiment = captures.find((c) => c.id === CAPTURE_EXPERIMENT);
    if (!baseline || !experiment) return false;

    const baseSec = anchorSecondsFor(anchors, CAPTURE_BASELINE, baseline.timeData);
    const expSec = anchorSecondsFor(anchors, CAPTURE_EXPERIMENT, experiment.timeData);

    const seriesList = [];
    if (Array.isArray(baseline.timeData) && baseline.timeData.length > 0) {
        seriesList.push({
            name: CAPTURE_BASELINE,
            color: BASELINE_COLOR,
            timeData: rebase(baseline.timeData, baseSec),
            valueData: baseline.valueData || [],
            fill: false,
        });
    }
    if (Array.isArray(experiment.timeData) && experiment.timeData.length > 0) {
        seriesList.push({
            name: CAPTURE_EXPERIMENT,
            color: EXPERIMENT_COLOR,
            timeData: rebase(experiment.timeData, expSec),
            valueData: experiment.valueData || [],
            fill: false,
        });
    }
    if (seriesList.length === 0) return FALLBACK;

    return {
        kind: 'spec',
        spec: {
            ...spec,
            multiSeries: seriesList,
            xAxisFormatter: relativeTimeFormatter,
        },
    };
};

/**
 * Render baseline and experiment heatmaps as two siblings side-by-side.
 * When the per-chart `diff` toggle is on, renders a single diff heatmap
 * instead via `renderDiffHeatmap`.
 */
// Build a side-by-side pair (baseline + experiment as sibling charts)
// with a unified color domain. `styleOverride` swaps the per-slot
// `opts.style`; `extraSlotFields(cap)` contributes additional
// top-level spec fields (e.g. bucket_bounds for histogram heatmaps).
const sideBySidePair = ({ spec, captures, anchors, chartsState, interval, Chart, styleOverride, extraSlotFields }) => {
    const [a, b] = captures;
    // Unify color domain across both slots: same visualMap min/max so a
    // cell of equal intensity reads the same color on both sides.
    const { min: sharedMin, max: sharedMax } = unifiedHeatmapRange(a, b, spec);
    const makeSlotSpec = (cap) => {
        const timeData = cap.timeData || spec.time_data || [];
        const anchorSec = anchorSecondsFor(anchors, cap.id, timeData);
        // Per-slot id: Chart registers itself in chartsState.charts
        // keyed by opts.id. Without this suffix both slots collide
        // under the same key and datazoom only dispatches to one.
        const opts = {
            ...spec.opts,
            id: `${spec.opts.id || 'chart'}::${cap.id}`,
            title: `${spec.opts.title} — ${cap.id}`,
        };
        if (styleOverride) opts.style = styleOverride;
        return {
            ...spec,
            ...(extraSlotFields ? extraSlotFields(cap) : null),
            opts,
            time_data: rebase(timeData, anchorSec),
            data: cap.heatmapData || spec.data,
            min_value: sharedMin,
            max_value: sharedMax,
            xAxisFormatter: relativeTimeFormatter,
        };
    };

    const slot = (cap, dotCls) => m('div.compare-slot', [
        m('div.compare-slot-label', [
            m(`span.compare-dot.${dotCls}`, '\u25CF'),
            m('span', cap.id),
        ]),
        m(Chart, { spec: makeSlotSpec(cap), chartsState, interval }),
    ]);
    return {
        kind: 'vnode',
        vnode: m('div.compare-heatmap-pair', [
            slot(a, 'compare-baseline-dot'),
            slot(b, 'compare-experiment-dot'),
        ]),
    };
};

const sideBySideHeatmap = ({ spec, captures, anchors, toggles, chartsState, interval, Chart }) => {
    const chartId = spec?.opts?.id;
    const diffMode = !!(toggles && chartId && toggles[chartId] && toggles[chartId].diff);
    if (diffMode) {
        return renderDiffHeatmap({ spec, captures, anchors, chartsState, interval, Chart });
    }
    return sideBySidePair({ spec, captures, anchors, chartsState, interval, Chart });
};

// Unified (min, max) across both captures for the shared visualMap.
// Each capture's extract* stashed its own scanned min/max, so this is
// just Math.min/Math.max of the two pairs. Falls back to the spec's
// own bounds if neither capture had numeric samples.
const unifiedHeatmapRange = (a, b, spec) => {
    const lo = Math.min(
        a?.minValue != null ? a.minValue : Infinity,
        b?.minValue != null ? b.minValue : Infinity,
    );
    const hi = Math.max(
        a?.maxValue != null ? a.maxValue : -Infinity,
        b?.maxValue != null ? b.maxValue : -Infinity,
    );
    if (!Number.isFinite(lo) || !Number.isFinite(hi)) {
        return {
            min: spec.min_value != null ? spec.min_value : 0,
            max: spec.max_value != null ? spec.max_value : 1,
        };
    }
    return { min: lo, max: hi };
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
    if (!aMatrix || !bMatrix) return FALLBACK;

    const rows = Math.min(aMatrix.length, bMatrix.length);
    const bins = Math.min(
        (aMatrix[0] || []).length,
        (bMatrix[0] || []).length,
    );

    const triples = [];
    let dMin = Infinity;
    let dMax = -Infinity;
    for (let r = 0; r < rows; r++) {
        for (let c = 0; c < bins; c++) {
            const av = aMatrix[r][c];
            const bv = bMatrix[r][c];
            const d = nullDiff(bv, av); // experiment − baseline
            if (d != null) {
                if (d < dMin) dMin = d;
                if (d > dMax) dMax = d;
            }
            triples.push([c, r, d]);
        }
    }
    // Fallback when all cells were null.
    if (!Number.isFinite(dMin) || !Number.isFinite(dMax)) {
        dMin = -1;
        dMax = 1;
    } else if (dMin === dMax) {
        // Flat-zero or flat-nonzero data still needs a non-degenerate range.
        const pad = Math.max(Math.abs(dMin), 1) * 0.5;
        dMin -= pad;
        dMax += pad;
    }

    const timeData = (a.timeData || spec.time_data || []).slice(0, bins);
    const baselineAnchorSec = anchorSecondsFor(anchors, CAPTURE_BASELINE, timeData);

    // Theme is applied as `data-theme` on <html> (see theme.js); the
    // body-class probe this used to use was always false, which
    // silently pinned both nullCellColor and the diff palette to the
    // light-theme variant.
    const isDark = isDarkTheme();

    // Use the data's actual [min, max] rather than forcing a symmetric
    // band around 0. Resample the diverging palette so neutral still
    // lands on value=0 — one-sided ranges collapse to the relevant half
    // of the palette (blue-to-neutral or neutral-to-green) and mixed
    // ranges preserve neutral at zero's natural fraction.
    //
    // Theme-specific base palette: the light variant fades to
    // near-white at neutral, the dark variant fades to the dark card
    // bg. Either way near-zero cells visually blend into the canvas
    // while extremes stay saturated — without the per-stop alpha
    // dilution that muddied the extreme hues in earlier attempts.
    const basePalette = isDark ? DIVERGING_BLUE_GREEN_DARK : DIVERGING_BLUE_GREEN;
    const resampledPalette = resampleDivergingForRange(basePalette, dMin, dMax);

    const diffSpec = {
        ...spec,
        opts: { ...spec.opts, title: `${spec.opts.title} (experiment − baseline)` },
        time_data: rebase(timeData, baselineAnchorSec),
        data: triples,
        min_value: dMin,
        max_value: dMax,
        colormap: resampledPalette,
        nullCellColor: nullCellColor(isDark),
        // Directional caption under the gradient bar. The numeric min/max
        // labels still show the actual (experiment − baseline) extremes;
        // these labels make the directionality unambiguous at a glance.
        diffLegendLabels: { left: 'base is higher', right: 'exp is higher' },
        // Side-channel so heatmap.js's tooltip can show the original
        // baseline + experiment values for a hovered cell instead of the
        // computed delta. Indexed as matrix[row][bin].
        diffMatrices: { baseline: aMatrix, experiment: bMatrix },
        xAxisFormatter: relativeTimeFormatter,
    };

    return {
        kind: 'vnode',
        vnode: m('div.compare-heatmap-diff', m(Chart, { spec: diffSpec, chartsState, interval })),
    };
};

/**
 * Split a `multi` chart (e.g. per-CPU, per-cgroup line) into one overlay
 * line chart per shared label.
 */
const splitMultiToSubgroup = ({ spec, captures, anchors }) =>
    splitIntoOverlayLines({ spec, captures, anchors, labelFor: multiLabel });

/**
 * Split a `scatter` chart (histogram percentiles) into one overlay
 * scatter chart per shared quantile label. Percentile series are
 * naturally discrete samples, not continuous measurements — points
 * read more honestly than a connecting line suggests.
 */
const splitScatterToSubgroup = ({ spec, captures, anchors }) =>
    splitIntoOverlayLines({
        spec, captures, anchors, labelFor: percentileLabel, seriesType: 'scatter',
    });

const splitIntoOverlayLines = ({ spec, captures, anchors, labelFor: _labelFor, seriesType = 'line' }) => {
    const baseline = captures.find((c) => c.id === CAPTURE_BASELINE);
    const experiment = captures.find((c) => c.id === CAPTURE_EXPERIMENT);
    if (!baseline || !experiment) return FALLBACK;

    const mapA = baseline.seriesMap || new Map();
    const mapB = experiment.seriesMap || new Map();
    const labelsA = new Set(mapA.keys());
    const labelsB = new Set(mapB.keys());
    const shared = [...intersectLabels(labelsA, labelsB)].sort();
    const asScatter = seriesType === 'scatter';

    const specs = shared.map((label) => {
        const a = mapA.get(label);
        const b = mapB.get(label);
        const baseSec = anchorSecondsFor(anchors, CAPTURE_BASELINE, a.timeData);
        const expSec = anchorSecondsFor(anchors, CAPTURE_EXPERIMENT, b.timeData);
        return {
            ...spec,
            opts: {
                ...spec.opts,
                id: `${spec.opts.id || 'chart'}::${label}`,
                title: `${spec.opts.title} — ${label}`,
                style: 'line',
            },
            // Bare sub-chart label (e.g. "p50") for the caller to render
            // as a small header above this sub-chart. The full opts.title
            // stays around for tooltip/fallback use.
            _splitLabel: label,
            multiSeries: [
                {
                    name: CAPTURE_BASELINE,
                    color: BASELINE_COLOR,
                    timeData: rebase(a.timeData, baseSec),
                    valueData: a.valueData,
                    fill: false,
                    scatter: asScatter,
                },
                {
                    name: CAPTURE_EXPERIMENT,
                    color: EXPERIMENT_COLOR,
                    timeData: rebase(b.timeData, expSec),
                    valueData: b.valueData,
                    fill: false,
                    scatter: asScatter,
                },
            ],
            xAxisFormatter: relativeTimeFormatter,
        };
    });

    return { kind: 'split', specs };
};

const multiLabel = (r) => {
    const mm = r.metric || {};
    return Object.keys(mm).sort().filter((k) => k !== '__name__')
        .map((k) => `${k}=${mm[k]}`).join(',');
};

const percentileLabel = (r) => canonicalQuantileLabel(r) || 'unknown';

/**
 * Render baseline and experiment histogram heatmaps side-by-side. No
 * diff variant — a meaningful diff would need a per-bucket log-scale
 * divergence metric that's out of scope for this iteration.
 */
const sideBySideHistogramHeatmap = (opts) => sideBySidePair({
    ...opts,
    styleOverride: 'histogram_heatmap',
    extraSlotFields: (cap) => ({
        bucket_bounds: cap.bucketBounds || opts.spec.bucket_bounds,
    }),
});

