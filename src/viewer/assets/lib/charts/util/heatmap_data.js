// Scan a flat [timeIdx, y, value] triple array for the numeric value
// bounds. Returns { min: null, max: null } when there are no numeric
// samples.
const heatmapTriplesMinMax = (triples) => {
    if (!Array.isArray(triples) || triples.length === 0) return { min: null, max: null };
    let lo = Infinity;
    let hi = -Infinity;
    for (const t of triples) {
        const v = Array.isArray(t) ? t[2] : null;
        if (v == null || Number.isNaN(v)) continue;
        if (v < lo) lo = v;
        if (v > hi) hi = v;
    }
    if (!Number.isFinite(lo) || !Number.isFinite(hi)) return { min: null, max: null };
    return { min: lo, max: hi };
};

// Build a rows × bins matrix from a flat [timeIdx, y, value] triple
// array. Gaps fill with null.
const heatmapTriplesToMatrix = (triples, binCount) => {
    if (!Array.isArray(triples) || triples.length === 0) return [];
    let maxY = -1;
    for (const t of triples) {
        const y = Number(t?.[1]);
        if (Number.isFinite(y) && y > maxY) maxY = y;
    }
    if (maxY < 0) return [];
    const rows = maxY + 1;
    const cols = Math.max(1, binCount || 0);
    const matrix = Array.from({ length: rows }, () =>
        new Array(cols).fill(null));
    for (const [ti, y, v] of triples) {
        const r = Number(y);
        const c = Number(ti);
        if (!Number.isFinite(r) || !Number.isFinite(c)) continue;
        if (r < 0 || r >= rows || c < 0 || c >= cols) continue;
        matrix[r][c] = (v === null || v === undefined) ? null : Number(v);
    }
    return matrix;
};

// Compare-mode diff heatmaps need a dense rows × bins matrix for O(1)
// cell lookups, but side-by-side mode only needs the original triples.
// Materialize the matrix lazily and memoize it on the capture object.
const ensureHeatmapMatrix = (capture) => {
    if (!capture || typeof capture !== 'object') return [];
    if (Array.isArray(capture.heatmapMatrix)) return capture.heatmapMatrix;
    const matrix = heatmapTriplesToMatrix(
        capture.heatmapData || [],
        capture.timeData?.length || 0,
    );
    capture.heatmapMatrix = matrix;
    return matrix;
};

// Build a sparse row-major index from triples so mounted heatmap charts
// can materialize only the currently displayed resolution instead of
// retaining both full-res and downsampled flattened arrays.
const createHeatmapResolutionStore = (triples, timeData, emitNullCells) => {
    const cpuIds = new Set();
    let maxY = -1;
    for (const t of triples || []) {
        const y = Number(t?.[1]);
        if (!Number.isFinite(y) || y < 0) continue;
        cpuIds.add(y);
        if (y > maxY) maxY = y;
    }

    const yCount = maxY + 1;
    const rowMaps = Array.from({ length: Math.max(0, yCount) }, () => new Map());
    for (const [timeIndex, yRaw, value] of triples || []) {
        const y = Number(yRaw);
        const ti = Number(timeIndex);
        if (!Number.isFinite(y) || !Number.isFinite(ti)) continue;
        if (y < 0 || y >= rowMaps.length || ti < 0) continue;
        rowMaps[y].set(ti, value === null || value === undefined ? null : Number(value));
    }

    return {
        timeData: timeData || [],
        emitNullCells: !!emitNullCells,
        yCount: Math.max(0, yCount),
        cpuIds: Array.from(cpuIds).sort((a, b) => a - b),
        rowMaps,
        current: null,
    };
};

const materializeHeatmapResolution = (store, factor) => {
    const resolvedFactor = Math.max(1, factor | 0);
    const { timeData, emitNullCells, yCount, rowMaps } = store;
    const xCount = timeData.length;

    if (resolvedFactor === 1) {
        const out = [];
        for (let t = 0; t < xCount; t++) {
            for (let y = 0; y < yCount; y++) {
                const row = rowMaps[y];
                const v = row.has(t) ? row.get(t) : null;
                if (v !== null) {
                    out.push([timeData[t] * 1000, y, t, null, v]);
                } else if (emitNullCells) {
                    out.push([timeData[t] * 1000, y, t, null, null]);
                }
            }
        }
        return { factor: resolvedFactor, data: out };
    }

    const binCount = Math.ceil(xCount / resolvedFactor);
    const aggregatedRows = rowMaps.map((row) => {
        const bins = new Map();
        row.forEach((value, timeIndex) => {
            if (value === null || value === undefined) return;
            const bin = Math.floor(timeIndex / resolvedFactor);
            const current = bins.get(bin);
            if (!current) {
                bins.set(bin, [value, value]);
            } else {
                if (value < current[0]) current[0] = value;
                if (value > current[1]) current[1] = value;
            }
        });
        return bins;
    });

    const out = [];
    for (let bin = 0; bin < binCount; bin++) {
        const timeIndex = bin * resolvedFactor;
        const timeValue = timeData[timeIndex];
        if (timeValue == null) continue;
        for (let y = 0; y < yCount; y++) {
            const minMax = aggregatedRows[y].get(bin);
            if (minMax) {
                out.push([timeValue * 1000, y, timeIndex, minMax[0], minMax[1]]);
            } else if (emitNullCells) {
                out.push([timeValue * 1000, y, timeIndex, null, null]);
            }
        }
    }
    return { factor: resolvedFactor, data: out };
};

const ensureHeatmapResolution = (store, factor) => {
    const resolvedFactor = Math.max(1, factor | 0);
    if (store.current && store.current.factor === resolvedFactor) {
        return store.current;
    }
    const resolution = materializeHeatmapResolution(store, resolvedFactor);
    store.current = resolution;
    return resolution;
};

export {
    heatmapTriplesMinMax,
    heatmapTriplesToMatrix,
    ensureHeatmapMatrix,
    createHeatmapResolutionStore,
    ensureHeatmapResolution,
};
