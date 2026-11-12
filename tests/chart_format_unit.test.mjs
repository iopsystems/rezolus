// The single-chart view's unit selector and the unit a series is stored in
// (`format.unit`, from the metric catalog or a KPI's stored unit).
import { test } from 'node:test';
import assert from 'node:assert/strict';

// The components touch the DOM at import; a vnode-returning `m` and a few
// stubs are enough to render them in node. These globals are safe only
// because `node --test` runs each file in its own process.
const isVnode = (x) => x != null && typeof x === 'object' && 'tag' in x;
globalThis.m = (tag, ...rest) => {
    const attrs = rest.length && rest[0] != null && typeof rest[0] === 'object'
        && !Array.isArray(rest[0]) && !isVnode(rest[0]) ? rest.shift() : {};
    return { tag, attrs, children: rest.length === 1 ? rest[0] : rest };
};
globalThis.m.route = Object.assign(() => {}, { get: () => '/cpu@h2/chart/busy', set() {}, prefix: '#!' });
globalThis.m.redraw = () => {};
globalThis.requestAnimationFrame = () => 0;
globalThis.window = {
    addEventListener() {}, removeEventListener() {}, location: { search: '', hash: '', pathname: '/' },
    history: { replaceState() {} }, matchMedia: () => ({ matches: false, addEventListener() {} }),
};
globalThis.document = {
    documentElement: { dataset: {}, getAttribute() {}, setAttribute() {} },
    addEventListener() {}, body: { appendChild() {} }, getElementById: () => null, querySelector: () => null,
    createElement: () => ({ style: {}, classList: { add() {}, remove() {} }, appendChild() {}, setAttribute() {} }),
    querySelectorAll: () => [],
};
globalThis.getComputedStyle = () => ({ getPropertyValue: () => '' });

const { chartFormat } = await import('../src/viewer/assets/lib/features/explorers.js');

test("the chart's own unit system keeps its whole format", () => {
    const format = { unit_system: 'time', unit: 'seconds', precision: 3 };
    assert.equal(chartFormat(format, 'time'), format);
    const unitOnly = { unit: 'seconds' };
    assert.equal(chartFormat(unitOnly, ''), unitOnly);
});

test('another unit system keeps the stored unit', () => {
    // A metric-browser chart of a `_seconds` series has no unit system; picking
    // `time` must still read the values as seconds.
    assert.deepEqual(chartFormat({ unit: 'seconds' }, 'time'), {
        unit_system: 'time', precision: 2, unit: 'seconds',
    });
    assert.deepEqual(chartFormat({ unit_system: 'count' }, 'bytes'), {
        unit_system: 'bytes', precision: 2,
    });
});

test("a stored unit drops the override's range, which would apply to stored values", () => {
    // A gauge stored in `percent` (0 to 100): `percentage`'s 0..1 range would
    // cut 50% at 1%.
    assert.deepEqual(chartFormat({ unit: 'percent' }, 'percentage'), {
        unit_system: 'percentage', precision: 2, unit: 'percent',
    });
    // Without a stored unit the range stays.
    assert.deepEqual(chartFormat(undefined, 'percentage'), {
        unit_system: 'percentage', precision: 2, range: { min: 0, max: 1 },
    });
});

test('no selection and no format draws with none', () => {
    assert.equal(chartFormat(undefined, ''), undefined);
});
