import test from 'node:test';
import assert from 'node:assert/strict';

// scatter.js pulls in colormap.js, which reads CSS custom properties at
// module-load. Stub the DOM globals it touches BEFORE importing (not jsdom —
// a 2-line shim), so the palette falls back to its literals. Static ESM
// imports are hoisted, so scatter.js is imported dynamically below.
globalThis.document = globalThis.document || { documentElement: {} };
globalThis.getComputedStyle = globalThis.getComputedStyle || (() => ({ getPropertyValue: () => '' }));
const { buildPercentileBandSeries } = await import('../src/viewer/assets/lib/charts/scatter.js');

// Follow-on to the round-3 viewer-band work: percentile charts render as a
// scatter (scatter.js), which previously drew no measurement-uncertainty band.
// buildPercentileBandSeries turns each percentile's `intervals` (bucket-
// resolution `[lo,hi]`, parallel to its values) into a translucent band series
// behind the dots. These tests pin that the bands are built when intervals are
// present and skipped when they aren't.

const TIME = [100, 101, 102]; // seconds
const COLORS = ['#111', '#222', '#333'];
const LABELS = ['p50', 'p90', 'p99'];

test('builds a band per percentile that carries intervals', () => {
    const seriesIntervals = [
        [[9, 11], [10, 12], [11, 13]],   // p50
        [[90, 110], [100, 120], [110, 130]], // p90
    ];
    const out = buildPercentileBandSeries(seriesIntervals, TIME, LABELS, COLORS, null, 1);
    // buildBandSeries emits 2 stacked series (lo baseline + hi-lo delta) per band.
    assert.equal(out.length, 4, 'two band series per percentile');
    // The delta series carries the translucent fill.
    const fills = out.filter((s) => s.areaStyle && s.areaStyle.opacity > 0);
    assert.equal(fills.length, 2, 'one filled delta per percentile');
    assert.equal(fills[0].areaStyle.color, '#111', 'band uses the percentile color');
});

test('skips percentiles with no usable intervals', () => {
    const seriesIntervals = [
        [[9, 11], [10, 12], [11, 13]], // p50 has bands
        null,                          // p90 has none (non-rate/degenerate)
    ];
    const out = buildPercentileBandSeries(seriesIntervals, TIME, LABELS, COLORS, null, 1);
    assert.equal(out.length, 2, 'only p50 contributes a band');
});

test('no series_intervals → no bands', () => {
    assert.deepEqual(buildPercentileBandSeries(null, TIME, LABELS, COLORS, null, 1), []);
    assert.deepEqual(buildPercentileBandSeries([], TIME, LABELS, COLORS, null, 1), []);
});
