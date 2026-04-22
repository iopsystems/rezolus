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

// Canonicalize a histogram quantile into the shared "pXX" label form.
// Accepts either a raw value (number or string like "0.5", "50", "p99")
// or an object with .metric carrying percentile/quantile labels, and
// returns either a canonical label or null when it can't be parsed.
// metriken_query emits `percentile` as a fraction; legacy sources may
// use `quantile`; either fraction (<=1) or percent form is accepted.
export const canonicalQuantileLabel = (input) => {
    let raw = input;
    if (input && typeof input === 'object') {
        const mm = input.metric || input;
        raw = mm.percentile != null ? mm.percentile : mm.quantile;
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
