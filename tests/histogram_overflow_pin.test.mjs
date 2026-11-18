// A chart pinned to the Notebook keeps its own format. A percentile chart
// showing a +Inf quantile has a range.max that markOverflow added for its
// out-of-bounds band; pinned with it, the stale max would persist in
// localStorage, a selection export and a saved report, and clamp later
// finite values above it.
import { test } from 'node:test';
import assert from 'node:assert/strict';

// selection.js touches the DOM at import; a vnode-returning `m` and a few
// stubs are enough. Safe because `node --test` runs each file in its own
// process.
const isVnode = (x) => x != null && typeof x === 'object' && 'tag' in x;
globalThis.m = (tag, ...rest) => {
    const attrs = rest.length && rest[0] != null && typeof rest[0] === 'object'
        && !Array.isArray(rest[0]) && !isVnode(rest[0]) ? rest.shift() : {};
    return { tag, attrs, children: rest.length === 1 ? rest[0] : rest };
};
globalThis.m.route = Object.assign(() => {}, { get: () => '/', set() {}, prefix: '#' });
globalThis.m.request = async () => ({ status: 'success', data: [] });
globalThis.m.redraw = () => {};
globalThis.m.trust = (s) => s;
globalThis.requestAnimationFrame = () => 0;
globalThis.window = {
    addEventListener() {}, removeEventListener() {}, location: { search: '', hash: '', pathname: '/' },
    history: { replaceState() {} }, matchMedia: () => ({ matches: false, addEventListener() {} }),
};
globalThis.document = {
    documentElement: { dataset: {}, getAttribute() {}, setAttribute() {} },
    addEventListener() {}, body: { appendChild() {} }, getElementById: () => null, querySelector: () => null,
    createElement: () => ({
        style: {}, classList: { add() {}, remove() {} }, appendChild() {}, remove() {},
        setAttribute() {}, addEventListener() {}, querySelector: () => null,
    }),
    querySelectorAll: () => [],
};
const storage = new Map();
globalThis.localStorage = {
    getItem: (k) => storage.get(k) ?? null, setItem: (k, v) => storage.set(k, v), removeItem: (k) => storage.delete(k),
};
globalThis.sessionStorage = globalThis.localStorage;
globalThis.fetch = async () => ({ ok: false, status: 404, json: async () => ({}), text: async () => '' });
globalThis.getComputedStyle = () => ({ getPropertyValue: () => '' });

const { applyResultToPlot } = await import('../src/viewer/assets/lib/data.js');
const { toggleSelection, setStorageScope, notebookStore, buildPayload } =
    await import('../src/viewer/assets/lib/selection/selection.js');

const result = (p99) => ({
    status: 'success',
    data: {
        resultType: 'matrix',
        result: [
            { metric: { quantile: '0.5' }, values: [[1, '1'], [2, '1']] },
            { metric: { quantile: '0.99' }, values: [[1, '5'], [2, p99]] },
        ],
    },
});

test('a pinned chart keeps its own format, not the +Inf range', () => {
    const scope = { filename: 'overflow.dendro', minTime: 1, maxTime: 2, numSeries: 2 };
    setStorageScope(scope);
    const own = { unit_system: 'time' };
    const spec = {
        promql_query: 'latency',
        opts: { id: 'h', title: 'h', type: 'histogram', subtype: 'percentiles', format: own },
    };
    applyResultToPlot(spec, result('+Inf'));
    assert.equal(spec.opts.format.range.max, 5, 'the chart on screen has its band');

    assert.equal(toggleSelection(spec, 'latency', 'Latency', 'g'), true);
    assert.deepEqual(notebookStore.entries[0].chartOpts.format, own);
    assert.deepEqual(buildPayload(notebookStore, {}).entries[0].chartOpts.format, own);

    // Reload: the notebook comes back from localStorage and its chart is
    // drawn as chartLoaderMixin draws it, here over a finite p99 of 9.
    setStorageScope(scope);
    assert.equal(notebookStore.entries.length, 1);
    const reloaded = { opts: { ...notebookStore.entries[0].chartOpts }, promql_query: 'latency' };
    applyResultToPlot(reloaded, result('9'));
    assert.equal(reloaded.opts.format.range, undefined, 'nothing clamps the 9');
    assert.equal(reloaded.data[2][1], 9);
});
