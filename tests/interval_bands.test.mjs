import test from 'node:test';
import assert from 'node:assert/strict';
import { parseIntervals } from '../src/viewer/assets/lib/data.js';

// parseIntervals turns a series' optional `intervals` field (the NEW,
// OPTIONAL rate() uncertainty bounds parallel to `values`) into a clean
// [[lo, hi], …] array, or null when absent/unusable. It must parse
// defensively so older responses and non-rate queries (no `intervals`)
// keep rendering exactly as before.

test('absent intervals → null (non-rate / older responses)', () => {
    assert.equal(parseIntervals({ values: [[1, 2]] }), null);
    assert.equal(parseIntervals({}), null);
    assert.equal(parseIntervals(null), null);
    assert.equal(parseIntervals(undefined), null);
});

test('empty or non-array intervals → null', () => {
    assert.equal(parseIntervals({ intervals: [] }), null);
    assert.equal(parseIntervals({ intervals: 'nope' }), null);
    assert.equal(parseIntervals({ intervals: {} }), null);
});

test('well-formed bounds parsed to numbers, parallel to values', () => {
    const out = parseIntervals({
        intervals: [[5.99, 6.01], [2.99, 3.0], [1.0, 1.0]],
    });
    assert.deepEqual(out, [[5.99, 6.01], [2.99, 3.0], [1.0, 1.0]]);
});

test('string bounds are coerced to numbers', () => {
    assert.deepEqual(parseIntervals({ intervals: [['1.5', '2.5']] }), [[1.5, 2.5]]);
});

test('reversed pairs are normalized to lo ≤ hi', () => {
    assert.deepEqual(parseIntervals({ intervals: [[9, 3]] }), [[3, 9]]);
});

test('malformed pairs become null but keep positional alignment', () => {
    const out = parseIntervals({
        intervals: [[1, 2], null, [3], ['x', 4], [5, 6]],
    });
    assert.deepEqual(out, [[1, 2], null, null, null, [5, 6]]);
});

test('all-malformed intervals → null (nothing usable)', () => {
    assert.equal(parseIntervals({ intervals: [null, [NaN, 1], ['a', 'b']] }), null);
});
