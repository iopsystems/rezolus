import test from 'node:test';
import assert from 'node:assert/strict';
import { resampleLinear, familyBand, familyBandLabel } from '../src/viewer/assets/lib/charts/util/family_math.js';

test('resampleLinear interpolates inside the member range and is null outside or across a hole', () => {
    const t = [0, 1, 2, 3];
    const v = [0, 10, null, 30];
    assert.deepEqual(resampleLinear([0, 0.5, 1, 1.5, 2.5, 3, 4, -1], t, v), [0, 5, 10, null, null, 30, null, null]);
    assert.deepEqual(resampleLinear([0, 1], [], []), [null, null]);
    assert.deepEqual(resampleLinear([0, 1], [0], [7]), [7, null]);
});

test('a sigma band is mean ± k·sd over the members present at each bucket', () => {
    const band = familyBand([
        { t: [0, 1, 2], v: [10, 10, 10] },
        { t: [0, 1, 2], v: [12, 14, 10] },
        { t: [0, 1], v: [14, 18] },
    ], { kind: 'sigma', k: 1 });
    assert.deepEqual(band.t, [0, 1, 2]);
    assert.deepEqual(band.n, [3, 3, 2]);
    assert.equal(band.mean[0], 12);
    // sd of [10,12,14] = 2
    assert.equal(band.lower[0], 10);
    assert.equal(band.upper[0], 14);
    // sd of [10,14,18] = 4, mean 14
    assert.equal(band.lower[1], 10);
    assert.equal(band.upper[1], 18);
    // Two members at t=2: still a band (n >= 2).
    assert.equal(band.n[2], 2);
    assert.equal(band.mean[2], 10);
    assert.equal(band.lower[2], 10);
    assert.equal(band.members, 3);
    assert.equal(familyBandLabel(band), 'family mean ± 1σ (3 members)');
});

test('a single-member bucket has a mean but no sigma band; an envelope still has one', () => {
    const members = [{ t: [0, 1], v: [1, 2] }, { t: [0], v: [3] }];
    const s = familyBand(members, { kind: 'sigma', k: 2 });
    assert.equal(s.n[1], 1);
    assert.equal(s.mean[1], 2);
    assert.equal(s.lower[1], null);
    const e = familyBand(members, { kind: 'envelope' });
    assert.deepEqual([e.lower[1], e.upper[1]], [2, 2]);
    assert.deepEqual([e.lower[0], e.upper[0]], [1, 3]);
    assert.equal(familyBandLabel(e), 'family min..max (2 members)');
});

test('members on different cadences are resampled onto the first member grid', () => {
    const band = familyBand([
        { t: [0, 1, 2, 3, 4], v: [0, 1, 2, 3, 4] },
        { t: [0, 2, 4], v: [0, 2, 4] },
    ], { kind: 'envelope' });
    // The coarse member interpolates to the same line: zero-width envelope.
    assert.deepEqual(band.lower, [0, 1, 2, 3, 4]);
    assert.deepEqual(band.upper, [0, 1, 2, 3, 4]);
});

test('exact matches at the next sample and an unsorted grid resolve', () => {
    const t = [10, 20, 30];
    const v = [1, 2, 3];
    assert.deepEqual(resampleLinear([10, 20, 30], t, v), [1, 2, 3]);
    assert.deepEqual(resampleLinear([30, 10, 25, 5], t, v), [3, 1, 2.5, null]);
});

test('a first member shorter than the others sets the grid', () => {
    const band = familyBand([{ t: [0, 1], v: [1, 1] }, { t: [0, 1, 2, 3], v: [2, 2, 2, 2] }, { t: [0, 1, 2], v: [3, 3, 3] }], { kind: 'envelope' });
    assert.deepEqual(band.t, [0, 1]);
    assert.deepEqual(band.n, [3, 3]);
    assert.deepEqual(band.upper, [3, 3]);
});

test('fewer than two usable members is no band', () => {
    assert.equal(familyBand([{ t: [0], v: [1] }]), null);
    assert.equal(familyBand([{ t: [0], v: [1] }, { t: [0, 1], v: [1] }]), null);
    assert.equal(familyBand(null), null);
    assert.equal(familyBandLabel(null), '');
});

test('a bucket outside every other member is mean-only with n=1', () => {
    const band = familyBand([{ t: [0, 1, 2], v: [1, 1, 1] }, { t: [0, 1], v: [3, 3] }], { kind: 'sigma' });
    assert.deepEqual(band.n, [2, 2, 1]);
    assert.equal(band.lower[2], null);
    assert.equal(band.mean[2], 1);
});
