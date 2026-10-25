import { test } from 'node:test';
import assert from 'node:assert/strict';

// #1517: a per-entity chart can carry dozens of series, and a legend that
// wrapped past one row was drawn over the plot. The multi-series chart has no
// legend and uses the single-series chart's plot offset.

// multi.js pulls in colormap.js, which reads CSS custom properties at module
// load; the same DOM shim the other chart tests use.
globalThis.document = globalThis.document || { documentElement: { getAttribute: () => null } };
globalThis.getComputedStyle = globalThis.getComputedStyle || (() => ({ getPropertyValue: () => '' }));
const { configureMultiSeriesChart } = await import('../src/viewer/assets/lib/charts/multi.js');
const { getBaseOption } = await import('../src/viewer/assets/lib/charts/base.js');

const chartWith = (names) => {
    const ts = Array.from({ length: 10 }, (_, i) => 1_700_000_000 + i);
    const captured = {};
    return {
        captured,
        interval: 1,
        spec: {
            data: [ts, ...names.map(() => ts.map((_, i) => i))],
            opts: {},
            series_names: names,
        },
        domNode: { classList: { add() {}, remove() {} } },
        echart: { setOption: (o) => { captured.option = o; }, dispatchAction() {}, clear() {} },
    };
};

test('a multi-series chart has no legend, however many series', () => {
    for (const n of [2, 32]) {
        const chart = chartWith(Array.from({ length: n }, (_, i) => `/a/very/long/sysfs/path/sensor${i}`));
        configureMultiSeriesChart(chart);
        const { option } = chart.captured;
        assert.equal(option.legend, undefined, `${n} series`);
        assert.equal(option.series.filter((s) => s.name).length, n);
    }
});

test('the plot starts where a single-series chart starts', () => {
    const chart = chartWith(['a', 'b']);
    configureMultiSeriesChart(chart);
    assert.equal(chart.captured.option.grid.top, getBaseOption().grid.top);
});

test('the tooltip still names each series', () => {
    const chart = chartWith(['a', 'b']);
    configureMultiSeriesChart(chart);
    const { option } = chart.captured;
    assert.equal(typeof option.tooltip.formatter, 'function');
    assert.deepEqual(option.series.filter((s) => s.name).map((s) => s.name), ['a', 'b']);
});
