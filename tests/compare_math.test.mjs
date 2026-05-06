import test from 'node:test';
import assert from 'node:assert/strict';
import {
    nullDiff,
    intersectLabels,
    unifyHistogramRange,
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
