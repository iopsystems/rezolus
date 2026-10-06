// How the dashboard fetches a per-entity heatmap (a `by (id)` panel), through
// the real `processDashboardData` with the range query stubbed: a rate at the
// heatmap step in grid mode with each value at its column's start, a line
// that averages over `by (id)` not as a heatmap, and a gauge heatmap whose
// display fetch fails falling back to the full matrix.
import { test } from 'node:test';
import assert from 'node:assert';
import {
    createDataApi, setDisplayMode, setRateMode, setStepOverride,
} from '../src/viewer/assets/lib/data.js';

const meta = { minTime: 0, maxTime: 34_677, interval: 1 };

const api = (calls) => createDataApi({
    getMetadata: async () => ({ status: 'success', data: meta }),
    queryRange: async (query, start, end, step, captureId, signal, rateMode) => {
        calls.push({ query, start, end, step, rateMode });
        // Two CPUs, each two values stamped at the end of the steps they
        // average.
        const values = [[step, '1'], [2 * step, '2']];
        return {
            status: 'success',
            data: {
                resultType: 'matrix',
                result: [{ metric: { id: '0' }, values }, { metric: { id: '1' }, values }],
            },
        };
    },
});

// A section of one chart, and the chart, which the fetch fills in.
const section = (query) => {
    const plot = { promql_query: query, opts: { type: 'delta_counter' } };
    return { data: { groups: [{ name: 'g', subgroups: [{ plots: [plot] }] }] }, plot };
};

test('a rate heatmap is fetched at the heatmap step in grid mode, at column starts', async () => {
    setDisplayMode(true);
    setStepOverride(null);
    setRateMode('raw');
    try {
        const calls = [];
        const { data, plot } = section('sum by (id) (irate(cpu_usage[5m])) / 1000000000');
        await api(calls).processDashboardData(data, null, '/cpu');
        assert.strictEqual(calls.length, 1);
        // In node the chart width is 500 px, so 63 buckets: 550 s rounds to 600 s.
        assert.strictEqual(calls[0].step, 600);
        assert.strictEqual(calls[0].rateMode, 'grid');
        assert.deepStrictEqual(plot.time_data, [0, 600]);
    } finally {
        setRateMode('grid');
    }
});

test('a line averaging over by (id) is not fetched as a heatmap', async () => {
    setDisplayMode(true);
    setStepOverride(null);
    const calls = [];
    const q = 'avg(sum by (id) (irate(core_cstate_residency[5m])) / sum by (id) (irate(cpu_tsc[5m])))';
    await api(calls).processDashboardData(section(q).data, null, '/cpu');
    // Display mode has no server here, so it falls back to the native step.
    assert.ok(calls.every((c) => c.step !== 600), JSON.stringify(calls));
});

test('a gauge heatmap whose display fetch fails falls back to the full matrix', async () => {
    setDisplayMode(true);
    setStepOverride(null);
    const calls = [];
    const { data, plot } = section('sum by (id, vendor) (gpu_utilization) / 100');
    await api(calls).processDashboardData(data, null, '/gpu');
    assert.strictEqual(calls.length, 1);
    assert.strictEqual(calls[0].step, 1);
    assert.ok(plot.data.length > 0, 'the panel is not blank');
});
