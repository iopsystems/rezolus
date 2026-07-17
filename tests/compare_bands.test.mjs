import test from 'node:test';
import assert from 'node:assert/strict';
import { promqlResultToLinePair, applyResultToPlot, CAPTURE_BASELINE, CAPTURE_EXPERIMENT } from '../src/viewer/assets/lib/data.js';

// compare.js pulls in colormap.js, which reads CSS custom properties at
// module-load. Stub the two DOM globals it touches (not jsdom — a 2-line
// shim) BEFORE importing, so the palette falls back to its literals. Static
// ESM imports are hoisted, so compare.js is imported dynamically below.
globalThis.document = globalThis.document || { documentElement: {} };
globalThis.getComputedStyle = globalThis.getComputedStyle || (() => ({ getPropertyValue: () => '' }));
const { renderCompareChart } = await import('../src/viewer/assets/lib/charts/compare.js');

// Round 3 of the measurement-uncertainty arc: rate()/histogram value bands
// must render on COMPARE (A/B overlay) and MULTI-series charts, not just
// single-series. The data layer previously dropped `intervals` for every
// non-single-series render; these tests pin the compare-line path carrying a
// per-capture band all the way into line.js's `multiSeries` entries (which
// `buildBandSeries` already knows how to draw).

test('promqlResultToLinePair extracts intervals when present', () => {
    const results = [{ values: [[1, '10'], [2, '20']], intervals: [[9, 11], [19, 21]] }];
    const pair = promqlResultToLinePair(results);
    assert.deepEqual(pair.intervals, [[9, 11], [19, 21]]);
});

test('promqlResultToLinePair intervals null when absent (non-rate)', () => {
    const pair = promqlResultToLinePair([{ values: [[1, '10'], [2, '20']] }]);
    assert.equal(pair.intervals, null);
});

test('compare-line overlay carries each capture band into multiSeries', () => {
    const baseline = {
        id: CAPTURE_BASELINE,
        timeData: [100, 101, 102],
        valueData: [10, 20, 30],
        intervals: [[9, 11], [19, 21], [29, 31]],
    };
    const experiment = {
        id: CAPTURE_EXPERIMENT,
        timeData: [200, 201, 202],
        valueData: [12, 22, 32],
        intervals: [[11, 13], [21, 23], [31, 33]],
    };
    const out = renderCompareChart({
        spec: { opts: { style: 'line' } },
        captures: [baseline, experiment],
        anchors: {},
        captureLabels: {},
    });
    assert.equal(out.kind, 'spec');
    const ms = out.spec.multiSeries;
    assert.equal(ms.length, 2);
    // Bands ride along per capture, parallel to valueData (time-independent).
    assert.deepEqual(ms[0].intervals, baseline.intervals);
    assert.deepEqual(ms[1].intervals, experiment.intervals);
});

// Multi-series data path: each series' optional band is carried on
// `plot.series_intervals`, parallel to `series_names` (multi.js renders them
// only for percentile charts). Absent bands → null entries, positional.
test('multi-series result carries per-series intervals parallel to names', () => {
    const plot = { opts: { style: 'multi' } };
    const result = {
        status: 'success',
        data: {
            result: [
                { metric: { quantile: '0.5' }, values: [[1, '10'], [2, '11']], intervals: [[9, 10], [10, 11]] },
                { metric: { quantile: '0.9' }, values: [[1, '20'], [2, '21']], intervals: [[19, 20], [20, 21]] },
            ],
        },
    };
    applyResultToPlot(plot, result);
    assert.equal(plot.series_names.length, 2);
    assert.deepEqual(plot.series_intervals[0], [[9, 10], [10, 11]]);
    assert.deepEqual(plot.series_intervals[1], [[19, 20], [20, 21]]);
});

test('multi-series without bands → series_intervals of nulls', () => {
    const plot = { opts: { style: 'multi' } };
    const result = {
        status: 'success',
        data: {
            result: [
                { metric: { cpu: '0' }, values: [[1, '10']] },
                { metric: { cpu: '1' }, values: [[1, '20']] },
            ],
        },
    };
    applyResultToPlot(plot, result);
    assert.deepEqual(plot.series_intervals, [null, null]);
});

test('compare-line overlay omits intervals when captures have none', () => {
    const mk = (id, t0) => ({
        id,
        timeData: [t0, t0 + 1, t0 + 2],
        valueData: [1, 2, 3],
    });
    const out = renderCompareChart({
        spec: { opts: { style: 'line' } },
        captures: [mk(CAPTURE_BASELINE, 100), mk(CAPTURE_EXPERIMENT, 200)],
        anchors: {},
        captureLabels: {},
    });
    assert.equal(out.kind, 'spec');
    for (const s of out.spec.multiSeries) {
        assert.ok(s.intervals == null, 'no band when capture carries none');
    }
});
