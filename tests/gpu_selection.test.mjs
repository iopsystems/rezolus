// The GPU selector filters charts by rewriting each query's label matchers.
// A GPU is (vendor, id), not id alone: every vendor's sampler numbers its own
// devices from 0, so a host with an NVIDIA card and an Intel iGPU has two
// different GPUs both labelled id="0". Getting this wrong does not error — it
// silently plots the wrong device's data, which is why it is tested here.
import test from 'node:test';
import assert from 'node:assert/strict';
import { applyGpuSelection, promqlResultToHeatmapTriples } from '../src/viewer/assets/lib/data.js';

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

// Per-GPU charts group by (id, vendor) so two GPUs sharing an id draw as two
// lines. They must stay exempt from the selector's filter, which is keyed on a
// regex over that grouping — a regex that only matched the older `by (id)`
// form would silently start filtering them.
test('per-GPU groupings are exempt from the selection filter', () => {
    const re = /by\s*\(\s*id\s*[,)]/;
    assert.ok(re.test('sum by (id) (gpu_utilization) / 100'));
    assert.ok(re.test('sum by (id, vendor) (gpu_utilization) / 100'));
    assert.ok(re.test('max by (id, vendor) (gpu_temperature)'));
    assert.ok(re.test('sum by ( id , vendor ) (gpu_clock)'));
    // Aggregate charts are not exempt; they are what the selector filters.
    assert.ok(!re.test('avg(gpu_utilization) / 100'));
    assert.ok(!re.test('sum(gpu_memory{state="used"})'));
    assert.ok(!re.test('sum by (vendor) (gpu_utilization)'));
});

// Per-GPU labels always carry the vendor. An id is only meaningful within a
// vendor — every vendor's sampler numbers its devices from 0 — so "nvidia 0"
// identifies a GPU where "0" merely indexes one.
//
// These exercise the SHIPPED `promqlResultToHeatmapTriples`, not a copy of it.
// An earlier version of this file re-implemented the labelling locally, which
// let a regression through: the real function returned labels for CPU-shaped
// results too ("0", "1", "2"), and the renderer read that as "these rows name
// themselves" and dropped the "CPU" y-axis title on every host. A local copy
// cannot catch that; importing does.
const series = (vendor, id) => ({ metric: { vendor, id }, values: [[0, '1']] });
const rowsOf = (results) => promqlResultToHeatmapTriples(results).rowLabels;

test('rows carry the vendor on a single-vendor host too', () => {
    assert.deepEqual(rowsOf([series('intel', '0'), series('intel', '1')]),
        ['intel 0', 'intel 1']);
});

test('two GPUs sharing an id get distinct rows', () => {
    // Rows were indexed by parseInt(id), so two GPUs both at id="0" landed on
    // the same row, one silently overwriting the other.
    const rows = rowsOf([series('nvidia', '0'), series('intel', '0')]);
    assert.equal(rows.length, 2, 'both GPUs must occupy their own row');
    assert.deepEqual(rows, ['nvidia 0', 'intel 0']);
});

test('a sparse id set packs its rows instead of leaving a gap', () => {
    // VRAM is discrete-only, so a host whose discrete GPU is id=1 yields only
    // that series; indexing by id drew a blank row 0 above the real one.
    assert.deepEqual(rowsOf([series('intel', '1')]), ['intel 1']);
});

test('a CPU-shaped result yields no labels, so the axis keeps its title', () => {
    // The regression this file previously missed. Labels equal to their own row
    // index tell the renderer nothing; handing them over made it drop the "CPU"
    // y-axis title and render tooltips as "2" instead of "CPU 2".
    const cpu = [{ metric: { id: '0' }, values: [[0, '1']] },
                 { metric: { id: '1' }, values: [[0, '2']] },
                 { metric: { id: '2' }, values: [[0, '3']] }];
    assert.equal(rowsOf(cpu), null);
});

test('a series with no id at all is left to the renderer', () => {
    assert.equal(rowsOf([{ metric: {}, values: [[0, '1']] }]), null);
});
