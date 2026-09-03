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

// Series naming for per-GPU charts. The vendor is shown only when the result
// spans vendors: on a single-vendor host "intel GPU 0" is noise where "GPU 0"
// says the same thing, but with two vendors both numbering from 0 the vendor
// is the only thing telling the lines apart.
//
// Mirrors the logic in data.js; the vendor-spanning case cannot be produced by
// any host available here (it needs a discrete Intel GPU alongside another
// vendor publishing a shared metric name), so it is covered here instead.
const nameSeries = (results) => {
    const vendors = new Set(results.map((i) => i.metric && i.metric.vendor).filter(Boolean));
    const qualify = vendors.size > 1;
    return results.map((i) => {
        const { id, vendor } = i.metric;
        if (id !== undefined && vendor !== undefined) {
            return qualify ? `${vendor} GPU ${id}` : `GPU ${id}`;
        }
        return null;
    });
};

const series = (vendor, id) => ({ metric: { vendor, id } });

test('one vendor: series are named by id alone', () => {
    assert.deepEqual(nameSeries([series('amd', '0'), series('amd', '1')]),
        ['GPU 0', 'GPU 1']);
});

test('two vendors sharing an id: the vendor disambiguates', () => {
    assert.deepEqual(nameSeries([series('nvidia', '0'), series('intel', '0')]),
        ['nvidia GPU 0', 'intel GPU 0']);
});

test('a series with no vendor label falls back rather than throwing', () => {
    // The macOS GPU sampler sets no vendor on any of its metrics.
    assert.deepEqual(nameSeries([{ metric: { id: '0' } }]), [null]);
});

// Heatmap row layout. Rows were indexed by parseInt(id), so two GPUs sharing an
// id — an NVIDIA card and an Intel iGPU, both id="0" — landed on the same row,
// one silently overwriting the other. Rows now fall back to result order when
// the ids do not uniquely identify the series, and each row carries a label.
const heatmapRowLabels = (results) => {
    const ids = results.map((i) => i.metric && i.metric.id);
    const nums = ids.map((v) => parseInt(v, 10));
    const usable = nums.every((v) => !Number.isNaN(v)) && new Set(nums).size === nums.length;
    const vendors = new Set(results.map((i) => i.metric && i.metric.vendor).filter(Boolean));
    const qualify = vendors.size > 1;
    const out = [];
    results.forEach((item, idx) => {
        const m = item.metric || {};
        const row = usable ? nums[idx] : idx;
        out[row] = m.id == null
            ? String(row)
            : (qualify && m.vendor ? `${m.vendor} ${m.id}` : String(m.id));
    });
    return out;
};

test('heatmap rows: one vendor keeps bare ids', () => {
    assert.deepEqual(heatmapRowLabels([series('amd', '0'), series('amd', '1')]), ['0', '1']);
});

test('heatmap rows: two GPUs sharing an id get distinct rows, vendor-qualified', () => {
    const rows = heatmapRowLabels([series('nvidia', '0'), series('intel', '0')]);
    assert.equal(rows.length, 2, 'both GPUs must occupy their own row');
    assert.deepEqual(rows, ['nvidia 0', 'intel 0']);
});

test('heatmap rows: a single GPU still yields one labelled row', () => {
    // A per-entity chart is a heatmap even with one entity, so the one-row case
    // must label correctly rather than fall through to the index.
    assert.deepEqual(heatmapRowLabels([series('intel', '0')]), ['0']);
});
