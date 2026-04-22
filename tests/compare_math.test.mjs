import test from 'node:test';
import assert from 'node:assert/strict';
import {
    toRelative,
    nullDiff,
    intersectLabels,
    longerDuration,
} from '../src/viewer/assets/lib/charts/util/compare_math.js';

test('toRelative: identity when anchor is zero', () => {
    const s = { timestamps: [0, 1000, 2000], values: [1, 2, 3] };
    const r = toRelative(s, 0);
    assert.deepEqual(r.timestamps, [0, 1000, 2000]);
    assert.deepEqual(r.values, [1, 2, 3]);
});

test('toRelative: subtracts anchor', () => {
    const s = { timestamps: [1000, 1500, 2500], values: [1, 2, 3] };
    const r = toRelative(s, 1000);
    assert.deepEqual(r.timestamps, [0, 500, 1500]);
});

test('toRelative: preserves null values', () => {
    const s = { timestamps: [0, 1000, 2000], values: [1, null, 3] };
    const r = toRelative(s, 0);
    assert.deepEqual(r.values, [1, null, 3]);
});

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

test('longerDuration: returns max', () => {
    assert.equal(longerDuration(100, 200), 200);
    assert.equal(longerDuration(200, 100), 200);
    assert.equal(longerDuration(0, 0), 0);
});
