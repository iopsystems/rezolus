// The cgroups section in an A/B comparison: the experiment's query resolves
// the cgroup selection as the baseline's does (experimentQuery), whether or
// not the selector has written it into the plot; discovery lists the
// cgroups of every capture (queryEveryCapture); and an individual chart with
// nothing selected says so instead of drawing an error.
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

const { experimentQuery, queryEveryCapture, CompareChartWrapper } = await import('../src/viewer/assets/lib/viewer_core.js');
const { substituteCgroupPattern } = await import('../src/viewer/assets/lib/data.js');
const { cgroupQueryVariants, deriveCgroupGroups, resolveCgroupSelection } = await import('../src/viewer/assets/lib/features/cgroup_selection.js');
const { discoverCgroups } = await import('../src/viewer/assets/lib/features/cgroup_discovery.js');

const INDIVIDUAL = 'sum by (name) (irate(cgroup_cpu_usage{name=~"__SELECTED_CGROUPS__"}[5m])) / 1000000000';
const AGGREGATE = 'sum(irate(cgroup_cpu_usage{name!~"__SELECTED_CGROUPS__"}[5m])) / 1000000000';
const plot = (promql_query) => ({ promql_query, opts: { id: 'c', title: 'c', style: 'multi' }, data: [] });

const names = new Set(['/system.slice/a.service/x', '/system.slice/a.service/y', '/system.slice/ssh.service', '/user.slice']);
const groups = deriveCgroupGroups(names);
const selection = resolveCgroupSelection(new Set(['/system.slice/a.service/', '/system.slice/ssh.service']), groups);

// What the cgroup selector writes into the plot (cgroup_selector.js
// updateQueries).
const selectorQuery = (template) => {
    const [v] = cgroupQueryVariants(template, selection);
    return substituteCgroupPattern(v.query, v.selection);
};

test('the experiment resolves a template plot as the selector would have', () => {
    for (const template of [INDIVIDUAL, AGGREGATE]) {
        const want = selectorQuery(template);
        assert.ok(!want.includes('__SELECTED_CGROUPS__'));
        // A plot still carrying the template (a refresh replaced it, or the
        // selector has not run yet) and one the selector rewrote ask the
        // experiment for the same query.
        assert.equal(experimentQuery(plot(template), { activeCgroupPattern: selection }), want);
        assert.equal(experimentQuery(plot(want), { activeCgroupPattern: selection }), want);
    }
    assert.match(selectorQuery(INDIVIDUAL), /label_replace/, 'a group renames its members');
});

test('an individual chart with nothing selected has no experiment query', () => {
    assert.equal(experimentQuery(plot(INDIVIDUAL), { activeCgroupPattern: null }), null);
    assert.equal(experimentQuery(plot(AGGREGATE), { activeCgroupPattern: null }),
        'sum(irate(cgroup_cpu_usage[5m])) / 1000000000');
});

test('an individual chart with nothing selected says so in compare mode', () => {
    const vnode = { attrs: { spec: plot(INDIVIDUAL), activeCgroupPattern: null }, state: {} };
    CompareChartWrapper.oninit(vnode);
    const out = CompareChartWrapper.view(vnode);
    assert.equal(out.tag, 'div.chart-loading');
    assert.equal(out.children, 'No cgroups selected');
});

test('discovery lists the cgroups of every capture', async () => {
    const byCapture = {
        baseline: ['/a', '/b'],
        experiment: ['/b', '/c'],
        arm3: ['/d'],
        broken: 'throw',
    };
    const answer = (names) => ({
        status: 'success',
        data: { resultType: 'matrix', result: names.map((name) => ({ metric: { name }, values: [] })) },
    });
    const seen = [];
    const deps = {
        listCaptures: async () => Object.keys(byCapture).map((id) => ({ id })),
        captureMetadata: async (id) => ({ data: { minTime: 0, maxTime: 60, interval: 1, id } }),
        queryRangeForCapture: async (id, query) => {
            seen.push(id);
            if (byCapture[id] === 'throw') throw new Error('refused');
            return answer(byCapture[id]);
        },
    };
    const names = await discoverCgroups((query) => queryEveryCapture(
        query, async () => answer(byCapture.baseline), deps));
    assert.deepEqual([...names].sort(), ['/a', '/b', '/c', '/d']);
    assert.ok(!seen.includes('baseline'), 'the baseline is read through its own scoped query');
});

test('a compare chart fetches again when the selection changes, and only then', async () => {
    // Every request the viewer API makes, by either transport.
    const saved = { fetch: globalThis.fetch, request: globalThis.m.request };
    let fetches = 0;
    globalThis.fetch = async () => {
        fetches++;
        return { ok: false, status: 404, json: async () => ({}), text: async () => '', arrayBuffer: async () => new ArrayBuffer(0) };
    };
    globalThis.m.request = async () => {
        fetches++;
        return { status: 'success', data: { resultType: 'matrix', result: [] } };
    };
    const settle = () => new Promise((r) => setTimeout(r, 20));
    try {
        const vnode = {
            attrs: {
                spec: plot(INDIVIDUAL),
                activeCgroupPattern: null,
                experimentQueryRange: { start: 0, end: 60, step: 1, interval: 1 },
            },
            state: {},
        };
        CompareChartWrapper.oninit(vnode);
        CompareChartWrapper.view(vnode);
        await settle();
        assert.equal(fetches, 0, 'nothing selected, nothing fetched');

        // A selection: the same plot, still the template.
        vnode.attrs = { ...vnode.attrs, activeCgroupPattern: selection };
        CompareChartWrapper.view(vnode);
        assert.equal(vnode.state._fetchInFlight, true, 'the selection starts a fetch');
        await settle();
        const after = fetches;
        assert.ok(after > 0);

        // Redraws, and the selector writing the same selection into the plot,
        // fetch nothing more.
        for (let i = 0; i < 5; i++) CompareChartWrapper.view(vnode);
        vnode.attrs = { ...vnode.attrs, spec: plot(selectorQuery(INDIVIDUAL)) };
        CompareChartWrapper.view(vnode);
        await settle();
        assert.equal(fetches, after);
    } finally {
        globalThis.fetch = saved.fetch;
        globalThis.m.request = saved.request;
    }
});

