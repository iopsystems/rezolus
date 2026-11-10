// metriken-recorder 0.12 stores a Prometheus series whose name ends in a
// unit token under a normalized name, with the exporter's name in the
// internal label `__prometheus_name__` (`rpc_seconds` stored as
// `rpc_nanoseconds`). The server sends labels in key order, where `__`
// sorts before every letter, so a legend that took the first label would
// print the exporter's name. Every legend and compare key here is what it
// is for the same series without the label.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { applyDisplayToPlot, applyResultToPlot } from '../src/viewer/assets/lib/data.js';
import { composeScatterLabel } from '../src/viewer/assets/lib/compare/compare_math.js';

const renamed = (labels, exporter = 'rpc_seconds') => ({
    __name__: 'rpc_nanoseconds',
    __prometheus_name__: exporter,
    ...labels,
});

const without = (metric) => {
    const { __prometheus_name__, ...rest } = metric;
    return rest;
};

const matrixNames = (metrics) => {
    const plot = { opts: { style: 'multi' } };
    applyResultToPlot(plot, {
        status: 'success',
        data: {
            resultType: 'matrix',
            result: metrics.map((metric) => ({ metric, values: [[1, '1'], [2, '2']] })),
        },
    });
    return plot.series_names;
};

const displayNames = (metrics) => {
    const plot = { opts: { style: 'multi' } };
    applyDisplayToPlot(plot, {
        series: metrics.map((metric) => ({
            metric,
            t: new Float64Array([1, 2]),
            median: new Float64Array([1, 2]),
        })),
    });
    return plot.series_names;
};

test('a lone renamed series is not named by its exporter name', () => {
    const metric = renamed({ alias: 'svc' });
    for (const names of [matrixNames, displayNames]) {
        assert.deepEqual(names([metric]), ['svc']);
        assert.deepEqual(names([metric]), names([without(metric)]));
    }
});

test('renamed series are named by the visible labels that differ', () => {
    // Two exporter names, one stored name: the label differs between the
    // series and still names neither.
    const metrics = [
        renamed({ alias: 'svc', op: 'get' }),
        renamed({ alias: 'svc', op: 'put' }, 'rpc_ms'),
    ];
    for (const names of [matrixNames, displayNames]) {
        assert.deepEqual(names(metrics), ['get', 'put']);
    }
});

test("a compare key ignores the exporter's name, so a renamed series pairs with an older one", () => {
    const older = { __name__: 'rpc_seconds', quantile: '0.99', alias: 'svc' };
    const newer = renamed({ quantile: '0.99', alias: 'svc' });
    assert.equal(composeScatterLabel(newer), 'p99 · svc');
    assert.equal(composeScatterLabel(newer), composeScatterLabel(older));
    assert.equal(composeScatterLabel(renamed({})), null);
});
