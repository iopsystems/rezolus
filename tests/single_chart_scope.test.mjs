// The single-chart view of a chart of a combined view: the Scope field
// lists the chart's source uuids, and Apply, Remove and Reset set the scope
// every query of the view is sent with. Renders the real SingleChartView
// with ViewerApi's range query stubbed.
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

const { ViewerApi } = await import('../src/viewer/assets/lib/viewer_api.js');
const { SingleChartView } = await import('../src/viewer/assets/lib/features/explorers.js');

// Each range query's scope (and in `queries`, its text); `hold` keeps the
// next answer back until released; `series` is how many series it returns.
const sent = [];
const queries = [];
let hold = null;
let series = 1;
let fail = false;
ViewerApi.getMetadata = async () => ({ status: 'success', data: { minTime: 0, maxTime: 60, interval: 1 } });
ViewerApi.queryRange = async (query, start, end, step, captureId, signal, rateMode, scope) => {
    sent.push(scope);
    queries.push(query);
    if (hold) await hold;
    if (fail) return { status: 'error', error: 'bad query' };
    if (query.startsWith('histogram_heatmap(')) {
        const result = { timestamps: [1], bucket_bounds: [1, 2], data: [[0, 0, 1]], min_value: 1, max_value: 1 };
        return { status: 'success', data: { resultType: 'histogram_heatmap', result } };
    }
    const result = Array.from({ length: series }, () => ({ metric: {}, values: [[1, '1']] }));
    return { status: 'success', data: { resultType: 'matrix', result } };
};
// No display transport here: the heatmap falls back to the range query.
ViewerApi.queryRangeDisplay = async () => { throw new Error('no display transport'); };

const walk = (node, out = []) => {
    if (node == null || typeof node !== 'object') return out;
    if (Array.isArray(node)) { node.forEach((n) => walk(n, out)); return out; }
    out.push(node);
    walk(node.children, out);
    return out;
};

// A mounted view of chart `busy`, whose plot has `scope` (and `opts`).
const mount = (scope, opts = { type: 'line' }) => {
    const plot = {
        promql_query: 'sum(irate(cpu_usage[5m]))',
        promql_query_total: 'sum(cpu_usage_total)',
        opts: { id: 'busy', title: 'Busy', ...opts },
    };
    if (scope) plot.scope = scope;
    const vnode = {
        attrs: {
            data: { groups: [{ subgroups: [{ plots: [plot] }] }], interval: 1 },
            chartId: 'busy',
            applyResultToPlot: () => {},
        },
        state: {},
    };
    SingleChartView.oninit(vnode);
    const render = () => SingleChartView.view(vnode);
    const field = () => walk(render()).find((n) => n.tag === 'div.single-chart-field.scope-field');
    const input = () => walk(field()).find((n) => n.tag === 'input.field-input');
    const button = (label) => walk(field()).find((n) => n.tag === 'button.field-apply-btn' && n.children === label);
    const action = (label) => walk(render()).find((n) => n.tag === 'button.section-action-btn' && n.children === label);
    return { vnode, render, field, input, button, action };
};

test('the field lists the chart scope and Remove, Reset and Apply set the queries scope', async () => {
    const v = mount(['u1', 'u2']);
    assert.equal(v.input().attrs.value, 'u1, u2');
    assert.equal(v.button('Reset'), undefined, 'the chart scope needs no reset');

    sent.length = 0;
    await v.button('Remove').attrs.onclick();
    assert.deepEqual(sent, [null]);
    assert.equal(v.input().attrs.value, '');
    assert.equal(v.button('Remove'), undefined);

    sent.length = 0;
    await v.button('Reset').attrs.onclick();
    assert.deepEqual(sent, [['u1', 'u2']]);

    sent.length = 0;
    v.input().attrs.oninput({ target: { value: 'u3, u3' } });
    await v.button('Apply').attrs.onclick();
    assert.deepEqual(sent, [['u3']]);
    assert.equal(v.vnode.state.scope.join(), 'u3');
    // The chart's spec carries the view's scope, for its Total toggle and
    // spectra.
    const chart = walk(v.render()).find((n) => n.attrs?.spec?.opts?.id === 'busy');
    assert.deepEqual(chart.attrs.spec.scope, ['u3']);
});

test('a chart without a scope has no Scope field', () => {
    assert.equal(mount(null).field(), undefined);
});

test('the scope cannot change while a query is loading', async () => {
    const v = mount(['u1']);
    let release;
    hold = new Promise((r) => { release = r; });
    sent.length = 0;
    const first = v.button('Remove').attrs.onclick();
    assert.equal(v.vnode.state.loading, true);
    assert.equal(v.button('Reset').attrs.disabled, true);
    v.input().attrs.oninput({ target: { value: 'u9' } });
    const second = v.button('Apply').attrs.onclick();
    hold = null;
    release();
    await Promise.all([first, second]);
    assert.deepEqual(sent, [null], 'only the first change was sent');
    assert.equal(v.vnode.state.scope, null);
});

test('a query that returns no series says so, mentioning the scope', async () => {
    const v = mount(['u1']);
    series = 0;
    try {
        await v.button('Remove').attrs.onclick();
        assert.equal(v.vnode.state.error, 'The query returned no series.');
        await v.button('Reset').attrs.onclick();
        assert.equal(v.vnode.state.error, 'The query returned no series from the sources in its scope.');
    } finally {
        series = 1;
    }
});

