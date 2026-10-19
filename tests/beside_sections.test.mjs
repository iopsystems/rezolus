// A recording shown beside a comparison: its sections, its charts and the
// charts pinned from them render as one capture, and the badge names it
// after the arms. Each decision is a pure helper in group_utils.js; the
// tests below also render the components that call them, so a component
// that stops calling its helper fails here.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import {
    isSingleCapture, besideNames, sectionCompares, groupCompares, pinCompareMeta, pinnedCompares,
    defaultLandingRoute, uploadLandingRoute,
} from '../src/viewer/assets/lib/features/group_utils.js';

// The components touch the DOM at import; a vnode-returning `m` and a few
// stubs are enough to render them in node.
const isVnode = (x) => x != null && typeof x === 'object' && 'tag' in x;
globalThis.m = (tag, ...rest) => {
    const attrs = rest.length && rest[0] != null && typeof rest[0] === 'object'
        && !Array.isArray(rest[0]) && !isVnode(rest[0]) ? rest.shift() : {};
    return { tag, attrs, children: rest.length === 1 ? rest[0] : rest };
};
// `m.route(root, default, routes)` records the page's default route; an
// upload's `m.route.set` records where it went.
const routed = { default: null, set: [] };
globalThis.m.route = Object.assign((root, def) => { routed.default = def; }, {
    get: () => '/', set: (r) => routed.set.push(r), prefix: '#!',
});
globalThis.m.request = async () => ({ status: 'success', data: [] });
globalThis.m.mount = () => {};
globalThis.requestAnimationFrame = () => 0;
globalThis.m.trust = (s) => s;
globalThis.m.redraw = () => {};
globalThis.window = {
    addEventListener() {}, removeEventListener() {}, location: { search: '', hash: '', pathname: '/' },
    history: { replaceState() {} }, scrollTo() {},
    matchMedia: () => ({ matches: false, addEventListener() {} }),
};
globalThis.document = {
    documentElement: { dataset: {}, getAttribute() {}, setAttribute() {} },
    addEventListener() {}, body: { appendChild() {} }, getElementById: () => null, querySelector: () => null,
    createElement: () => ({
        style: {}, classList: { add() {}, remove() {} }, appendChild() {}, remove() {},
        setAttribute() {}, addEventListener() {}, querySelector: () => null,
    }),
    querySelectorAll: () => [], title: '',
};
const storage = new Map();
globalThis.localStorage = {
    getItem: (k) => storage.get(k) ?? null, setItem: (k, v) => storage.set(k, v), removeItem: (k) => storage.delete(k),
};
globalThis.sessionStorage = globalThis.localStorage;
globalThis.fetch = async () => ({ ok: false, status: 404, json: async () => ({}), text: async () => '' });
globalThis.getComputedStyle = () => ({ getPropertyValue: () => '' });

const { createGroupComponent, CompareChartWrapper } = await import('../src/viewer/assets/lib/viewer_core.js');
const { TopNav } = await import('../src/viewer/assets/lib/ui/layout.js');
const {
    notebookStore, reportStore, persistNotebook, loadPayloadIntoStore, buildPayload, pinnedChart,
    SELECTION_SCHEMA_VERSION,
} = await import('../src/viewer/assets/lib/selection/selection.js');
const { Chart } = await import('../src/viewer/assets/lib/charts/chart.js');
const app = await import('../src/viewer/assets/lib/app.js');
const { uploadParquet } = await import('../src/viewer/assets/lib/script.js');
const { ViewerApi } = await import('../src/viewer/assets/lib/viewer_api.js');
const { renderServiceSection } = await import('../src/viewer/assets/lib/features/service.js');
const { createMetadataView, besideRows } = await import('../src/viewer/assets/lib/sections/section_views.js');

// Every vnode in a tree.
const walk = (node, out = []) => {
    if (node == null || typeof node !== 'object') return out;
    if (Array.isArray(node)) { node.forEach((n) => walk(n, out)); return out; }
    out.push(node);
    walk(node.children, out);
    return out;
};
const text = (node) => walk(node).flatMap((n) => (typeof n.children === 'string' ? [n.children] : [])).join(' ');

test('a section marked single_capture renders as one capture', () => {
    assert.equal(isSingleCapture({ single_capture: 'h' }), true);
    assert.equal(isSingleCapture({}), false);
    assert.equal(isSingleCapture(undefined), false);
    assert.equal(sectionCompares(true, { single_capture: 'h' }), false);
    assert.equal(sectionCompares(true, {}), true);
    assert.equal(sectionCompares(false, {}), false);
    assert.equal(groupCompares(true, { singleCapture: true }), false);
    assert.equal(groupCompares(true, {}), true);
    assert.equal(groupCompares(false, {}), false);
});

test('a pin records the aliases in compare mode and single_capture beside it', () => {
    const aliases = { baselineAlias: 'a' };
    assert.equal(pinCompareMeta(true, true, aliases), aliases);
    assert.deepEqual(pinCompareMeta(true, false, aliases), { singleCapture: true });
    assert.equal(pinCompareMeta(false, false, aliases), null);
});

