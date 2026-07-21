// Regression: in compare mode, a plot that also went through single-capture
// display mode carries a top-level `spec.boxplot` (the decimated bands) that
// runQuery attached. line.js / scatter.js PRIORITIZE that field over the
// compare `multiSeries` / split data, so if the compare adapter leaks it
// through, the chart renders only the baseline's single-capture bands and the
// experiment overlay vanishes (gauges) or the bands mix into the split lines
// (percentiles). The adapter must strip `boxplot` / `boxplotDecimated` from
// every line/scatter compare spec. The per-capture envelope rides inside
// `multiSeries[].boxplot`, so the overlay itself is unaffected.

import test from 'node:test';
import assert from 'node:assert/strict';

// compare.js -> colormap.js reads CSS custom properties at module-load time, so
// stub the browser globals before importing (dynamic import runs after this).
globalThis.getComputedStyle = () => ({ getPropertyValue: () => '' });
if (typeof globalThis.document === 'undefined') {
    globalThis.document = { documentElement: {}, body: {} };
}

const { renderCompareChart } = await import('../src/viewer/assets/lib/charts/compare.js');
const { CAPTURE_BASELINE, CAPTURE_EXPERIMENT } = await import('../src/viewer/assets/lib/data.js');

const bp = (median) => ({
    t: [1, 2, 3],
    min: [1, 1, 1],
    lo: [2, 2, 2],
    median: [median, median, median],
    hi: [8, 8, 8],
    max: [9, 9, 9],
});

const labels = { [CAPTURE_BASELINE]: 'baseline', [CAPTURE_EXPERIMENT]: 'experiment' };
const anchors = { [CAPTURE_BASELINE]: 0, [CAPTURE_EXPERIMENT]: 0 };

test('overlayLine (gauge): strips single-capture boxplot, keeps both captures', () => {
    const spec = {
        opts: { style: 'line', id: 'g', title: 'gauge' },
        // The single-capture display bands runQuery attached — the poison pill.
        boxplot: [bp(100)],
        boxplotDecimated: true,
    };
    const captures = [
        { id: CAPTURE_BASELINE, timeData: [1, 2, 3], valueData: [100, 100, 100], boxplot: bp(100) },
        { id: CAPTURE_EXPERIMENT, timeData: [1, 2, 3], valueData: [160, 160, 160], boxplot: bp(160) },
    ];
    const result = renderCompareChart({ spec, captures, anchors, captureLabels: labels });

    assert.equal(result.kind, 'spec');
    // The bug: without stripping, line.js renders spec.boxplot (baseline-only).
    assert.equal(result.spec.boxplot, undefined, 'top-level boxplot stripped');
    assert.equal(result.spec.boxplotDecimated, undefined, 'boxplotDecimated stripped');
    // Both captures survive as overlay series, each with its own envelope.
    assert.equal(result.spec.multiSeries.length, 2, 'baseline + experiment');
    assert.deepEqual(result.spec.multiSeries.map((s) => s.name), ['baseline', 'experiment']);
    assert.ok(result.spec.multiSeries.every((s) => s.boxplot), 'per-capture envelope preserved');
});

test('overlayLine: anchors each envelope on its OWN boxplot grid (aligned relative time)', () => {
    const grid = (t0) => ({
        t: [t0, t0 + 5, t0 + 10],
        min: [1, 1, 1], lo: [2, 2, 2], median: [5, 5, 5], hi: [8, 8, 8], max: [9, 9, 9],
    });
    const spec = { opts: { style: 'line', id: 'g', title: 'gauge' }, boxplot: [grid(2)] };
    const captures = [
        // baseline: matrix and decimated boxplot both start at t=2 (consistent)
        { id: CAPTURE_BASELINE, timeData: [2, 7, 12], valueData: [5, 5, 5], boxplot: grid(2) },
        // experiment: raw matrix starts at t=1, but its decimated boxplot starts at t=2
        { id: CAPTURE_EXPERIMENT, timeData: [1, 2, 3], valueData: [5, 5, 5], boxplot: grid(2) },
    ];
    const result = renderCompareChart({ spec, captures, anchors, captureLabels: labels });
    const [b, e] = result.spec.multiSeries;
    // Both envelopes must rebase to the SAME relative origin. The bug anchored the
    // experiment on its matrix (t0=1) while drawing its boxplot grid (t0=2),
    // shifting it 1s off the baseline.
    assert.equal(e.timeData[0], 0, 'experiment envelope starts at relative 0');
    assert.equal(b.timeData[0], e.timeData[0], 'baseline and experiment share relative t0');
    assert.deepEqual(Array.from(e.timeData), Array.from(b.timeData), 'grids coincide');
});

