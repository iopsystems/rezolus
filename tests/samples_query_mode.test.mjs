// The single-chart view's samples toggle runs its query in raw mode; every
// other range query runs on the step grid.
import { test } from 'node:test';
import assert from 'node:assert';
import { createDataApi } from '../src/viewer/assets/lib/data.js';

const meta = { minTime: 0, maxTime: 600, interval: 1 };

const api = (calls) => createDataApi({
    getMetadata: async () => ({ status: 'success', data: meta }),
    queryRange: async (query, start, end, step, captureId, signal, rateMode) => {
        calls.push({ query, rateMode });
        return { status: 'success', data: { resultType: 'matrix', result: [] } };
    },
});

test('a range query runs on the grid by default', async () => {
    const calls = [];
    await api(calls).executePromQLRangeQuery('irate(x[5m])', meta);
    assert.deepStrictEqual(calls, [{ query: 'irate(x[5m])', rateMode: 'grid' }]);
});

test('a range query asked for samples runs in raw mode', async () => {
    const calls = [];
    await api(calls).executePromQLRangeQuery('irate(x[5m])', meta, undefined, 'raw');
    assert.deepStrictEqual(calls, [{ query: 'irate(x[5m])', rateMode: 'raw' }]);
});
