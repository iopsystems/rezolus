import test from 'node:test';
import assert from 'node:assert/strict';
import { quantilesForKind } from '../src/viewer/assets/lib/charts/util/spectrum_quantiles.js';

test('quantilesForKind: full produces 100 quantiles from 0.01 to 1.00', () => {
    const qs = quantilesForKind('full');
    assert.equal(qs.length, 100);
    assert.equal(qs[0], 0.01);
    assert.equal(qs[99], 1.0);
});

test('quantilesForKind: tail produces 100 quantiles from 0.9901 to 1.0000', () => {
    const qs = quantilesForKind('tail');
    assert.equal(qs.length, 100);
    assert.equal(qs[0], 0.9901);
    assert.equal(qs[99], 1.0);
});

test('quantilesForKind: full quantiles are uniform 0.01 apart', () => {
    const qs = quantilesForKind('full');
    for (let i = 1; i < qs.length; i++) {
        // Float-tolerant equality at 1e-9; quantilesForKind uses integer math,
        // so the produced values are exact divisions of 100.
        assert.ok(Math.abs((qs[i] - qs[i - 1]) - 0.01) < 1e-9);
    }
});

test('quantilesForKind: tail quantiles are uniform 0.0001 apart', () => {
    const qs = quantilesForKind('tail');
    for (let i = 1; i < qs.length; i++) {
        assert.ok(Math.abs((qs[i] - qs[i - 1]) - 0.0001) < 1e-9);
    }
});

test('quantilesForKind: unknown kind defaults to full', () => {
    assert.deepEqual(quantilesForKind('unknown'), quantilesForKind('full'));
    assert.deepEqual(quantilesForKind(null), quantilesForKind('full'));
});