test('with no query the scope is not changed', async () => {
    const v = mount(['u1']);
    v.render();
    v.vnode.state.query = '  ';
    sent.length = 0;
    v.input().attrs.oninput({ target: { value: 'u2' } });
    await v.button('Apply').attrs.onclick();
    assert.deepEqual(sent, []);
    assert.deepEqual(v.vnode.state.scope, ['u1']);
    assert.equal(v.input().attrs.value, 'u1');
    assert.match(v.vnode.state.error, /Enter a query/);
});

test('in heatmap mode a new scope fetches the heatmap again with it', async () => {
    const v = mount(['u1'], { type: 'histogram', subtype: 'percentiles' });
    await v.action('SHOW HEATMAP').attrs.onclick();
    assert.equal(v.vnode.state.heatmapMode, true);
    sent.length = 0;
    queries.length = 0;
    v.input().attrs.oninput({ target: { value: 'u2' } });
    await v.button('Apply').attrs.onclick();
    const heatmaps = queries.flatMap((q, i) => (q.startsWith('histogram_heatmap(') ? [sent[i]] : []));
    assert.deepEqual(heatmaps, [['u2']], JSON.stringify(queries));
    assert.equal(v.vnode.state.heatmapMode, true);
});

test('a scope whose query fails is not kept', async () => {
    const v = mount(['u1']);
    fail = true;
    try {
        v.input().attrs.oninput({ target: { value: 'u2' } });
        await v.button('Apply').attrs.onclick();
    } finally {
        fail = false;
    }
    assert.equal(v.vnode.state.error, 'bad query');
    assert.deepEqual(v.vnode.state.scope, ['u1']);
    assert.equal(v.input().attrs.value, 'u1');
    assert.equal(v.button('Reset'), undefined);
});

test("the chart's Total is offered only for its own query and scope", async () => {
    const v = mount(['u1']);
    const spec = () => walk(v.render()).find((n) => n.attrs?.spec?.opts?.id === 'busy').attrs.spec;
    assert.equal(spec().promql_query_total, 'sum(cpu_usage_total)');
    // Editing the text runs nothing, and the chart still draws its own query.
    v.vnode.state.query = 'sum(irate(cpu_usage{state="user"}[5m]))';
    assert.equal(spec().promql_query_total, 'sum(cpu_usage_total)');
    // Once the edited query has run, there is no Total, even with the
    // original text typed back without running it.
    await walk(v.render()).find((n) => n.tag === 'button.execute-btn').attrs.onclick();
    assert.equal(spec().promql_query_total, undefined);
    v.vnode.state.query = 'sum(irate(cpu_usage[5m]))';
    assert.equal(spec().promql_query_total, undefined);
    await walk(v.render()).find((n) => n.tag === 'button.execute-btn').attrs.onclick();
    assert.equal(spec().promql_query_total, 'sum(cpu_usage_total)');
    // Another scope has no Total either.
    v.input().attrs.oninput({ target: { value: 'u2' } });
    await v.button('Apply').attrs.onclick();
    assert.equal(spec().promql_query_total, undefined);
    assert.deepEqual(spec().scope, ['u2']);
});

test('while a new scope is applied the spec keeps the drawn scope and the heatmap waits', async () => {
    const v = mount(['u1'], { type: 'histogram', subtype: 'percentiles' });
    const spec = () => walk(v.render()).find((n) => n.attrs?.spec?.opts?.id === 'busy').attrs.spec;
    let release;
    hold = new Promise((r) => { release = r; });
    sent.length = 0;
    queries.length = 0;
    v.input().attrs.oninput({ target: { value: 'u2' } });
    const applying = v.button('Apply').attrs.onclick();
    assert.deepEqual(spec().scope, ['u1']);
    assert.equal(v.action('SHOW HEATMAP').attrs.disabled, true);
    // Execute (and Ctrl+Enter) and SHOW HEATMAP send nothing while it runs.
    await walk(v.render()).find((n) => n.tag === 'button.execute-btn').attrs.onclick();
    await v.action('SHOW HEATMAP').attrs.onclick();
    hold = null;
    release();
    await applying;
    assert.deepEqual(sent, [['u2']], JSON.stringify(queries));
    assert.deepEqual(spec().scope, ['u2']);
});

test('a scope cannot be applied while a heatmap is loading', async () => {
    const v = mount(['u1'], { type: 'histogram', subtype: 'percentiles' });
    v.render();
    let release;
    hold = new Promise((r) => { release = r; });
    sent.length = 0;
    const heatmap = v.action('SHOW HEATMAP').attrs.onclick();
    assert.equal(v.vnode.state.heatmapLoading, true);
    v.input().attrs.oninput({ target: { value: 'u2' } });
    const applying = v.button('Apply').attrs.onclick();
    hold = null;
    release();
    await Promise.all([heatmap, applying]);
    assert.deepEqual(sent, [['u1']]);
    assert.deepEqual(v.vnode.state.scope, ['u1']);
});

test('the same uuids in another order are the same scope', async () => {
    const v = mount(['u1', 'u2']);
    sent.length = 0;
    v.input().attrs.oninput({ target: { value: 'u2, u1' } });
    await v.button('Apply').attrs.onclick();
    assert.deepEqual(sent, []);
    assert.equal(v.button('Reset'), undefined);
});
