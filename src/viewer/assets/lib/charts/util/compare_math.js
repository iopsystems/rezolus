// Pure comparison math. No DOM, no echarts, no mithril. Safe to run in Node.
// Null-propagation is the universal rule: if either operand is null,
// undefined, or NaN, the result is null.

const isMissing = (v) => v === null || v === undefined || Number.isNaN(v);

export const toRelative = (series, anchorMs) => ({
    ...series,
    timestamps: series.timestamps.map((t) => t - anchorMs),
    values: series.values.slice(),
});

export const nullDiff = (a, b) => {
    if (isMissing(a) || isMissing(b)) return null;
    return a - b;
};

export const intersectLabels = (setA, setB) => {
    const out = new Set();
    for (const x of setA) if (setB.has(x)) out.add(x);
    return out;
};

export const longerDuration = (a, b) => (a > b ? a : b);
