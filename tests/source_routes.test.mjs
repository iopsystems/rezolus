// Regression test for Bug 1: expanding a chart in simple-metric (foreign
// source) mode. The `/source/:sourceName/chart/:chartId` route must reconstruct
// the single chart from the metric catalog (there is no server-rendered section
// to resolve it from) and hand a populated plot to SingleChartView. Mirrors
// tests/service_routes.test.mjs — the route factory is exported so its onmatch
// is exercisable with stubbed deps.
import test from 'node:test';
import assert from 'node:assert/strict';
import { createSourceRoutes } from '../src/viewer/assets/lib/features/source_routes.js';

// Minimal hyperscript fake that distinguishes an attrs object (plain object,
// no `tag`) from children (array / string / vnode), so we can walk the tree.
const setupGlobals = () => {
    const prevM = globalThis.m;
    const prevWindow = globalThis.window;
    globalThis.m = (tag, attrs, ...rest) => {
        let children;
        if (Array.isArray(attrs)) {
            children = attrs; // m(tag, [children])
            attrs = {};
        } else if (attrs && typeof attrs === 'object' && !('tag' in attrs)) {
            children = rest; // m(tag, attrsObject, ...children)
        } else {
            children = [attrs, ...rest].filter((x) => x !== undefined); // m(tag, child)
            attrs = {};
        }
        return { tag, attrs, children };
    };
    globalThis.m.route = { get: () => '/overview', set: () => {} };
    globalThis.m.redraw = () => {};
    globalThis.window = { scrollTo() {} };
    return () => {
        globalThis.m = prevM;
        globalThis.window = prevWindow;
    };
};

const findByTag = (vnode, tag) => {
    if (Array.isArray(vnode)) {
        for (const v of vnode) {
            const found = findByTag(v, tag);
            if (found) return found;
        }
        return null;
    }
    if (!vnode || typeof vnode !== 'object') return null;
    if (vnode.tag === tag) return vnode;
    return findByTag(vnode.children, tag);
};

const baseDeps = (over = {}) => ({
    sectionResponseCache: {},
    ViewerApi: {
        getMetrics: async () => ({
            metrics: [
                { name: 'hub_heartbeat', metric_type: 'counter' },
                { name: 'queue_depth', metric_type: 'gauge' },
            ],
        }),
        getTimestamps: async () => ({ timestamps: [1e9, 2.003e9, 2.998e9] }),
    },
    // Mimic processDashboardData: populate the plot in place and resolve.
    processDashboardData: async (payload) => {
        const plot = payload.groups[0].subgroups[0].plots[0];
        plot.data = [[[0, 1]]];
        return payload;
    },
    applyResultToPlot: () => {},
    SingleChartView: 'SingleChartView',
    TopNav: 'TopNav',
    topNavAttrs: (data, route) => ({ data, route }),
    Main: 'Main',
    getSections: () => [{ name: 'source: hub', route: '/source/hub' }],
    getCompareMode: () => false,
    chartsState: { charts: new Map() },
    ...over,
});

test('source chart route reconstructs the chart from the catalog', async () => {
    const restore = setupGlobals();
    try {
        const routes = createSourceRoutes(baseDeps());
        const comp = routes['/source/:sourceName/chart/:chartId'].onmatch({
            sourceName: 'hub',
            chartId: 'source-metric-hub_heartbeat',
        });
        await comp.ready; // await the async reconstruction

        const v = comp.view();
        const scv = findByTag(v, 'SingleChartView');
        assert.ok(scv, 'expected SingleChartView to render once reconstructed');
        assert.equal(scv.attrs.chartId, 'source-metric-hub_heartbeat');

        const plots = scv.attrs.data.groups[0].subgroups[0].plots;
        assert.equal(plots[0].opts.id, 'source-metric-hub_heartbeat');
        assert.equal(plots[0].promql_query, 'rate(hub_heartbeat[5m])');
        assert.ok(plots[0].data, 'plot should be populated by the query');
    } finally {
        restore();
    }
});

test('source chart route reconstructs the jitter chart', async () => {
    const restore = setupGlobals();
    try {
        const routes = createSourceRoutes(baseDeps());
        const comp = routes['/source/:sourceName/chart/:chartId'].onmatch({
            sourceName: 'hub', chartId: 'source-timestamp-jitter',
        });
        await comp.ready;
        const scv = findByTag(comp.view(), 'SingleChartView');
        assert.ok(scv, 'expected jitter chart to reconstruct');
        assert.equal(scv.attrs.data.groups[0].subgroups[0].plots[0].opts.id, 'source-timestamp-jitter');
    } finally { restore(); }
});

test('source chart route reports not-found for an unknown chart id', async () => {
    const restore = setupGlobals();
    try {
        const routes = createSourceRoutes(baseDeps());
        const comp = routes['/source/:sourceName/chart/:chartId'].onmatch({
            sourceName: 'hub',
            chartId: 'source-metric-does_not_exist',
        });
        await comp.ready;

        const v = comp.view();
        assert.equal(findByTag(v, 'SingleChartView'), null, 'no chart for a stale id');
        // TopNav still renders so the user can navigate away.
        assert.ok(findByTag(v, 'TopNav'), 'expected TopNav in the error view');
    } finally {
        restore();
    }
});

test('source section route hands an empty group list to Main', async () => {
    const restore = setupGlobals();
    try {
        const routes = createSourceRoutes(baseDeps());
        const result = routes['/source/:sourceName'].onmatch(
            { sourceName: 'hub' },
            '/source/hub',
        );
        const v = result.view();
        assert.equal(v.tag, 'Main');
        assert.equal(v.attrs.activeSection.route, '/source/hub');
        assert.deepEqual(v.attrs.groups, []);
    } finally {
        restore();
    }
});
