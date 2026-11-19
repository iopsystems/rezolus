// A plot of a combined archive carries `scope`, the uuids of the sources its
// queries read. Through the real `processDashboardData` and
// `fetchHeatmapForPlot`, with the range query stubbed: each query is sent
// with its plot's scope, and a plot without one reads every source.
import { test } from 'node:test';
import assert from 'node:assert';
import {
    createDataApi, setDisplayMode, setStepOverride,
} from '../src/viewer/assets/lib/data.js';

const meta = { minTime: 0, maxTime: 60, interval: 1 };

const api = (calls) => createDataApi({
    getMetadata: async () => ({ status: 'success', data: meta }),
    queryRange: async (query, start, end, step, captureId, signal, rateMode, scope) => {
        calls.push({ query, scope });
        return {
            status: 'success',
            data: { resultType: 'matrix', result: [{ metric: {}, values: [[1, '1']] }] },
        };
    },
});

const section = (plots) => ({ groups: [{ name: 'g', subgroups: [{ plots }] }] });

test('each plot is queried with its own scope', async () => {
    setDisplayMode(false);
    setStepOverride(null);
    const calls = [];
    const data = section([
        { promql_query: 'sum(irate(cpu_usage[5m]))', opts: { type: 'delta_counter' }, scope: ['u1'] },
        { promql_query: 'sum(irate(svc_requests[5m]))', opts: { type: 'delta_counter' }, scope: ['u1', 'u2'] },
        { promql_query: 'cpu_cores', opts: { type: 'gauge' } },
    ]);
    await api(calls).processDashboardData(data, null, '/overview');
    const byQuery = Object.fromEntries(calls.map((c) => [c.query, c.scope]));
    assert.deepStrictEqual(byQuery, {
        'sum(irate(cpu_usage[5m]))': ['u1'],
        'sum(irate(svc_requests[5m]))': ['u1', 'u2'],
        cpu_cores: null,
    });
});

test('an entity heatmap is queried with its plot scope', async () => {
    setDisplayMode(true);
    setStepOverride(null);
    const calls = [];
    const plot = {
        promql_query: 'sum by (id) (irate(cpu_usage[5m]))',
        opts: { type: 'delta_counter' },
        scope: ['u1'],
    };
    await api(calls).processDashboardData(section([plot]), null, '/cpu');
    assert.ok(calls.length > 0);
    assert.ok(calls.every((c) => c.scope?.[0] === 'u1'), JSON.stringify(calls));
    setDisplayMode(false);
});

test('a histogram heatmap is queried with its plot scope', async () => {
    const calls = [];
    const plot = {
        promql_query: 'syscall_latency',
        opts: { type: 'histogram', subtype: 'heatmap' },
        scope: ['u2'],
    };
    // The display fetch has no server here and falls back to the range
    // query.
    await api(calls).fetchHeatmapForPlot(plot);
    assert.deepStrictEqual(calls.map((c) => c.scope), [['u2']]);
});
