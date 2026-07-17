import assert from 'node:assert/strict';
import test from 'node:test';
import { TIMESTAMP_JITTER_CHART_ID, deltasMs, toDeviation, medianMs, nominalMsFor, jitterSpec }
    from '../src/viewer/assets/lib/charts/jitter.js';

const ts = [1_000_000_000, 2_003_000_000, 2_998_000_000, 4_001_000_000]; // ns

test('deltasMs: n-1 points, ns->ms', () => {
    assert.deepEqual(deltasMs(ts), [1003, 995, 1003]);
});
test('toDeviation subtracts nominal', () => {
    assert.deepEqual(toDeviation([1003, 995, 1003], 1000), [3, -5, 3]);
});
test('medianMs: robust central delta (odd + even)', () => {
    assert.equal(medianMs([1003, 995, 1003]), 1003);   // sorted [995,1003,1003]
    assert.equal(medianMs([100, 104, 96, 100]), 100);  // sorted [96,100,100,104] -> (100+100)/2
    assert.equal(medianMs([]), 0);
});
test('nominalMsFor: declared wins; median only when undeclared', () => {
    // Declared interval is the intent — used even if it differs from the median,
    // so a consistent lag behind intent stays visible.
    assert.equal(nominalMsFor([1003, 995, 1003], 1000), 1000);
    // No declared interval -> data-derived median.
    assert.equal(nominalMsFor([1003, 995, 1003], null), 1003);
    assert.equal(nominalMsFor([1003, 995, 1003], 0), 1003);
});
// NOTE: chart.js/line.js consume `data` as parallel arrays [timeData, valueData]
// (seconds, base-unit value) — NOT [x,y] point-pairs. `unit_system: 'time'`
// expects the y base value in NANOSECONDS, so deltasMs()'s ms output is scaled up.
test('jitterSpec absolute mode: data is [timeSec, valueNs] parallel arrays', () => {
    const spec = jitterSpec(ts, { mode: 'absolute' });
    assert.equal(spec.opts.id, TIMESTAMP_JITTER_CHART_ID);
    const [xs, ys] = spec.data;
    assert.deepEqual(xs, ts.slice(1).map((ns) => ns / 1e9));
    assert.deepEqual(ys, [1003, 995, 1003].map((ms) => ms * 1e6));
});
test('jitterSpec deviation mode: declared nominal subtracted', () => {
    const spec = jitterSpec(ts, { mode: 'deviation', nominalMs: 1000 });
    const [, ys] = spec.data;
    assert.deepEqual(ys, [3, -5, 3].map((ms) => ms * 1e6));
});
test('jitterSpec deviation mode: no declared nominal -> deviation from median', () => {
    // median of [1003,995,1003] is 1003 -> deviations [0,-8,0]; independent of
    // the recording's (possibly defaulted/absent) declared interval.
    const spec = jitterSpec(ts, { mode: 'deviation' });
    const [, ys] = spec.data;
    assert.deepEqual(ys, [0, -8, 0].map((ms) => ms * 1e6));
});
