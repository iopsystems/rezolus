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
