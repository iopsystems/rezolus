import { test } from 'node:test';
import assert from 'node:assert/strict';
import { insertGapNulls } from '../src/viewer/assets/lib/charts/util/utils.js';

// multi.js pulls in colormap.js, which reads CSS custom properties at module
// load; the same two-line DOM shim compare_bands.test.mjs uses, then a
// dynamic import so it runs after the shim.
globalThis.document = globalThis.document || { documentElement: {} };
globalThis.getComputedStyle = globalThis.getComputedStyle || (() => ({ getPropertyValue: () => '' }));
const { multiLineGeometry } = await import('../src/viewer/assets/lib/charts/multi.js');

// #1516: a decimated multi-series chart holds one point per display bucket
// (5s on a ~10 minute live view of a 1s recording). Judging gaps against the
// 1s interval inserted a null after every bucket, and `step: 'start'` drew
// each point as a 1s dash followed by a 4s gap.

const nulls = (pts) => pts.filter(([, v]) => v === null).length;
const zip = (ts) => ts.map((t, i) => [t * 1000, i]);

test('decimated buckets wider than the interval are not gaps', () => {
    const ts = Array.from({ length: 20 }, (_, i) => 1_700_000_000 + i * 5);
    const g = multiLineGeometry(ts, 1, true);
    assert.equal(g.gapInterval, 5);
    assert.equal(g.step, false);
    assert.equal(nulls(insertGapNulls(zip(ts), g.gapInterval)), 0);
});

test('a missing bucket in a decimated response is still a gap', () => {
    const ts = Array.from({ length: 20 }, (_, i) => 1_700_000_000 + i * 5).filter((_, i) => i !== 7);
    const g = multiLineGeometry(ts, 1, true);
    assert.equal(g.gapInterval, 5);
    assert.equal(nulls(insertGapNulls(zip(ts), g.gapInterval)), 1);
});

test('native resolution keeps the interval threshold and stepped lines', () => {
    const ts = Array.from({ length: 20 }, (_, i) => 1_700_000_000 + i);
    const g = multiLineGeometry(ts, 1, false);
    assert.deepEqual(g, { gapInterval: 1, step: 'start' });
    const holed = ts.filter((_, i) => i !== 4);
    assert.equal(nulls(insertGapNulls(zip(holed), g.gapInterval)), 1);
});

test('a short bucket at the edge of the range does not set the spacing', () => {
    const ts = Array.from({ length: 20 }, (_, i) => 1_700_000_000 + i * 10);
    ts.push(ts[ts.length - 1] + 1);
    const g = multiLineGeometry(ts, 1, true);
    assert.equal(g.gapInterval, 10);
    assert.equal(nulls(insertGapNulls(zip(ts), g.gapInterval)), 0);
});

test('a decimated response of one point falls back to the interval', () => {
    assert.deepEqual(multiLineGeometry([1_700_000_000], 1, true), { gapInterval: 1, step: false });
});
