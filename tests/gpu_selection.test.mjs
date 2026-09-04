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
// identifies a GPU where "0" merely indexes one. Showing it unconditionally
// also keeps a chart's labels stable when the set of GPUs in a result changes.
const series = (vendor, id) => ({ metric: { vendor, id } });

const seriesName = (item) => {
    const { id, vendor } = item.metric;
    if (id === undefined || vendor === undefined) return null;
    return `${vendor} ${id}`;
};

const heatmapRowLabels = (results) => {
    const ids = results.map((i) => i.metric && i.metric.id);
    const nums = ids.map((v) => parseInt(v, 10));
    const usable = nums.every((v) => !Number.isNaN(v))
        && new Set(nums).size === nums.length
        && Math.max(...nums) === nums.length - 1;
    const out = [];
    results.forEach((item, idx) => {
        const m = item.metric || {};
        const row = usable ? nums[idx] : idx;
        out[row] = m.id == null ? String(row) : (m.vendor ? `${m.vendor} ${m.id}` : String(m.id));
    });
    return out;
};

test('series names carry the vendor on a single-vendor host too', () => {
    assert.equal(seriesName(series('intel', '1')), 'intel 1');
    assert.equal(seriesName(series('amd', '0')), 'amd 0');
});

test('heatmap rows carry the vendor on a single-vendor host too', () => {
    // The reported case: sky is Intel-only, and its rows read "intel 0"/"intel 1"
    // rather than a bare "0"/"1".
    assert.deepEqual(heatmapRowLabels([series('intel', '0'), series('intel', '1')]),
        ['intel 0', 'intel 1']);
});

test('heatmap rows: two GPUs sharing an id get distinct rows', () => {
    // Rows were indexed by parseInt(id), so two GPUs both at id="0" landed on
    // the same row, one silently overwriting the other.
    const rows = heatmapRowLabels([series('nvidia', '0'), series('intel', '0')]);
    assert.equal(rows.length, 2, 'both GPUs must occupy their own row');
    assert.deepEqual(rows, ['nvidia 0', 'intel 0']);
});

test('a series with no vendor label falls back rather than throwing', () => {
    // The macOS GPU sampler sets no vendor on any of its metrics.
    assert.equal(seriesName({ metric: { id: '0' } }), null);
    assert.deepEqual(heatmapRowLabels([{ metric: { id: '0' } }]), ['0']);
});

test('a single GPU still yields one labelled row', () => {
    assert.deepEqual(heatmapRowLabels([series('intel', '0')]), ['intel 0']);
});

test('a sparse id set packs its rows instead of leaving a gap', () => {
    // sky publishes VRAM for its discrete GPU (id=1) but not its integrated
    // one, so indexing rows by id drew a blank row 0 above the only real row.
    assert.deepEqual(heatmapRowLabels([series('intel', '1')]), ['intel 1']);
    assert.deepEqual(heatmapRowLabels([series('amd', '2')]), ['amd 2']);
});
