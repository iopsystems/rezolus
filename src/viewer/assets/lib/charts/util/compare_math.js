// Pure comparison math. No DOM, no echarts, no mithril. Safe to run in Node.
// Null-propagation is the universal rule: if either operand is null,
// undefined, or NaN, the result is null.

const isMissing = (v) => v === null || v === undefined || Number.isNaN(v);

export const nullDiff = (a, b) => {
    if (isMissing(a) || isMissing(b)) return null;
    return a - b;
};

export const intersectLabels = (setA, setB) => {
    const out = new Set();
    for (const x of setA) if (setB.has(x)) out.add(x);
    return out;
};

/**
 * Unify the color scale across two spectrum captures so a side-by-side
 * pair renders with the same [colorMin, colorMax] — the same color
 * always means the same value.
 *
 * `colorMin` prefers each capture's `color_min_anchor` (the p0 anchor
 * computed by fetchQuantileSpectrumForPlot); when missing, falls back
 * to the natural min scanned over each capture's positive cells.
 * `colorMax` is the natural max scanned over both captures.
 *
 * Each capture must have shape `{ data: [times, q1Series, q2Series, …],
 * color_min_anchor: number | null }`. Returns `{ colorMin, colorMax }`.
 */
export function unifyHistogramRange(a, b) {
    const aScan = scanPositiveRange(a?.data);
    const bScan = scanPositiveRange(b?.data);

    // Per-capture effective min: anchor if present, otherwise the
    // capture's own scanned positive min. Each capture stays
    // independent so an asymmetric anchor situation (one present,
    // one absent) doesn't clip the no-anchor capture's lower values.
    const aMin = positiveOr(a?.color_min_anchor, Number.isFinite(aScan.min) ? aScan.min : Infinity);
    const bMin = positiveOr(b?.color_min_anchor, Number.isFinite(bScan.min) ? bScan.min : Infinity);
    let colorMin = Math.min(aMin, bMin);
    if (!Number.isFinite(colorMin)) colorMin = 0;

    const naturalMax = Math.max(
        Number.isFinite(aScan.max) ? aScan.max : -Infinity,
        Number.isFinite(bScan.max) ? bScan.max : -Infinity,
    );

    let colorMax = naturalMax;
    if (!Number.isFinite(colorMax)) colorMax = 1;

    if (colorMax <= colorMin) {
        colorMax = colorMin > 0 ? colorMin * 10 : 1;
    }

    return { colorMin, colorMax };
}

// data shape: [timeCol, q1Col, q2Col, …]. Skips column 0 (times) and
// only counts strictly positive, finite cells (matches the renderer's
// own log-scale skip rule in quantile_heatmap.js).
function scanPositiveRange(data) {
    let min = Infinity;
    let max = -Infinity;
    if (!Array.isArray(data) || data.length < 2) return { min, max };
    for (let s = 1; s < data.length; s++) {
        const col = data[s];
        if (!Array.isArray(col)) continue;
        for (const v of col) {
            if (v != null && !Number.isNaN(v) && v > 0) {
                if (v < min) min = v;
                if (v > max) max = v;
            }
        }
    }
    return { min, max };
}

function positiveOr(v, fallback) {
    return (v != null && Number.isFinite(v) && v > 0) ? v : fallback;
}

