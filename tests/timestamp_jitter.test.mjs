import assert from 'node:assert/strict';
import test from 'node:test';
import { TIMESTAMP_JITTER_CHART_ID, deltasMs, toDeviation, jitterSpec }
    from '../src/viewer/assets/lib/charts/jitter.js';

const ts = [1_000_000_000, 2_003_000_000, 2_998_000_000, 4_001_000_000]; // ns

test('deltasMs: n-1 points, ns->ms', () => {
    assert.deepEqual(deltasMs(ts), [1003, 995, 1003]);
});
test('toDeviation subtracts nominal', () => {
    assert.deepEqual(toDeviation([1003, 995, 1003], 1000), [3, -5, 3]);
});
// NOTE: chart.js/line.js consume `data` as a pair of PARALLEL arrays
// `[timeData, valueData]` (seconds, base-unit value) — NOT an array of
// [x,y] point-pairs. Confirmed by reading configureLineChart in
// charts/line.js ("Single-series callers pass `data: [timeData,
// valueData]`") and applyResultToPlot in data.js (`plot.data =
// [timestamps, values]`). Time-axis values are seconds (line.js
// multiplies by 1000 to get ms for echarts); `unit_system: 'time'`
// (see crates/dashboard/src/plot.rs FormatConfig/Unit) expects its
// base value in NANOSECONDS, not ms — so the y series is scaled back
// up from deltasMs()'s ms output.
test('jitterSpec absolute mode: data is [timeSec, valueNs] parallel arrays', () => {
    const spec = jitterSpec(ts, { mode: 'absolute', nominalMs: 1000 });
    assert.equal(spec.opts.id, TIMESTAMP_JITTER_CHART_ID);
    const [xs, ys] = spec.data;
    assert.deepEqual(xs, ts.slice(1).map((ns) => ns / 1e9));
    assert.deepEqual(ys, [1003, 995, 1003].map((ms) => ms * 1e6));
});
test('jitterSpec deviation mode', () => {
    const spec = jitterSpec(ts, { mode: 'deviation', nominalMs: 1000 });
    const [, ys] = spec.data;
    assert.deepEqual(ys, [3, -5, 3].map((ms) => ms * 1e6));
});
