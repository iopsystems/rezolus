// Guards which plots are routed through display mode (median + bands). The
// regression this pins: per-entity charts that group `by (id)` render as
// heatmaps (one row per CPU/GPU/...), and display mode — which collapses each
// series to a median + bands — cannot represent a heatmap. Those must stay on
// the native render path. Percentile scatters and aggregate/multi line charts
// do use display.
import { test } from 'node:test';
import assert from 'node:assert';
import { plotUsesDisplay } from '../src/viewer/assets/lib/data.js';

const p = (query, opts = {}) => ({ promql_query: query, opts });

test('per-entity (by id) counter/gauge charts stay off display (heatmap)', () => {
    for (const q of [
        'sum by (id) (irate(cpu_usage[5m])) / 1000000000',
        'sum by (id) (gpu_clock{clock="graphics"})',
        'sum by (id) (irate(scheduler_runqueue_wait[5m]))',
        'sum by ( id ) (x)',
        'by(id)(x)',
        'sum by (id, state) (irate(cpu_usage[5m]))',
    ]) {
        assert.equal(plotUsesDisplay(p(q, { type: 'delta_counter' })), false, q);
    }
});

test('aggregate / multi line charts use display', () => {
    for (const q of [
        'sum(irate(cpu_usage[5m])) / cpu_cores / 1000000000',
        'avg by (state) (cpu_usage)',
        'rate(grid_metric[5m])',     // "grid" must not trip the id match
        'sum by (width) (x)',        // "width" must not trip the id match
    ]) {
        assert.equal(plotUsesDisplay(p(q, { type: 'delta_counter' })), true, q);
    }
});

test('percentile histograms use display; bucket/quantile heatmaps do not', () => {
    assert.equal(plotUsesDisplay(p('histogram_quantiles(x, "p50")', { type: 'histogram', subtype: 'percentiles' })), true);
    assert.equal(plotUsesDisplay(p('histogram_heatmap(x)', { type: 'histogram', subtype: 'buckets' })), false);
    assert.equal(plotUsesDisplay(p('x', { type: 'histogram', subtype: 'quantile_heatmap' })), false);
});

test('plots without a query never use display', () => {
    assert.equal(plotUsesDisplay({ opts: { type: 'delta_counter' } }), false);
    assert.equal(plotUsesDisplay(null), false);
});
