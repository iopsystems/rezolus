import test from 'node:test';
import assert from 'node:assert/strict';
import {
    createDataApi,
    nativeInterval,
    stepAtLeast,
    setStepOverride,
} from '../src/viewer/assets/lib/data.js';
import { granularityOptions } from '../src/viewer/assets/lib/ui/controls.js';

// A recording made at `--interval 100ms` has ten samples per second. The
// frontend used to floor every query step and histogram stride at one second,
// which threw nine of every ten away: a 2 Hz signal in such a recording came
// back as a flat line at its mean. There is no 1s floor anywhere in the query
// path — the engine takes `step` as f64 seconds and works in nanoseconds.

test('nativeInterval: sub-second cadence survives, absent metadata falls back to 1s', () => {
    assert.equal(nativeInterval({ interval: 0.1 }), 0.1);
    assert.equal(nativeInterval({ interval: 5 }), 5);
    assert.equal(nativeInterval({ interval: 0 }), 1);
    assert.equal(nativeInterval({}), 1);
    assert.equal(nativeInterval(null), 1);
});

test('stepAtLeast: never finer than the cadence, quantized to whole ticks', () => {
    // A target below the cadence buys interpolation, not resolution.
    assert.equal(stepAtLeast(0.1, 0.02), 0.1);
    assert.equal(stepAtLeast(1, 0.4), 1);
    // A coarser target lands on a whole multiple of the cadence.
    assert.equal(stepAtLeast(0.1, 0.25), 0.3);
    assert.equal(stepAtLeast(1, 7.2), 8);
    // And without the binary-float tail that would otherwise reach the server
    // in a query string (0.1 * 3 = 0.30000000000000004).
    assert.ok(String(stepAtLeast(0.1, 0.25)).length < 5, 'no float tail in the step');
});

function makeApi(calls, { minTime, maxTime, interval }) {
    return createDataApi({
        getMetadata: async () => ({
            status: 'success',
            data: { minTime, maxTime, interval },
        }),
        queryRange: async (query, start, end, step, captureId = 'baseline') => {
            calls.push({ query, start, end, step, captureId });
            return { status: 'success', data: { resultType: 'matrix', result: [] } };
        },
        logHeatmapErrors: false,
    });
}

test('a 100ms recording is queried at 100ms, not at 1s', async () => {
    const minTime = 1_700_000_000;
    const calls = [];
    const api = makeApi(calls, { minTime, maxTime: minTime + 30, interval: 0.1 });

    await api.executePromQLRangeQuery('irate(demo_events_total[1m])');
    assert.equal(calls[0].step, 0.1);
});

test('histogram heatmap strides at the cadence, not at a 1s floor', async () => {
    const minTime = 1_700_000_000;
    const calls = [];
    // 30s of a 100ms recording is 300 samples — fewer than the pixel budget,
    // so every sample gets its own column and no stride argument is sent. With
    // the old 1s floor this asked for a 1s stride and drew 30 columns.
    const api = makeApi(calls, { minTime, maxTime: minTime + 30, interval: 0.1 });

    await api.fetchHeatmapForPlot({
        promql_query: 'latency',
        opts: { type: 'histogram', subtype: 'buckets' },
    });
    assert.equal(calls[0].query, 'histogram_heatmap(latency)', 'no stride at native resolution');
});

test('a step override coarser than the cadence still strides a sub-second recording', async () => {
    const minTime = 1_700_000_000;
    const calls = [];
    const api = makeApi(calls, { minTime, maxTime: minTime + 30, interval: 0.1 });

    // 1s is a real 10x stride here. The old `stepOverride > 1` gate compared
    // against one second rather than the recording's own cadence, so this
    // override reached neither the stride nor the query.
    setStepOverride(1);
    try {
        await api.fetchHeatmapForPlot({
            promql_query: 'latency',
            opts: { type: 'histogram', subtype: 'buckets' },
        });
        assert.equal(calls[0].query, 'histogram_heatmap(latency, 1)');
    } finally {
        setStepOverride(null);
    }
});

test('granularity options start at the recording cadence', () => {
    const labels = (interval) => granularityOptions(interval).map((o) => o.label);

    // A 1s recording keeps exactly the choices it has always had.
    assert.deepEqual(labels(1), ['Auto', '1s', '5s', '15s', '1m']);
    // A sub-second recording gets the steps that can actually resolve it.
    assert.deepEqual(labels(0.1), ['Auto', '100ms', '250ms', '500ms', '1s', '5s', '15s', '1m']);
    // A cadence off the ladder is offered as itself.
    assert.deepEqual(labels(2), ['Auto', '2s', '5s', '15s', '1m']);
    // Nothing finer than the recording: a 5s recording cannot resolve 1s.
    assert.deepEqual(labels(5), ['Auto', '5s', '15s', '1m']);
});

test('granularity values parse back as numbers, including sub-second', () => {
    for (const opt of granularityOptions(0.1)) {
        if (opt.value === '') continue;
        assert.ok(parseFloat(opt.value) > 0, `${opt.label} parses to a positive step`);
    }
    // parseInt, the previous parser, truncates every sub-second option to 0.
    assert.equal(parseInt('0.1', 10), 0);
});
