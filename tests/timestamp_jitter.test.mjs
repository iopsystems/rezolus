import assert from 'node:assert/strict';
import test from 'node:test';
import { TIMESTAMP_JITTER_CHART_ID, deltasMs, toDeviation, averageMs, nominalMsFor, jitterSpec }
    from '../src/viewer/assets/lib/charts/jitter.js';

const ts = [1_000_000_000, 2_003_000_000, 2_998_000_000, 4_001_000_000]; // ns, ~1s cadence

test('deltasMs: n-1 points, ns->ms', () => {
    assert.deepEqual(deltasMs(ts), [1003, 995, 1003]);
});
test('toDeviation subtracts nominal', () => {
    assert.deepEqual(toDeviation([1003, 995, 1003], 1000), [3, -5, 3]);
});
test('averageMs: mean delta (empty -> 0)', () => {
    assert.equal(averageMs([90, 100, 110]), 100);
    assert.equal(averageMs([100, 100, 100]), 100);
    assert.equal(averageMs([]), 0);
});
test('nominalMsFor: honor a plausible declared interval', () => {
    // Declared at/below the achieved average is the intent — honored even if it
    // differs from the average, so a consistent lag behind intent stays visible.
    assert.equal(nominalMsFor([1003, 995, 1003], 1000), 1000); // ~= average
    assert.equal(nominalMsFor([90, 100, 110], 95), 95);        // below average (lag)
});
test('nominalMsFor: average when the interval is undeclared', () => {
    assert.equal(nominalMsFor([90, 100, 110], null), 100);
    assert.equal(nominalMsFor([90, 100, 110], 0), 100);
});
test('nominalMsFor: reject a bogus declared interval slower than reality', () => {
    // The heartbeat case: producer hardcodes 1000ms but actually beats at ~100ms.
    // 1000 >> average(100), impossible for a real target -> use the average.
    assert.equal(nominalMsFor([90, 100, 110], 1000), 100);
    assert.equal(nominalMsFor([100, 100, 100], 1000), 100);
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
test('jitterSpec deviation mode: plausible declared nominal subtracted', () => {
    const spec = jitterSpec(ts, { mode: 'deviation', nominalMs: 1000 });
    const [, ys] = spec.data;
    assert.deepEqual(ys, [3, -5, 3].map((ms) => ms * 1e6));
});
test('jitterSpec deviation mode: undeclared nominal -> deviation from average', () => {
    // deltas [90,100,110], average 100 -> deviations [-10,0,10].
    const ts2 = [1_000_000_000, 1_090_000_000, 1_190_000_000, 1_300_000_000];
    const spec = jitterSpec(ts2, { mode: 'deviation' });
    const [, ys] = spec.data;
    assert.deepEqual(ys, [-10, 0, 10].map((ms) => ms * 1e6));
});
test('jitterSpec deviation mode: bogus 1000ms declared on 100ms data -> average baseline', () => {
    // The real heartbeat failure: declared 1000ms, actual ~100ms. Must center on
    // the achieved cadence, not show a ~-900ms offset.
    const ts3 = [1_000_000_000, 1_100_000_000, 1_200_000_000, 1_300_000_000]; // 100ms deltas
    const spec = jitterSpec(ts3, { mode: 'deviation', nominalMs: 1000 });
    const [, ys] = spec.data;
    assert.deepEqual(ys, [0, 0, 0]);
});
