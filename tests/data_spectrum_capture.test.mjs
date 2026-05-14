import test from 'node:test';
import assert from 'node:assert/strict';
import { createDataApi, CAPTURE_BASELINE, CAPTURE_EXPERIMENT } from '../src/viewer/assets/lib/data.js';

// Minimal mock: returns a successful PromQL matrix result with one
// series so fetchQuantileSpectrumForPlot's parsing path completes.
// Records call args on `calls` for assertions.
function makeApi(calls, opts = {}) {
    return createDataApi({
        getMetadata: async () => ({
            status: 'success',
            data: { minTime: opts.minTime ?? 1000, maxTime: opts.maxTime ?? 2000 },
        }),
        queryRange: async (query, start, end, step, captureId = 'baseline') => {
            calls.push({ query, start, end, step, captureId });
            return {
                status: 'success',
                data: {
                    resultType: 'matrix',
                    result: [
                        // p0 series — anchors color_min, then gets stripped
                        { metric: { __name__: 'm', quantile: '0' }, values: [[start, '1'], [end, '2']] },
                        // p50 series
                        { metric: { __name__: 'm', quantile: '0.5' }, values: [[start, '5'], [end, '10']] },
                    ],
                },
            };
        },
        logHeatmapErrors: false,
    });
}

const plot = {
    promql_query: 'tcp_packet_latency',
    opts: { type: 'histogram' },
};

test('fetchQuantileSpectrumForPlot: defaults to baseline capture', async () => {
    const calls = [];
    const api = makeApi(calls);
    const r = await api.fetchQuantileSpectrumForPlot(plot, [0.5]);
    assert.ok(r);
    assert.equal(calls.length, 1);
    assert.equal(calls[0].captureId, CAPTURE_BASELINE);
});

test('fetchQuantileSpectrumForPlot: routes to experiment capture when captureId set', async () => {
    const calls = [];
    const api = makeApi(calls);
    const r = await api.fetchQuantileSpectrumForPlot(plot, [0.5], CAPTURE_EXPERIMENT);
    assert.ok(r);
    assert.equal(calls.length, 1);
    assert.equal(calls[0].captureId, CAPTURE_EXPERIMENT);
});

test('fetchQuantileSpectrumForPlot: passes caller-supplied range to experiment query', async () => {
    const calls = [];
    const api = makeApi(calls);
    const range = { start: 5000, end: 6000, step: 5 };
    const r = await api.fetchQuantileSpectrumForPlot(plot, [0.5], CAPTURE_EXPERIMENT, range);
    assert.ok(r);
    assert.equal(calls.length, 1);
    assert.equal(calls[0].start, 5000);
    assert.equal(calls[0].end, 6000);
    assert.equal(calls[0].step, 5);
    assert.equal(calls[0].captureId, CAPTURE_EXPERIMENT);
});

test('fetchQuantileSpectrumForPlot: experiment without range falls back to baseline metadata', async () => {
    const calls = [];
    const api = makeApi(calls, { minTime: 100, maxTime: 200 });
    const r = await api.fetchQuantileSpectrumForPlot(plot, [0.5], CAPTURE_EXPERIMENT);
    assert.ok(r);
    assert.equal(calls.length, 1);
    assert.equal(calls[0].captureId, CAPTURE_EXPERIMENT);
    // Range should derive from baseline meta: end = maxTime (200),
    // start = max(minTime, maxTime - 3600) = 100 (since duration < 3600).
    assert.equal(calls[0].end, 200);
    assert.equal(calls[0].start, 100);
});

test('fetchQuantileSpectrumForPlot: prepends q=0 to the queried quantiles', async () => {
    const calls = [];
    const api = makeApi(calls);
    await api.fetchQuantileSpectrumForPlot(plot, [0.5, 0.99]);
    // Query should reference [0, 0.5, 0.99] inside histogram_quantiles(...).
    const query = calls[0].query;
    assert.match(query, /histogram_quantiles\(\[\s*0\s*,\s*0\.5\s*,\s*0\.99\s*\]/);
});
