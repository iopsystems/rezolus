import test from 'node:test';
import assert from 'node:assert/strict';
import { createDataApi, defaultRangeFor } from '../src/viewer/assets/lib/data.js';

// Locks in the overview-query contract: the WHOLE recording at its
// NATIVE sampling interval. We deliberately do NOT decimate by widening
// the PromQL step — measured behavior is that histogram queries ignore
// `step` entirely (their resolution is the `stride` arg) so a coarse
// step bounds nothing for them, while counters honor `step` but need
// their rate window rewritten to match. Widening step therefore corrupts
// queries (mismatched per-type resolution, skipped rewrites) without
// reliably bounding payload — which showed up as charts not rendering
// until zoom. Decimation for display is echarts' LTTB (client-side); the
// real payload bound is a server-side post-evaluation reducer, tracked
// separately. The Granularity selector (_stepOverride) still wins.

function makeApi(calls, { minTime, maxTime, interval = 1 }) {
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

test('executePromQLRangeQuery: full range at native step (1s recording)', async () => {
    const minTime = 1_700_000_000;
    const maxTime = minTime + 86_400; // 24h @ 1s
    const calls = [];
    const api = makeApi(calls, { minTime, maxTime, interval: 1 });

    await api.executePromQLRangeQuery('cpu_usage');

    assert.equal(calls.length, 1);
    const { start, end, step } = calls[0];
    assert.equal(start, minTime, 'full range start = minTime (no trailing-window cap)');
    assert.equal(end, maxTime);
    assert.equal(step, 1, 'native step (1s) regardless of recording length — no step decimation');
});

test('executePromQLRangeQuery: step follows meta.interval (5s sampling)', async () => {
    const minTime = 1_700_000_000;
    const maxTime = minTime + 3_600;
    const calls = [];
    const api = makeApi(calls, { minTime, maxTime, interval: 5 });

    await api.executePromQLRangeQuery('cpu_usage');
    assert.equal(calls[0].step, 5, 'step = native interval (5s)');
});

test('executePromQLRangeQuery: sub-second interval clamps to a 1s step floor', async () => {
    // metriken itself floors step at interval().max(1.0); mirror that.
    const minTime = 1_700_000_000;
    const maxTime = minTime + 600;
    const calls = [];
    const api = makeApi(calls, { minTime, maxTime, interval: 0.1 });

    await api.executePromQLRangeQuery('cpu_usage');
    assert.equal(calls[0].step, 1, 'sub-second interval rounds up to a 1s step');
});

test('executePromQLRangeQuery: step override (Granularity selector) wins', async () => {
    const minTime = 1_700_000_000;
    const maxTime = minTime + 86_400;
    const calls = [];
    const api = makeApi(calls, { minTime, maxTime, interval: 1 });

    const { setStepOverride } = await import('../src/viewer/assets/lib/data.js');
    setStepOverride(60);
    try {
        await api.executePromQLRangeQuery('cpu_usage');
        assert.equal(calls[0].step, 60, 'user step override wins');
    } finally {
        setStepOverride(null);
    }
});

test('executePromQLRangeQuery: missing interval falls back to a 1s step', async () => {
    const minTime = 1_700_000_000;
    const maxTime = minTime + 600;
    const calls = [];
    const api = makeApi(calls, { minTime, maxTime, interval: undefined });
    await api.executePromQLRangeQuery('cpu_usage');
    assert.equal(calls[0].step, 1, 'falls back to 1s when interval absent');
});

test('defaultRangeFor: whole span, native step, degenerate interval guarded', () => {
    const minTime = 1_700_000_000;
    const r = defaultRangeFor({ minTime, maxTime: minTime + 86_400, interval: 1 });
    assert.deepEqual(r, { start: minTime, end: minTime + 86_400, step: 1 });

    // A non-finite / non-positive interval falls back to a 1s step.
    // (The f64::MAX-from-empty-multi-file case is normalized to 0.0
    // server-side in routes.rs / crates/viewer, so the JS only ever sees
    // 0 here, which this guard maps to 1.)
    for (const bad of [0, NaN, undefined, -1]) {
        const rr = defaultRangeFor({ minTime, maxTime: minTime + 600, interval: bad });
        assert.equal(rr.step, 1, `interval=${bad} → step 1`);
    }
});
