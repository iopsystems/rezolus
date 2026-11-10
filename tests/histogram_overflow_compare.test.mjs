// A +Inf quantile in an A/B comparison. The baseline's percentile chart
// keeps +Inf and a range.max for its out-of-bounds band (data.js
// markOverflow); the compare overlays draw both captures' +Inf as a gap and
// must not take that range, which would clamp the experiment's finite values
// above it.
import { test } from 'node:test';
import assert from 'node:assert/strict';

// viewer_core touches the DOM at import; a vnode-returning `m` and a few
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

const { CompareChartWrapper } = await import('../src/viewer/assets/lib/viewer_core.js');
const { applyResultToPlot } = await import('../src/viewer/assets/lib/data.js');

const result = (p99) => ({
    status: 'success',
    data: {
        resultType: 'matrix',
        result: [
            { metric: { quantile: '0.5' }, values: [[1, '1'], [2, '1'], [3, '1']] },
            { metric: { quantile: '0.99' }, values: p99 },
        ],
    },
});

test('a compare overlay draws +Inf as a gap and takes the plot\'s own format', async () => {
    const saved = globalThis.m.request;
    // The experiment's p99 is finite and above the baseline's largest finite
    // value (5), at 9.
    globalThis.m.request = async (o) => (String(o?.url).startsWith('/api/v1/metadata')
        ? { status: 'success', data: { interval: 1, minTime: 0, maxTime: 3 } }
        : result([[1, '9'], [2, '+Inf'], [3, '9']]));
    try {
        const spec = {
            promql_query: 'latency',
            opts: { id: 'h', title: 'h', type: 'histogram', subtype: 'percentiles', style: 'scatter' },
        };
        applyResultToPlot(spec, result([[1, '5'], [2, '+Inf'], [3, '4']]));
        assert.equal(spec.opts.format.range.max, 5, 'the baseline chart has its band');

        const vnode = {
            attrs: { spec, experimentQueryRange: { start: 0, end: 3, step: 1, interval: 1 } },
            state: {},
        };
        CompareChartWrapper.oninit(vnode);
        await new Promise((r) => setTimeout(r, 20));
        const out = CompareChartWrapper.view(vnode);
        assert.equal(out.tag, 'div.compare-split-subgroup', JSON.stringify(out));
        const specs = out.children.map((w) => w.children.find((c) => c && c.attrs?.spec).attrs.spec);
        const p99 = specs.find((s) => s._splitLabel === 'p99');
        assert.ok(p99, specs.map((s) => s._splitLabel).join());
        assert.equal(p99.opts.format, undefined, 'no range from the baseline');
        assert.equal(p99.multiSeries.length, 2);
        for (const s of p99.multiSeries) {
            assert.ok(!s.valueData.includes(Infinity), `${s.name}: ${s.valueData}`);
            assert.ok(s.valueData[1] == null || Number.isNaN(s.valueData[1]), `${s.name}: ${s.valueData}`);
        }
        assert.deepEqual(p99.multiSeries.map((s) => s.valueData[0]).sort(), [5, 9]);
        // The baseline plot keeps its band for the single-capture chart.
        assert.equal(spec.opts.format.range.max, 5);
    } finally {
        globalThis.m.request = saved;
    }
});
