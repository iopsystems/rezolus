// An internal label (`__uid__`, which the agent puts on a slot's labels) is
// part of a series' identity and never names it. The server sends labels in
// key order, where `__` sorts before every letter, so a legend that took the
// first label would print it. Every legend and compare key here is what it
// is for the same series without the label.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { applyDisplayToPlot, applyResultToPlot } from '../src/viewer/assets/lib/data.js';
import { composeScatterLabel } from '../src/viewer/assets/lib/compare/compare_math.js';

const tagged = (labels, uid = '7') => ({
    __name__: 'rpc_seconds',
    __uid__: uid,
    ...labels,
});

const without = (metric) => {
    const { __uid__, ...rest } = metric;
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

test('a lone series is not named by an internal label', () => {
    const metric = tagged({ alias: 'svc' });
    for (const names of [matrixNames, displayNames]) {
        assert.deepEqual(names([metric]), ['svc']);
        assert.deepEqual(names([metric]), names([without(metric)]));
    }
});

test('series are named by the visible labels that differ', () => {
    // The internal label differs between the series and still names neither.
    const metrics = [
        tagged({ alias: 'svc', op: 'get' }),
        tagged({ alias: 'svc', op: 'put' }, '8'),
    ];
    for (const names of [matrixNames, displayNames]) {
        assert.deepEqual(names(metrics), ['get', 'put']);
    }
});

test('a compare key ignores internal labels, so a series pairs with one that lacks them', () => {
    const plain = { __name__: 'rpc_seconds', quantile: '0.99', alias: 'svc' };
    const withUid = tagged({ quantile: '0.99', alias: 'svc' });
    assert.equal(composeScatterLabel(withUid), 'p99 · svc');
    assert.equal(composeScatterLabel(withUid), composeScatterLabel(plain));
    assert.equal(composeScatterLabel(tagged({})), null);
});
