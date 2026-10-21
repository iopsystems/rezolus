import { test } from 'node:test';
import assert from 'node:assert/strict';
import { insertGapNulls } from '../src/viewer/assets/lib/charts/util/utils.js';

// multi.js pulls in colormap.js, which reads CSS custom properties at module
// load; the same two-line DOM shim compare_bands.test.mjs uses, then a
// dynamic import so it runs after the shim.
globalThis.document = globalThis.document || { documentElement: { getAttribute: () => null } };
globalThis.getComputedStyle = globalThis.getComputedStyle || (() => ({ getPropertyValue: () => '' }));
const { multiLineGeometry } = await import('../src/viewer/assets/lib/charts/multi.js');

// #1516: a decimated multi-series chart holds one point per display bucket
// (5s on a ~10 minute live view of a 1s recording). Judging gaps against the
// 1s interval inserted a null after every bucket, and `step: 'start'` drew
// each point as a 1s dash followed by a 4s gap.

const nulls = (pts) => pts.filter(([, v]) => v === null).length;
const zip = (ts) => ts.map((t, i) => [t * 1000, i]);

test('decimated buckets wider than the interval are not gaps', () => {
    const ts = Array.from({ length: 20 }, (_, i) => 1_700_000_000 + i * 5);
    const g = multiLineGeometry(ts, 1, true);
    assert.equal(g.gapInterval, 5);
    assert.equal(g.step, false);
    assert.equal(nulls(insertGapNulls(zip(ts), g.gapInterval)), 0);
});

test('a missing bucket in a decimated response is still a gap', () => {
    const ts = Array.from({ length: 20 }, (_, i) => 1_700_000_000 + i * 5).filter((_, i) => i !== 7);
    const g = multiLineGeometry(ts, 1, true);
    assert.equal(g.gapInterval, 5);
    assert.equal(nulls(insertGapNulls(zip(ts), g.gapInterval)), 1);
});

test('native resolution keeps the interval threshold and stepped lines', () => {
    const ts = Array.from({ length: 20 }, (_, i) => 1_700_000_000 + i);
    const g = multiLineGeometry(ts, 1, false);
    assert.deepEqual(g, { gapInterval: 1, step: 'start' });
    const holed = ts.filter((_, i) => i !== 4);
    assert.equal(nulls(insertGapNulls(zip(holed), g.gapInterval)), 1);
});

test('a short bucket at the edge of the range does not set the spacing', () => {
    const ts = Array.from({ length: 20 }, (_, i) => 1_700_000_000 + i * 10);
    ts.push(ts[ts.length - 1] + 1);
    const g = multiLineGeometry(ts, 1, true);
    assert.equal(g.gapInterval, 10);
    assert.equal(nulls(insertGapNulls(zip(ts), g.gapInterval)), 0);
});

test('a decimated response of one point falls back to the interval', () => {
    assert.deepEqual(multiLineGeometry([1_700_000_000], 1, true), { gapInterval: 1, step: false });
});

// Through the chart entry point: the option handed to echarts.
const { configureMultiSeriesChart } = await import('../src/viewer/assets/lib/charts/multi.js');

const fakeChart = (spec) => {
    const captured = {};
    return {
        captured,
        interval: 1,
        spec,
        domNode: { classList: { add() {}, remove() {} } },
        echart: {
            setOption: (option) => { captured.option = option; },
            dispatchAction() {},
            clear() {},
        },
    };
};

const decimatedSpec = (extra = {}) => {
    const ts = Array.from({ length: 20 }, (_, i) => 1_700_000_000 + i * 5);
    return {
        data: [ts, ts.map((_, i) => i), ts.map((_, i) => 2 * i)],
        opts: {},
        series_names: ['a', 'b'],
        boxplotDecimated: true,
        ...extra,
    };
};

const lineNulls = (option) => option.series
    .filter((s) => s.name && !s.stack)
    .map((s) => s.data.filter((d) => (Array.isArray(d) ? d[1] : d.value[1]) === null).length);

test('configureMultiSeriesChart: decimated lines carry no gap nulls and are unstepped', () => {
    const chart = fakeChart(decimatedSpec());
    configureMultiSeriesChart(chart);
    const lines = chart.captured.option.series.filter((s) => s.name && !s.stack);
    assert.equal(lines.length, 2);
    assert.deepEqual(lineNulls(chart.captured.option), [0, 0]);
    assert.ok(lines.every((s) => s.step === false));
});

test('configureMultiSeriesChart: decimated percentile bands carry no gap nulls', () => {
    const spec = decimatedSpec({
        promql_query: 'histogram_quantiles([0.5, 0.99], x)',
    });
    spec.series_intervals = spec.data.slice(1).map((col) => col.map((v) => [v - 0.5, v + 0.5]));
    const chart = fakeChart(spec);
    configureMultiSeriesChart(chart);
    const bands = chart.captured.option.series.filter((s) => s.stack && String(s.stack).startsWith('band-'));
    assert.ok(bands.length > 0, 'percentile bands were drawn');
    for (const b of bands) {
        assert.equal(b.data.filter((d) => d[1] === null).length, 0);
    }
});
