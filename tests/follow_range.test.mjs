import test from 'node:test';
import assert from 'node:assert/strict';
import { clampRangeToExtent } from '../src/viewer/assets/lib/data.js';

// A followed hindsight buffer evicts its oldest rows, so the recording's
// start moves forward between refreshes. A committed zoom window is cut to
// the new start, or dropped when none of it is left.

test('clampRangeToExtent: no override stays no override', () => {
    assert.equal(clampRangeToExtent(null, { start: 10, end: 20 }), null);
});

test('clampRangeToExtent: a window inside the extent is returned unchanged', () => {
    const range = { start: 12, end: 18 };
    assert.equal(clampRangeToExtent(range, { start: 10, end: 20 }), range);
});

test('clampRangeToExtent: a window past the end is kept, since the end only grows', () => {
    const range = { start: 12, end: 30 };
    assert.equal(clampRangeToExtent(range, { start: 10, end: 20 }), range);
});

test('clampRangeToExtent: a window that starts before the extent is cut to its start', () => {
    assert.deepEqual(
        clampRangeToExtent({ start: 5, end: 15 }, { start: 10, end: 20 }),
        { start: 10, end: 15 },
    );
});

test('clampRangeToExtent: a window wholly before the extent is dropped', () => {
    assert.equal(clampRangeToExtent({ start: 2, end: 8 }, { start: 10, end: 20 }), null);
    assert.equal(clampRangeToExtent({ start: 2, end: 10 }, { start: 10, end: 20 }), null);
});

test('clampRangeToExtent: an unknown extent leaves the window alone', () => {
    const range = { start: 2, end: 8 };
    assert.equal(clampRangeToExtent(range, null), range);
    assert.equal(clampRangeToExtent(range, { start: NaN, end: NaN }), range);
});
