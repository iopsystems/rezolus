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

    const naturalMin = Math.min(
        Number.isFinite(aScan.min) ? aScan.min : Infinity,
        Number.isFinite(bScan.min) ? bScan.min : Infinity,
    );
    const naturalMax = Math.max(
        Number.isFinite(aScan.max) ? aScan.max : -Infinity,
        Number.isFinite(bScan.max) ? bScan.max : -Infinity,
    );

    let colorMin = Math.min(
        positiveOr(a?.color_min_anchor, Infinity),
        positiveOr(b?.color_min_anchor, Infinity),
    );
    if (!Number.isFinite(colorMin)) colorMin = naturalMin;
    if (!Number.isFinite(colorMin)) colorMin = 0;

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