test('overlayLine: emits a divergence band from the two aligned medians', () => {
    const grid = (med) => ({
        t: [2, 7], min: [1, 1], lo: [2, 2], median: [med, med], hi: [8, 8], max: [9, 9],
    });
    const spec = { opts: { style: 'line', id: 'g', title: 'gauge' }, boxplot: [grid(100)] };
    const captures = [
        { id: CAPTURE_BASELINE, timeData: [2, 7], valueData: [100, 100], boxplot: grid(100) },
        { id: CAPTURE_EXPERIMENT, timeData: [2, 7], valueData: [160, 160], boxplot: grid(160) },
    ];
    const band = renderCompareChart({ spec, captures, anchors, captureLabels: labels }).spec.divergenceBand;
    assert.ok(band, 'divergence band present');
    assert.deepEqual(band.lower, [100, 100]);
    assert.deepEqual(band.upper, [160, 160]);
});

test('overlayLine: no divergence band when the grids do not coincide', () => {
    const grid = (t0, med) => ({
        t: [t0, t0 + 5], min: [1, 1], lo: [2, 2], median: [med, med], hi: [8, 8], max: [9, 9],
    });
    const spec = { opts: { style: 'line', id: 'g', title: 'gauge' }, boxplot: [grid(2, 100)] };
    // Different bucket SPACING (5s vs 6s) → the rebased grids diverge after t0,
    // so even the anchor-aligned rebase can't make them coincide.
    const captures = [
        { id: CAPTURE_BASELINE, timeData: [2, 7], valueData: [100, 100], boxplot: grid(2, 100) },
        {
            id: CAPTURE_EXPERIMENT, timeData: [3, 9], valueData: [160, 160],
            boxplot: { t: [3, 9], min: [1, 1], lo: [2, 2], median: [160, 160], hi: [8, 8], max: [9, 9] },
        },
    ];
    const band = renderCompareChart({ spec, captures, anchors, captureLabels: labels }).spec.divergenceBand;
    assert.equal(band, null, 'mismatched x-grid → no band (never fill across different times)');
});

test('splitScatter (percentiles): strips single-capture boxplot from every split spec', () => {
    const seriesMap = () => new Map([
        ['p50', { timeData: [1, 2, 3], valueData: [1, 1, 1] }],
    ]);
    const spec = {
        opts: { style: 'scatter', id: 'h', title: 'latency' },
        boxplot: [bp(1)],
        boxplotDecimated: true,
    };
    const captures = [
        { id: CAPTURE_BASELINE, seriesMap: seriesMap() },
        { id: CAPTURE_EXPERIMENT, seriesMap: seriesMap() },
    ];
    const result = renderCompareChart({ spec, captures, anchors, captureLabels: labels });

    assert.equal(result.kind, 'split');
    assert.ok(result.specs.length >= 1, 'at least one shared quantile');
    for (const s of result.specs) {
        assert.equal(s.boxplot, undefined, 'split spec has no single-capture bands');
        assert.equal(s.boxplotDecimated, undefined);
        assert.equal(s.multiSeries.length, 2, 'baseline + experiment points');
        assert.ok(s.divergenceBand, 'each split quantile carries a divergence band');
    }
});
