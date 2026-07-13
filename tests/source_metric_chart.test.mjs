// Bug 1: expanding a chart in simple-metric (foreign source) mode was dead —
// the expand link builds `#/source/<name>/chart/<id>`, a 4-segment route that
// matched nothing, and even if routed there was no server-rendered section to
// resolve the plot from. The single-chart route reconstructs the chart from the
// URL by re-deriving the spec from the metric catalog. That only works if the
// expand link and the resolver agree on `opts.id` and on how each metric type
// maps to a query — this pins that shared contract.
import assert from 'node:assert/strict';
import test from 'node:test';
import {
    sourceMetricChartId,
    specForSourceMetric,
    specForChartId,
} from '../src/viewer/assets/lib/charts/source_metric.js';

test('chart id convention is source-metric-<name>', () => {
    assert.equal(sourceMetricChartId('hub_heartbeat'), 'source-metric-hub_heartbeat');
});

test('counter spec: rate() query, stable id, carried metadata', () => {
    const spec = specForSourceMetric({
        name: 'hub_heartbeat',
        metric_type: 'counter',
        description: 'beats',
    });
    assert.equal(spec.opts.id, 'source-metric-hub_heartbeat');
    assert.equal(spec.opts.type, 'counter');
    assert.equal(spec.opts.title, 'hub_heartbeat');
    assert.equal(spec.opts.description, 'beats');
    assert.equal(spec.promql_query, 'rate(hub_heartbeat[5m])');
});

test('gauge spec: raw query, empty description default', () => {
    const spec = specForSourceMetric({ name: 'queue_depth', metric_type: 'gauge' });
    assert.equal(spec.promql_query, 'queue_depth');
    assert.equal(spec.opts.description, '');
});

test('histogram spec: RAW metric + percentiles subtype (pipeline wraps once)', () => {
    // Must be the bare name, NOT histogram_quantiles(...) — the section
    // pipeline wraps histograms itself; a pre-wrapped query double-wraps.
    const spec = specForSourceMetric({ name: 'latency', metric_type: 'histogram' });
    assert.equal(spec.promql_query, 'latency');
    assert.equal(spec.opts.subtype, 'percentiles');
    assert.equal(spec.opts.type, 'histogram');
});

test('specForChartId round-trips the expand-link id back to the same spec', () => {
    const catalog = [
        { name: 'hub_heartbeat', metric_type: 'counter' },
        { name: 'queue_depth', metric_type: 'gauge' },
        { name: 'latency', metric_type: 'histogram' },
    ];
    for (const info of catalog) {
        const built = specForSourceMetric(info);
        const resolved = specForChartId(built.opts.id, catalog);
        assert.deepEqual(resolved, built);
    }
});

test('specForChartId returns null for unknown or stale ids', () => {
    const catalog = [{ name: 'hub_heartbeat', metric_type: 'counter' }];
    assert.equal(specForChartId('source-metric-does_not_exist', catalog), null);
    assert.equal(specForChartId('source-metric-hub_heartbeat', []), null);
    assert.equal(specForChartId('source-metric-hub_heartbeat', undefined), null);
});
