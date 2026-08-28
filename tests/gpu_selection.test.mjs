// The GPU selector filters charts by rewriting each query's label matchers.
// A GPU is (vendor, id), not id alone: every vendor's sampler numbers its own
// devices from 0, so a host with an NVIDIA card and an Intel iGPU has two
// different GPUs both labelled id="0". Getting this wrong does not error — it
// silently plots the wrong device's data, which is why it is tested here.
import test from 'node:test';
import assert from 'node:assert/strict';
import { applyGpuSelection } from '../src/viewer/assets/lib/data.js';

const Q = 'sum(gpu_memory{state="used"})';

test('a single GPU pins both vendor and id', () => {
    const q = applyGpuSelection(Q, [{ vendor: 'intel', id: '0' }]);
    assert.match(q, /vendor="intel"/);
    assert.match(q, /id="0"/);
});

test('same id under two vendors does not collapse to one id matcher', () => {
    // The bug this guards: injecting id="0" alone matches the NVIDIA card AND
    // the Intel iGPU, so selecting one plots the sum of both.
    const nvidia = applyGpuSelection(Q, [{ vendor: 'nvidia', id: '0' }]);
    const intel = applyGpuSelection(Q, [{ vendor: 'intel', id: '0' }]);
    assert.notEqual(nvidia, intel);
    assert.match(nvidia, /vendor="nvidia"/);
    assert.match(intel, /vendor="intel"/);
});

test('several GPUs of one vendor collapse to a regex over ids', () => {
    const q = applyGpuSelection(Q, [
        { vendor: 'nvidia', id: '0' },
        { vendor: 'nvidia', id: '1' },
    ]);
    assert.match(q, /vendor="nvidia"/);
    assert.match(q, /id=~"0\|1"/);
});

test('a selection with no vendor recorded filters on id alone', () => {
    // The Apple GPU sampler declares no vendor. Such a recording must keep
    // working exactly as it did before vendors were considered.
    const one = applyGpuSelection(Q, [{ vendor: null, id: '0' }]);
    assert.match(one, /id="0"/);
    assert.doesNotMatch(one, /vendor=/);

    const two = applyGpuSelection(Q, [{ vendor: null, id: '0' }, { vendor: null, id: '1' }]);
    assert.match(two, /id=~"0\|1"/);
    assert.doesNotMatch(two, /vendor=/);
});

test('a full cross product across vendors constrains both labels', () => {
    // Every selected vendor present at every selected id: the cross product is
    // exactly the selection, so both matchers are safe.
    const q = applyGpuSelection(Q, [
        { vendor: 'nvidia', id: '0' },
        { vendor: 'intel', id: '0' },
    ]);
    assert.match(q, /vendor=~"(intel\|nvidia|nvidia\|intel)"/);
    assert.match(q, /id="0"/);
});

test('a partial cross product does not claim a vendor constraint it cannot express', () => {
    // nvidia:0 and intel:1 only. Emitting vendor=~"nvidia|intel", id=~"0|1"
    // would also match nvidia:1 and intel:0 — GPUs the user did not select.
    // PromQL has no OR over matchers, so the vendor constraint is dropped
    // rather than stated wrongly; the id matcher over-matches, which is the
    // pre-existing behaviour and never drops selected data.
    const q = applyGpuSelection(Q, [
        { vendor: 'nvidia', id: '0' },
        { vendor: 'intel', id: '1' },
    ]);
    assert.match(q, /id=~"0\|1"/);
    assert.doesNotMatch(q, /vendor=~/);
});
