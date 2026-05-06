import test from 'node:test';
import assert from 'node:assert/strict';
import {
    nullDiff,
    intersectLabels,
    unifyHistogramRange,
    buildDeltaSpectrum,
} from '../src/viewer/assets/lib/charts/util/compare_math.js';

test('nullDiff: numbers', () => {
    assert.equal(nullDiff(5, 3), 2);
    assert.equal(nullDiff(0, 0), 0);
    assert.equal(nullDiff(-1, 1), -2);
});

test('nullDiff: null propagates from either side', () => {
    assert.equal(nullDiff(null, 3), null);
    assert.equal(nullDiff(5, null), null);
    assert.equal(nullDiff(null, null), null);
});

test('nullDiff: undefined treated same as null', () => {
    assert.equal(nullDiff(undefined, 3), null);
    assert.equal(nullDiff(5, undefined), null);
});

test('nullDiff: NaN treated as null', () => {
    assert.equal(nullDiff(Number.NaN, 3), null);
    assert.equal(nullDiff(5, Number.NaN), null);
});

test('intersectLabels: common subset', () => {
    const a = new Set(['a', 'b', 'c']);
    const b = new Set(['b', 'c', 'd']);
    assert.deepEqual([...intersectLabels(a, b)].sort(), ['b', 'c']);
});

test('intersectLabels: disjoint sets yield empty', () => {
    assert.deepEqual([...intersectLabels(new Set(['x']), new Set(['y']))], []);
});

const fakeSpectrum = (cols) => ({
    data: [[/* times unused */ 0, 1, 2], ...cols],
});

test('unifyHistogramRange: anchors win when present, else natural min', () => {
    const a = { ...fakeSpectrum([[1, 2, 3], [4, 5, 6]]), color_min_anchor: 0.5 };
    const b = { ...fakeSpectrum([[2, 3, 4], [5, 6, 7]]), color_min_anchor: 0.7 };
    const r = unifyHistogramRange(a, b);
    assert.equal(r.colorMin, 0.5);  // min(0.5, 0.7)
    assert.equal(r.colorMax, 7);    // max(6, 7)
});

test('unifyHistogramRange: missing anchor falls back to scanned min', () => {
    const a = { ...fakeSpectrum([[2, 3, 4]]), color_min_anchor: null };
    const b = { ...fakeSpectrum([[1, 5, 6]]), color_min_anchor: null };
    const r = unifyHistogramRange(a, b);
    assert.equal(r.colorMin, 1);
    assert.equal(r.colorMax, 6);
});

test('unifyHistogramRange: skips null/NaN/non-positive cells in scan', () => {
    const a = { ...fakeSpectrum([[null, 0, -1, 5]]), color_min_anchor: null };
    const b = { ...fakeSpectrum([[NaN, 2, null, 8]]), color_min_anchor: null };
    const r = unifyHistogramRange(a, b);
    assert.equal(r.colorMin, 2);
    assert.equal(r.colorMax, 8);
});

test('unifyHistogramRange: empty data falls back to (0, 1)', () => {
    const a = { data: [[]], color_min_anchor: null };
    const b = { data: [[]], color_min_anchor: null };
    const r = unifyHistogramRange(a, b);
    assert.equal(r.colorMin, 0);
    assert.equal(r.colorMax, 1);
});

test('unifyHistogramRange: collapsed range gets a non-zero ceiling', () => {
    const a = { ...fakeSpectrum([[5, 5, 5]]), color_min_anchor: 5 };
    const b = { ...fakeSpectrum([[5, 5, 5]]), color_min_anchor: 5 };
    const r = unifyHistogramRange(a, b);
    assert.equal(r.colorMin, 5);
    assert.ok(r.colorMax > r.colorMin);  // padded so log-scale doesn't collapse
});

test('unifyHistogramRange: asymmetric anchors — capture without anchor still pulls colorMin via its own scanned min', () => {
    const a = { ...fakeSpectrum([[10, 20, 30]]), color_min_anchor: 10 };
    const b = { ...fakeSpectrum([[0.1, 5, 8]]), color_min_anchor: null };
    const r = unifyHistogramRange(a, b);
    // B's natural min (0.1) is lower than A's anchor (10); colorMin
    // must be 0.1 so B's bottom cells aren't clipped on the shared scale.
    assert.equal(r.colorMin, 0.1);
    assert.equal(r.colorMax, 30);
});

test('unifyHistogramRange: asymmetric anchors, reversed — anchor on B, scan on A', () => {
    const a = { ...fakeSpectrum([[2, 5, 9]]), color_min_anchor: null };
    const b = { ...fakeSpectrum([[100, 200, 300]]), color_min_anchor: 100 };
    const r = unifyHistogramRange(a, b);
    assert.equal(r.colorMin, 2);   // A's natural min
    assert.equal(r.colorMax, 300); // B's natural max
});

const spectrum = (times, qSeries, names) => ({
    time_data: times,
    data: [times, ...qSeries],
    series_names: names,
});

test('buildDeltaSpectrum: per-cell experiment − baseline', () => {
    const baseline = spectrum([0, 1], [[1, 2], [3, 4]], ['p50', 'p99']);
    const experiment = spectrum([0, 1], [[2, 5], [4, 7]], ['p50', 'p99']);
    const r = buildDeltaSpectrum(baseline, experiment);
    assert.deepEqual(r.data[0], [0, 1]);
    assert.deepEqual(r.data[1], [1, 3]);   // p50 deltas: 2−1, 5−2
    assert.deepEqual(r.data[2], [1, 3]);   // p99 deltas: 4−3, 7−4
    assert.deepEqual(r.series_names, ['p50', 'p99']);
});

test('buildDeltaSpectrum: dMin/dMax over non-null deltas', () => {
    const baseline = spectrum([0, 1], [[1, 5]], ['p50']);
    const experiment = spectrum([0, 1], [[2, 3]], ['p50']);
    const r = buildDeltaSpectrum(baseline, experiment);
    assert.equal(r.dMin, -2);  // 3 − 5
    assert.equal(r.dMax, 1);   // 2 − 1
});

test('buildDeltaSpectrum: null on either side propagates', () => {
    const baseline = spectrum([0, 1], [[null, 2]], ['p50']);
    const experiment = spectrum([0, 1], [[5, null]], ['p50']);
    const r = buildDeltaSpectrum(baseline, experiment);
    assert.deepEqual(r.data[1], [null, null]);
    assert.equal(r.dMin, null);
    assert.equal(r.dMax, null);
});

test('buildDeltaSpectrum: returns matrices keyed by qIdx then tIdx for tooltip lookup', () => {
    const baseline = spectrum([0, 1], [[1, 2], [3, 4]], ['p50', 'p99']);
    const experiment = spectrum([0, 1], [[2, 5], [4, 7]], ['p50', 'p99']);
    const r = buildDeltaSpectrum(baseline, experiment);
    assert.equal(r.matrices.baseline[0][1], 2);    // p50 at t=1 → 2
    assert.equal(r.matrices.experiment[1][0], 4);  // p99 at t=0 → 4
});

test('buildDeltaSpectrum: time/series mismatch returns null', () => {
    const baseline = spectrum([0, 1], [[1, 2]], ['p50']);
    const experiment = spectrum([0], [[5]], ['p50']);
    const r = buildDeltaSpectrum(baseline, experiment);
    assert.equal(r, null);
});

test('buildDeltaSpectrum: empty inputs return null', () => {
    const r = buildDeltaSpectrum({ data: [[]] }, { data: [[]] });
    assert.equal(r, null);
});
