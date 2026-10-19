import assert from 'node:assert/strict';
import test from 'node:test';

// section_views.js imports chart_controls.js, which reads CSS custom
// properties at module load; stub the browser globals first (see
// metric_browser_timestamp.test.mjs).
globalThis.getComputedStyle = () => ({ getPropertyValue: () => '' });
if (typeof globalThis.document === 'undefined') {
    globalThis.document = { documentElement: {}, body: {} };
}

const { collectServiceQueries } = await import('../src/viewer/assets/lib/sections/section_views.js');

test('a combined file\'s templates are read from each source\'s entries', () => {
    const widgets = { service_name: 'widgetd', kpis: [] };
    const gadgets = { service_name: 'gadgetd', kpis: [] };
    const meta = {
        source: ['gadgetd', 'widgetd'],
        per_source_metadata: {
            widgetd: { 0: { service_queries: widgets }, descriptions: { m: 'help' } },
            gadgetd: { 0: { version: '1' }, 1: { service_queries: gadgets } },
            rezolus: { web01: { node: 'web01' } },
        },
    };
    assert.deepEqual(collectServiceQueries(meta), [
        { source: 'widgetd', queries: widgets },
        { source: 'gadgetd', queries: gadgets },
    ]);
});

test('a single-source file\'s top-level template is used when no source has one', () => {
    const t = { service_name: 'svc', kpis: [] };
    assert.deepEqual(collectServiceQueries({ source: 'svc', service_queries: t }), [
        { source: 'svc', queries: t },
    ]);
    assert.deepEqual(collectServiceQueries({}), []);
});
