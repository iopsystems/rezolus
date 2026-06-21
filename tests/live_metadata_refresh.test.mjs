import test from 'node:test';
import assert from 'node:assert/strict';
import { createDataApi } from '../src/viewer/assets/lib/data.js';

// Live-mode auto-refresh advances the query window with the growing TSDB.
// processDashboardData accepts a `freshMetadata` option that bypasses
// cachedMetadata so the next query uses the current maxTime, not the
// frozen one from initial page load.

const makeSectionPayload = () => ({
    groups: [
        {
            name: 'g',
            subgroups: [
                {
                    name: 'sg',
                    plots: [{ promql_query: 'm', opts: { type: 'gauge' } }],
                },
            ],
        },
    ],
});

const matrixOk = {
    status: 'success',
    data: {
        resultType: 'matrix',
        result: [{ metric: { __name__: 'm' }, values: [[0, '1'], [1, '2']] }],
    },
};

const makeApi = (metaSeq, queryRangeCalls) => createDataApi({
    getMetadata: async () => {
        const next = metaSeq.shift() ?? metaSeq[metaSeq.length - 1];
        return next ?? { status: 'success', data: { minTime: 0, maxTime: 0 } };
    },
    queryRange: async (query, start, end, step) => {
        queryRangeCalls.push({ query, start, end, step });
        return matrixOk;
    },
    logHeatmapErrors: false,
});

test('processDashboardData: default behavior caches metadata across calls', async () => {
    const metas = [
        { status: 'success', data: { minTime: 1000, maxTime: 2000 } },
        { status: 'success', data: { minTime: 1000, maxTime: 3000 } },
    ];
    const calls = [];
    const api = makeApi(metas, calls);

    await api.processDashboardData(makeSectionPayload(), null, '/cpu');
    await api.processDashboardData(makeSectionPayload(), null, '/cpu');

    assert.equal(calls.length, 2, 'two queryRange calls (one per processDashboardData)');
    assert.equal(calls[0].end, 2000, 'first call uses initial maxTime');
    assert.equal(calls[1].end, 2000, 'second call reuses cached maxTime (existing behavior)');
});

test('processDashboardData: { freshMetadata: true } refetches metadata, advances query window', async () => {
    const metas = [
        { status: 'success', data: { minTime: 1000, maxTime: 2000 } },
        { status: 'success', data: { minTime: 1000, maxTime: 3000 } },
    ];
    const calls = [];
    const api = makeApi(metas, calls);

    await api.processDashboardData(makeSectionPayload(), null, '/cpu');
    await api.processDashboardData(makeSectionPayload(), null, '/cpu', { freshMetadata: true });

    assert.equal(calls.length, 2);
    assert.equal(calls[0].end, 2000, 'first call uses initial maxTime');
    assert.equal(calls[1].end, 3000, 'second call (with freshMetadata) uses new maxTime');
});
