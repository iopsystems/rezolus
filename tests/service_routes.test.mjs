import test from 'node:test';
import assert from 'node:assert/strict';
import { createServiceRoutes } from '../src/viewer/assets/lib/service.js';

const setupGlobals = (overrides = {}) => {
    const previousM = globalThis.m;
    const previousWindow = globalThis.window;

    globalThis.m = (tag, attrs, children) => ({ tag, attrs, children });
    globalThis.m.route = {
        get: () => '/overview',
        set: overrides.routeSet || (() => {}),
    };
    globalThis.window = {
        scrollTo() {},
    };

    return () => {
        globalThis.m = previousM;
        globalThis.window = previousWindow;
    };
};

const baseDeps = (sectionResponseCache) => ({
    sectionResponseCache,
    loadSection: async () => null,
    preloadSections: () => {},
    chartsState: { charts: new Map() },
    Main: 'Main',
    TopNav: 'TopNav',
    topNavAttrs: (data, route, extra) => ({ data, route, extra }),
    SingleChartView: 'SingleChartView',
    applyResultToPlot: () => {},
    getCompareMode: () => false,
});

test('service section route hydrates lean cached payloads with shared sections', async () => {
    const restore = setupGlobals();
    const sections = [
        { name: 'Overview', route: '/overview' },
        { name: 'API', route: '/service/api' },
    ];

    try {
        const routes = createServiceRoutes({
            ...baseDeps({
                'service/api': {
                    groups: [{ id: 'latency' }],
                    interval: 1,
                    metadata: {},
                },
            }),
            getSections: () => sections,
            withSharedSections: (data) => ({ ...data, sections }),
        });

        const view = await routes['/service/:serviceName'].onmatch(
            { serviceName: 'api' },
            '/service/api',
        );
        const vnode = view.view();

        assert.equal(vnode.tag, 'Main');
        assert.deepEqual(vnode.attrs.sections, sections);
        assert.equal(vnode.attrs.activeSection.route, '/service/api');
    } finally {
        restore();
    }
});

test('service section route redirects to default when section is missing', async () => {
    const setRouteCalls = [];
    const previousWarn = console.warn;
    console.warn = () => {};
    const restore = setupGlobals({ routeSet: (target) => setRouteCalls.push(target) });

    try {
        const routes = createServiceRoutes({
            ...baseDeps({}),
            loadSection: async () => { throw new Error('Unknown section: service/llm-perf'); },
            getDefaultRoute: () => '/service/vllm',
        });

        const result = routes['/service/:serviceName'].onmatch(
            { serviceName: 'llm-perf' },
            '/service/llm-perf',
        );

        // recoverFromMissingSection returns a never-resolving promise; race
        // it against a microtask tick so we can assert on the redirect
        // without blocking the test.
        await Promise.race([result, new Promise((r) => setTimeout(r, 10))]);

        assert.deepEqual(setRouteCalls, ['/service/vllm']);
    } finally {
        restore();
        console.warn = previousWarn;
    }
});

test('service section route falls back to /overview when default route equals the failing route', async () => {
    // Repro for the redirect-loop bug: when the dashboard's default
    // route points at a section that isn't in dashboard_sections (can
    // happen when serviceInstances and section keys disagree, e.g.
    // compare-mode-without-category aliasing), we'd otherwise bounce
    // between the broken route and itself indefinitely. Mithril's
    // m.route.get() returns the last successfully resolved path;
    // simulate an initial load (no route resolved yet) by returning
    // undefined.
    const setRouteCalls = [];
    const previousWarn = console.warn;
    const previousM = globalThis.m;
    const previousWindow = globalThis.window;
    console.warn = () => {};
    globalThis.m = (tag, attrs, children) => ({ tag, attrs, children });
    globalThis.m.route = {
        get: () => undefined,
        set: (target) => setRouteCalls.push(target),
    };
    globalThis.window = { scrollTo() {} };

    try {
        const routes = createServiceRoutes({
            ...baseDeps({}),
            loadSection: async () => { throw new Error('Unknown section: service/llm-perf'); },
            getDefaultRoute: () => '/service/llm-perf',
        });

        const result = routes['/service/:serviceName'].onmatch(
            { serviceName: 'llm-perf' },
            '/service/llm-perf',
        );

        await Promise.race([result, new Promise((r) => setTimeout(r, 10))]);

        assert.deepEqual(setRouteCalls, ['/overview']);
    } finally {
        globalThis.m = previousM;
        globalThis.window = previousWindow;
        console.warn = previousWarn;
    }
});

test('service section route falls back to embedded sections when shared-section helpers are absent', async () => {
    const restore = setupGlobals();
    const sections = [
        { name: 'Overview', route: '/overview' },
        { name: 'API', route: '/service/api' },
    ];

    try {
        const routes = createServiceRoutes(baseDeps({
            'service/api': {
                groups: [{ id: 'latency' }],
                interval: 1,
                metadata: {},
                sections,
            },
        }));

        const view = await routes['/service/:serviceName'].onmatch(
            { serviceName: 'api' },
            '/service/api',
        );
        const vnode = view.view();

        assert.equal(vnode.tag, 'Main');
        assert.deepEqual(vnode.attrs.sections, sections);
        assert.equal(vnode.attrs.activeSection.route, '/service/api');
    } finally {
        restore();
    }
});