/**
 * Build a columnar delta spectrum (experiment − baseline) from two
 * spectrum-shaped captures. Returns null when the time axes or quantile
 * counts don't match (the renderer needs aligned axes for cross-cell
 * lookup).
 *
 * Shape of each input:
 *   { time_data: number[], data: [time, ...qCols], series_names: string[] }
 *
 * Return shape (or null on mismatch / empty inputs):
 *   {
 *     time_data: number[],         // alias of baseline.time_data (shared reference)
 *     data: [time, ...qDeltaCols], // null preserved when either side is null/NaN
 *     series_names: string[],      // taken from baseline
 *     dMin: number | null,         // null if all deltas null; can be 0 (flat)
 *     dMax: number | null,         //   — caller must pad when dMin === dMax
 *     matrices: {
 *       baseline:   number[][],    // [qIdx][tIdx] for tooltip lookup
 *       experiment: number[][],    // same
 *     },
 *   }
 *
 * Uses nullDiff so undefined/NaN propagate cleanly.
 *
 * Caller responsibilities:
 *   - Time axes must be VALUE-aligned, not just length-matched. This
 *     function only verifies length; matching cadence is the upstream
 *     fetch's job.
 *   - When dMin === dMax (flat captures), pad before passing to a
 *     diverging palette renderer (see renderDiffHeatmap in compare.js
 *     for the canonical pad recipe).
 *   - Do not mutate the returned `time_data` / `data[0]` in place;
 *     they alias baseline.time_data.
 */
export function buildDeltaSpectrum(baseline, experiment) {
    const baseTimes = baseline?.time_data;
    const expTimes = experiment?.time_data;
    if (!Array.isArray(baseTimes) || !Array.isArray(expTimes)) return null;
    if (baseTimes.length === 0 || baseTimes.length !== expTimes.length) return null;

    const baseData = baseline.data;
    const expData = experiment.data;
    if (!Array.isArray(baseData) || baseData.length < 2) return null;
    if (!Array.isArray(expData) || expData.length !== baseData.length) return null;

    const qCount = baseData.length - 1;
    const tCount = baseTimes.length;

    const deltaCols = [];
    const baseMatrix = [];
    const expMatrix = [];
    let dMin = Infinity;
    let dMax = -Infinity;

    for (let q = 0; q < qCount; q++) {
        const baseCol = baseData[q + 1];
        const expCol = expData[q + 1];
        const deltaCol = new Array(tCount);
        const baseRow = new Array(tCount);
        const expRow = new Array(tCount);
        for (let t = 0; t < tCount; t++) {
            const bv = baseCol?.[t];
            const ev = expCol?.[t];
            const d = nullDiff(ev, bv);
            deltaCol[t] = d;
            baseRow[t] = (bv != null && !Number.isNaN(bv)) ? bv : null;
            expRow[t]  = (ev != null && !Number.isNaN(ev)) ? ev : null;
            if (d != null) {
                if (d < dMin) dMin = d;
                if (d > dMax) dMax = d;
            }
        }
        deltaCols.push(deltaCol);
        baseMatrix.push(baseRow);
        expMatrix.push(expRow);
    }

    return {
        time_data: baseTimes,
        data: [baseTimes, ...deltaCols],
        series_names: baseline.series_names || [],
        dMin: Number.isFinite(dMin) ? dMin : null,
        dMax: Number.isFinite(dMax) ? dMax : null,
        matrices: { baseline: baseMatrix, experiment: expMatrix },
    };
}

// Canonicalize a histogram quantile into the shared "pXX" label form.
// Accepts either a raw value (number or string like "0.5", "50", "p99")
// or an object with .metric carrying a `quantile` label (the standard
// PromQL convention emitted by both `histogram_quantile` and the
// rezolus-extension `histogram_quantiles`), and returns either a
// canonical label or null when it can't be parsed. Either fraction
// (<=1) or percent form is accepted.
export const canonicalQuantileLabel = (input) => {
    let raw = input;
    if (input && typeof input === 'object') {
        const mm = input.metric || input;
        raw = mm.quantile;
        if (raw == null) {
            for (const [k, v] of Object.entries(mm)) {
                if (k !== '__name__') { raw = v; break; }
            }
        }
    }
    if (typeof raw === 'string') {
        const m = raw.match(/^p?(\d+(?:\.\d+)?)$/);
        if (m) raw = m[1];
    }
    const q = Number(raw);
    if (!Number.isFinite(q)) return null;
    const pct = q <= 1 ? q * 100 : q;
    return `p${pct.toFixed(2).replace(/\.?0+$/, '')}`;
};
