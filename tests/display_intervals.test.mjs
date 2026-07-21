// The decimated display path carries an aggregated measurement-uncertainty band
// as two parallel columns (uncLo/uncHi, NaN = no band at that point).
// displayIntervals turns those into the same `[[lo,hi]|null, …]` shape the matrix
// path's parseIntervals produces, so buildBandSeries renders both paths
// identically. These pin the NaN→gap, out-of-order-swap, and empty-band cases.
import test from 'node:test';
import assert from 'node:assert/strict';
import { displayIntervals } from '../src/viewer/assets/lib/data.js';

test('builds [lo,hi] pairs parallel to the columns', () => {
    const s = { uncLo: Float64Array.from([1.8, 2.8, 3.8]), uncHi: Float64Array.from([2.2, 3.2, 4.2]) };
    assert.deepEqual(displayIntervals(s), [[1.8, 2.2], [2.8, 3.2], [3.8, 4.2]]);
});

test('NaN edges become a per-point gap (null), not a pair', () => {
    const s = { uncLo: Float64Array.from([1, NaN, 3]), uncHi: Float64Array.from([2, NaN, 4]) };
    assert.deepEqual(displayIntervals(s), [[1, 2], null, [3, 4]]);
});

test('an all-NaN band yields null (nothing to draw)', () => {
    const s = { uncLo: Float64Array.from([NaN, NaN]), uncHi: Float64Array.from([NaN, NaN]) };
    assert.equal(displayIntervals(s), null);
});

test('a reversed edge is normalized to [lo,hi]', () => {
    const s = { uncLo: Float64Array.from([5]), uncHi: Float64Array.from([3]) };
    assert.deepEqual(displayIntervals(s), [[3, 5]]);
});

test('a series with no band columns yields null', () => {
    assert.equal(displayIntervals({}), null);
    assert.equal(displayIntervals(null), null);
});