test('a pinned chart of a beside recording does not draw A/B', () => {
    const spec = { promql_query: 'irate(x[5m])' };
    assert.equal(pinnedCompares(true, {}, spec), true);
    assert.equal(pinnedCompares(false, {}, spec), false);
    assert.equal(pinnedCompares(true, {}, {}), false);
    assert.equal(pinnedCompares(true, { single_capture: true }, spec), false);
    // Saved before `single_capture` existed: the query names the recording.
    assert.equal(pinnedCompares(true, { promql_query: 'irate(x{__capture__="h"}[5m])' }, spec), false);
    // An entry with no query of its own falls back to the spec's.
    assert.equal(pinnedCompares(true, {}, { promql_query: 'irate(x{__capture__="h"}[5m])' }), false);
});

test('the badge names each beside recording once, in nav order', () => {
    const sections = [
        { route: '/cpu', instance: null },
        { route: '/cpu@h', instance: 'h' },
        { route: '/memory@h', instance: 'h' },
        { route: '/cpu@g', instance: 'g' },
        { route: '/memory' },
    ];
    assert.deepEqual(besideNames(sections), ['h', 'g']);
    assert.deepEqual(besideNames(undefined), []);
});

test('Group draws a singleCapture group without the compare wrapper', () => {
    const Group = createGroupComponent(() => ({
        chartsState: { charts: new Map() }, heatmapEnabled: false, heatmapLoading: false,
        heatmapDataCache: new Map(), compareMode: true, toggles: {}, setChartToggle() {},
        anchors: { baseline: 0, experiment: 0 },
    }));
    const plot = { opts: { id: 'p', title: 'P' }, promql_query: 'x', data: [[1], [2]] };
    const render = (singleCapture) => Group.view({
        attrs: { id: 'g', name: 'G', plots: [plot], sectionRoute: '/cpu@h', singleCapture },
    });
    const wrappers = (tree) => walk(tree).filter((n) => n.tag === CompareChartWrapper).length;
    assert.equal(wrappers(render(false)), 1);
    assert.equal(wrappers(render(true)), 0);
    // Its pin records that it is a single capture, and does not claim the
    // comparison's aliases for the notebook.
    const pin = (tree) => walk(tree).find((n) => n.tag === 'button.chart-select');
    notebookStore.entries.length = 0;
    notebookStore.compare = null;
    pin(render(true)).attrs.onclick({ stopPropagation() {} });
    assert.equal(notebookStore.entries[0].single_capture, true);
    assert.equal(notebookStore.compare, null);
    notebookStore.entries.length = 0;
    pin(render(false)).attrs.onclick({ stopPropagation() {} });
    assert.equal(notebookStore.entries[0].single_capture, undefined);
    assert.notEqual(notebookStore.compare, null);
});

test('a service section beside a comparison asks its groups for one capture', () => {
    const groups = (meta) => walk(renderServiceSection({ metadata: meta, groups: [{ id: 'g' }] }, 'Group', '/service/x@h', 'x', 1))
        .filter((n) => n.tag === 'Group')
        .map((n) => n.attrs.singleCapture);
    assert.deepEqual(groups({ single_capture: 'h' }), [true]);
    assert.deepEqual(groups({}), [false]);
});

test('the compare badge lists beside recordings after the arms', () => {
    const nav = (sections) => TopNav.view({
        attrs: { compareMode: true, sections, groups: [], baselineAlias: 'svcA', experimentAlias: 'svcB' },
    });
    const summary = (tree) => walk(tree).find((n) => n.tag === 'summary.compare-badge-summary');
    const withBeside = summary(nav([{ route: '/cpu@h', instance: 'h' }]));
    assert.equal(withBeside.attrs.title, 'Comparing svcA, svcB; also: h');
    assert.match(text(withBeside), /also: h/);
    assert.doesNotMatch(text(summary(nav([{ route: '/cpu' }]))), /also:/);
});

test('the page opens on the section the backend marked landing', () => {
    const sections = [
        { route: '/overview' }, { route: '/service/svcA', landing: true }, { route: '/overview@h', instance: 'h' },
    ];
    assert.equal(defaultLandingRoute({ sections }), '/service/svcA');
    assert.equal(defaultLandingRoute({ sections, serviceNames: ['other'] }), '/service/svcA');
    assert.equal(defaultLandingRoute({ sections, reportMode: true }), '/report');
    assert.equal(defaultLandingRoute({ sections, categoryName: 'cat' }), '/service/cat');
    const plain = [{ route: '/overview' }, { route: '/service/x' }];
    assert.equal(defaultLandingRoute({ sections: plain }), '/overview');
    assert.equal(defaultLandingRoute({ sections: plain, serviceNames: ['x'] }), '/service/x');
    assert.equal(defaultLandingRoute({ sections: [{ route: '/source/app' }] }), '/source/app');
    assert.equal(defaultLandingRoute({}), '/overview');
});