test('a histogram chart does not fetch again while a refresh fetches the metadata again', async () => {
    const { setStepOverride, processDashboardData } = await import('../src/viewer/assets/lib/data.js');
    const savedRequest = globalThis.m.request;
    let fetches = 0;
    let release = null;
    globalThis.m.request = async (o) => {
        if (String(o?.url).startsWith('/api/v1/metadata')) {
            const meta = { status: 'success', data: { interval: 0.1, minTime: 0, maxTime: 600 } };
            if (release === 'hold') return new Promise((r) => { release = () => r(meta); });
            return meta;
        }
        fetches++;
        return { status: 'success', data: { resultType: 'matrix', result: [] } };
    };
    const settle = () => new Promise((r) => setTimeout(r, 20));
    // A 100ms baseline with a 0.5s override: a histogram query is strided
    // while the metadata is cached, and not while a refresh of a followed
    // archive (`freshMetadata`) fetches it again (nativeInterval's 1s).
    await processDashboardData({ groups: [] }, null, '/', {});
    setStepOverride(0.5);
    try {
        const spec = {
            promql_query: 'syscall_latency',
            opts: { id: 'h', title: 'h', type: 'histogram', subtype: 'percentiles', style: 'scatter' },
            data: [],
        };
        const vnode = {
            attrs: { spec, step: 0.5, experimentQueryRange: { start: 0, end: 600, step: 1, interval: 0.1 } },
            state: {},
        };
        CompareChartWrapper.oninit(vnode);
        CompareChartWrapper.view(vnode);
        await settle();
        const before = fetches;
        const result = vnode.state.experimentResult;
        assert.ok(result, 'the first fetch landed');
        release = 'hold';
        const refresh = processDashboardData({ groups: [] }, null, '/', { freshMetadata: true });
        await settle();
        CompareChartWrapper.view(vnode);
        assert.equal(vnode.state.experimentResult, result, 'the chart is not blanked');
        release();
        await refresh;
        CompareChartWrapper.view(vnode);
        await settle();
        assert.equal(fetches, before, 'a metadata refresh fetches nothing');
    } finally {
        setStepOverride(null);
        globalThis.m.request = savedRequest;
    }
});

const { renderCgroupSection, cgroupDiscoverQuery } = await import('../src/viewer/assets/lib/sections/section_views.js');

const cgroupSectionAttrs = () => ({
    section: { route: '/cgroups' },
    groups: [
        { metadata: { side: 'left' }, plots: [plot(AGGREGATE)] },
        { metadata: { side: 'right' }, plots: [plot(INDIVIDUAL)] },
    ],
});
const renderCgroups = (extra) => renderCgroupSection({
    attrs: cgroupSectionAttrs(),
    titleText: 'Cgroups',
    interval: 1,
    chartsState: {},
    Chart: 'Chart',
    CgroupSelector: 'CgroupSelector',
    executePromQLRangeQuery: () => null,
    applyResultToPlot: () => {},
    substituteCgroupPattern: (q) => q,
    setActiveCgroupPattern: () => {},
    globalColorMapper: { getColorByName: () => '#000' },
    ...extra,
});

test('in compare mode each cgroup pair renders as a Group with both plots', () => {
    const Group = 'Group';
    const discoverQuery = () => null;
    const [, selector, pairs] = renderCgroups({ compare: true, Group, discoverQuery }).children;
    assert.equal(selector.attrs.discoverQuery, discoverQuery, 'the selector discovers with the query passed in');
    assert.ok(Array.isArray(pairs) && pairs.length === 1, 'one Group per pair');
    assert.equal(pairs[0].tag, Group);
    assert.deepEqual(
        pairs[0].attrs.subgroups.map((s) => [s.name, s.plots.map((p) => p.promql_query)]),
        [['Aggregate', [AGGREGATE]], ['Individual', [INDIVIDUAL]]],
    );
});

test('outside compare mode the cgroup pairs render side by side, not as Groups', () => {
    const [, , pairs] = renderCgroups({ compare: false, Group: 'Group' }).children;
    assert.equal(pairs.tag, 'div.cgroup-pairs');
});

test('a comparison discovers cgroups on every capture, with the node applied', async () => {
    const calls = [];
    const baselineQuery = (q) => { calls.push(['baseline', q]); return 'b'; };
    const scopedQuery = (q) => { calls.push(['scoped', q]); return 's'; };
    const withNode = (q) => `${q}+node`;
    const queryEveryCapture = async (q, base) => { calls.push(['every', q]); return base(q); };
    const args = { baselineQuery, scopedQuery, withNode, queryEveryCapture };

    assert.equal(cgroupDiscoverQuery({ ...args, compare: false }), baselineQuery);

    const discover = cgroupDiscoverQuery({ ...args, compare: true });
    assert.equal(await discover('q'), 's');
    assert.deepEqual(calls, [['every', 'q+node'], ['scoped', 'q+node']]);
});
