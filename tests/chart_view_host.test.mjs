import test from 'node:test';
import assert from 'node:assert/strict';

// ui/chart_view.js gives the chart modules the viewer's theme version,
// Mithril redraw and event annotations through charts/host.js when it loads,
// and wraps charts/chart.js's Chart in a Mithril component. These check what
// it registers, how its event wrappers use the store, and that ChartView
// drives the Chart's lifecycle. Events are shown in the Notebook and Report
// views and can be added or deleted only in the Notebook, decided by the
// Mithril route, which a stub stands in for.

// charts/util/colormap.js reads CSS custom properties when it loads, and
// ui/theme.js's toggleTheme sets the theme attribute and saves the choice.
let themeAttr = 'dark';
globalThis.document = {
    documentElement: { getAttribute: () => themeAttr, setAttribute: (_k, v) => { themeAttr = v; } },
    querySelector: () => null,
};
globalThis.localStorage = { getItem: () => null, setItem() {} };
globalThis.getComputedStyle = () => ({ getPropertyValue: () => '' });

let route = '/cpu';
let redraws = 0;
globalThis.m = Object.assign(() => ({}), {
    route: { get: () => route },
    redraw: () => { redraws += 1; },
});

const { chartHost } = await import('../src/viewer/assets/lib/charts/host.js');
const { ChartsState } = await import('../src/viewer/assets/lib/charts/chart.js');
const { ChartView, viewerChartHost } = await import('../src/viewer/assets/lib/ui/chart_view.js');
const { eventsStore, EventsStore } = await import('../src/viewer/assets/lib/events/events_store.js');
const theme = await import('../src/viewer/assets/lib/ui/theme.js');

const EVENT = { timestamp: 1_000_000_000, description: 'deploy', chart_id: 'cpu' };

function withRoute(r, fn) {
    const prev = route;
    route = r;
    try { return fn(); } finally { route = prev; }
}

// A host built from a fresh store and popovers that record what they were
// given, so a test can call the callbacks the popovers would.
function stubbedHost() {
    const store = new EventsStore();
    const forms = [];
    const infos = [];
    const host = viewerChartHost({
        eventsStore: store,
        openEventForm: (args) => forms.push(args),
        openEventInfo: (args) => infos.push(args),
    });
    return { host, store, forms, infos };
}

test('loading chart_view.js registers the page and event functions', () => {
    const h = chartHost();
    const before = redraws;
    h.redraw();
    assert.equal(redraws, before + 1);
    assert.equal(typeof h.openEventForm, 'function');
    assert.equal(typeof h.openEventInfo, 'function');
});

test('the registered theme version follows toggleTheme', () => {
    const before = chartHost().getThemeVersion();
    theme.toggleTheme();
    assert.equal(chartHost().getThemeVersion(), before + 1);
    assert.equal(chartHost().getThemeVersion(), theme.themeVersion);
});

test('events are shown in the Notebook and Report views only', () => {
    eventsStore.replaceAll([EVENT]);
    try {
        const forCpu = () => chartHost().eventsForChart({ chartId: 'cpu', scope: {} });
        assert.deepEqual(withRoute('/cpu', forCpu), []);
        assert.deepEqual(withRoute('/notebook', forCpu), [EVENT]);
        assert.deepEqual(withRoute('/report', forCpu), [EVENT]);
        assert.deepEqual(withRoute('/notebook',
            () => chartHost().eventsForChart({ chartId: 'memory', scope: {} })), []);
    } finally {
        eventsStore.replaceAll([]);
    }
});

test('events can be added in the Notebook only', () => {
    assert.equal(withRoute('/notebook', () => chartHost().eventEditingAllowed()), true);
    assert.equal(withRoute('/report', () => chartHost().eventEditingAllowed()), false);
    assert.equal(withRoute('/cpu', () => chartHost().eventEditingAllowed()), false);
});

test('subscribeEvents follows the events store', () => {
    let calls = 0;
    const unsubscribe = chartHost().subscribeEvents(() => { calls += 1; });
    eventsStore.replaceAll([EVENT]);
    unsubscribe();
    eventsStore.replaceAll([]);
    assert.equal(calls, 1);
});

test('submitting the add-event form stores the event, then calls the chart back', () => {
    const { host, store, forms } = stubbedHost();
    let submitted = null;
    let storedWhenCalled = null;
    host.openEventForm({
        anchorEl: {},
        prefill: { chart_id: 'cpu' },
        onSubmit: (e) => {
            submitted = e;
            storedWhenCalled = store.filterForChart({ chartId: 'cpu' }).length;
        },
    });
    assert.equal(forms.length, 1);
    forms[0].onSubmit(EVENT);
    assert.deepEqual(store.filterForChart({ chartId: 'cpu' }), [EVENT]);
    assert.equal(submitted, EVENT);
    assert.equal(storedWhenCalled, 1);
});

test('the event popover offers Delete in the Notebook only', () => {
    const { host, store, infos } = stubbedHost();
    store.replaceAll([EVENT]);
    withRoute('/report', () => host.openEventInfo({ anchorPoint: { x: 0, y: 0 }, event: EVENT }));
    assert.equal(infos[0].onDelete, null);
    withRoute('/notebook', () => host.openEventInfo({ anchorPoint: { x: 0, y: 0 }, event: EVENT }));
    assert.equal(typeof infos[1].onDelete, 'function');
    infos[1].onDelete();
    assert.deepEqual(store.filterForChart({ chartId: 'cpu' }), []);
});

test('ChartView drives the Chart through mount, update and destroy', () => {
    const attrs = { spec: { opts: { id: 'x' } }, chartsState: new ChartsState(), interval: 1 };
    const view = new ChartView({ attrs });
    const calls = [];
    view.chart.mount = (dom) => calls.push(['mount', dom]);
    view.chart.update = (a) => calls.push(['update', a.spec]);
    view.chart.destroy = () => calls.push(['destroy']);
    const dom = {};
    const next = { ...attrs, spec: { opts: { id: 'x' }, data: [[1], [2]] } };
    view.oncreate({ dom, attrs });
    view.onupdate({ dom, attrs: next });
    view.onremove({ dom, attrs: next });
    assert.deepEqual(calls, [['mount', dom], ['update', next.spec], ['destroy']]);
    assert.equal(view.chart.spec, attrs.spec);
});