test('Metadata lists the recordings shown beside the comparison', () => {
    const meta = {
        source: 'svcA',
        beside_recordings: { h: { source: 'rezolus', version: '6.0.0', sampling_interval_ms: 1000 } },
    };
    assert.deepEqual(besideRows(meta), [['h', 'source rezolus, version 6.0.0, 1000 ms']]);
    assert.deepEqual(besideRows({}), []);
    const view = createMetadataView().view({ attrs: { data: meta } });
    assert.match(text(view), /Shown beside the comparison/);
    assert.match(text(view), /source rezolus, version 6\.0\.0, 1000 ms/);
});

test('an upload opens on the landing in the sections response', () => {
    const response = { status: 'success', data: { sections: [
        { route: '/overview' }, { route: '/service/svcA', landing: true },
    ] } };
    assert.equal(uploadLandingRoute(response), '/service/svcA');
    assert.equal(uploadLandingRoute({ data: { sections: [{ route: '/overview' }] } }), '/overview');
    assert.equal(uploadLandingRoute(undefined), '/overview');
});

test('uploadParquet routes to the landing the sections response names', async () => {
    const sections = [{ route: '/overview', name: 'Overview' }, { route: '/service/svcA', name: 'svcA', landing: true }];
    const stubs = {
        uploadParquet: async () => ({}),
        getMetadata: async () => ({ status: 'success', data: {} }),
        getSystemInfo: async () => null,
        getSelection: async () => null,
        getFileMetadata: async () => ({}),
        getMode: async () => ({ compare_mode: true }),
        getSections: async () => ({ status: 'success', data: { sections } }),
        // A section body carries no sections: the server strips them.
        getSection: async () => ({ groups: [], metadata: {} }),
    };
    const saved = {};
    for (const [k, f] of Object.entries(stubs)) { saved[k] = ViewerApi[k]; ViewerApi[k] = f; }
    try {
        routed.set.length = 0;
        await uploadParquet({ name: 'conc.rez', size: 1 });
        assert.deepEqual(routed.set, ['/service/svcA']);
    } finally {
        Object.assign(ViewerApi, saved);
    }
});

test('the first load opens on the landing, and SectionContent asks for one capture', () => {
    app.bootstrapSharedSections([
        { route: '/overview', name: 'Overview' },
        { route: '/service/svcA', name: 'svcA', landing: true },
        { route: '/cpu@h', name: 'CPU', instance: 'h' },
    ]);
    app.initDashboard({ compareMode: true });
    assert.equal(routed.default, '/service/svcA');
    const plot = { opts: { id: 'p', title: 'P' }, promql_query: 'x', data: [[1], [2]] };
    const render = (metadata) => app.SectionContent.view({ attrs: {
        section: { route: '/cpu@h', name: 'CPU' }, groups: [{ id: 'g', name: 'G', plots: [plot] }],
        metadata, interval: 1,
    } });
    const asks = (tree) => walk(tree).filter((n) => n.attrs && 'singleCapture' in n.attrs)
        .map((n) => n.attrs.singleCapture);
    assert.deepEqual(asks(render({ single_capture: 'h' })), [true]);
    assert.deepEqual(asks(render({})), [false]);
});

test('a pinned chart draws one capture when its entry says so, A/B otherwise', () => {
    const spec = { opts: { id: 'p' }, promql_query: 'irate(x[5m])' };
    const opts = { interval: 1, sectionRoute: '/cpu', captureLabels: {}, anchors: {}, toggles: {}, setChartToggle() {} };
    const attrs = { compareMode: true, chartsState: {} };
    const single = pinnedChart({ single_capture: true }, spec, attrs, opts);
    assert.equal(single.body.tag, Chart);
    assert.equal(single.toggle, false);
    const both = pinnedChart({}, spec, attrs, opts);
    assert.equal(both.body.tag, CompareChartWrapper);
    assert.notEqual(both.toggle, false);
    // An entry saved before `single_capture`, whose own query names the
    // recording, though the spec it renders from does not.
    const legacy = pinnedChart({ promql_query: 'irate(x{__capture__="h"}[5m])' }, spec, attrs, opts);
    assert.equal(legacy.body.tag, Chart);
});

test('single_capture survives the notebook, a loaded report and a saved payload', () => {
    const entry = { chartId: 'c', section: 'cpu@h', promql_query: 'q', single_capture: true, chartOpts: {} };
    notebookStore.entries.length = 0;
    notebookStore.entries.push({ ...entry, note: '' });
    persistNotebook();
    const stored = [...storage.values()].map((v) => JSON.parse(v)).find((d) => Array.isArray(d.entries));
    assert.equal(stored.entries[0].single_capture, true);
    assert.ok(loadPayloadIntoStore(reportStore, { version: SELECTION_SCHEMA_VERSION, entries: [entry] }));
    assert.equal(reportStore.entries[0].single_capture, true);
    assert.equal(buildPayload(reportStore, {}).entries[0].single_capture, true);
    notebookStore.entries.length = 0;
});
