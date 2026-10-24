import { test } from 'node:test';
import assert from 'node:assert/strict';
import { seriesGapGeometry, insertGapNulls } from '../src/viewer/assets/lib/charts/util/utils.js';

// #1519. A compare chart's baseline arrives in decimated buckets while its
// experiment arrives at the recording's interval, and the line chart gapped
// both against the interval, so the baseline drew as dashes. Separately, a
// decimated per-entity chart's min/max envelope spanned a missing bucket that
// its line broke at.

globalThis.document = globalThis.document || { documentElement: { getAttribute: () => null } };
globalThis.getComputedStyle = globalThis.getComputedStyle || (() => ({ getPropertyValue: () => '' }));
const { configureLineChart } = await import('../src/viewer/assets/lib/charts/line.js');
const { configureMultiSeriesChart } = await import('../src/viewer/assets/lib/charts/multi.js');

const T0 = 1_700_000_000;
const grid = (n, step, skip = -1) => Array.from({ length: n }, (_, i) => T0 + i * step).filter((_, i) => i !== skip);
const yOf = (d) => (Array.isArray(d) ? d[1] : d.value[1]);
const nulls = (data) => data.filter((d) => yOf(d) === null).length;

const fakeChart = (spec, interval = 1) => {
    const captured = {};
    return {
        captured, interval, spec,
        domNode: { classList: { add() {}, remove() {} }, querySelector: () => null, querySelectorAll: () => [], appendChild() {}, style: {} },
        echart: { setOption: (o) => { captured.option = o; }, dispatchAction() {}, clear() {}, on() {}, off() {} },
    };
};

test('seriesGapGeometry: native spacing keeps the interval and steps', () => {
    assert.deepEqual(seriesGapGeometry(grid(20, 1), 1), { gapInterval: 1, step: 'start' });
    // A hole does not move the median.
    assert.deepEqual(seriesGapGeometry(grid(20, 1, 5), 1), { gapInterval: 1, step: 'start' });
});

test('seriesGapGeometry: decimated buckets are gapped at their spacing, unstepped', () => {
    const g = seriesGapGeometry(grid(20, 10), 1);
    assert.deepEqual(g, { gapInterval: 10, step: false });
    const zip = (ts) => ts.map((t, i) => [t * 1000, i]);
    assert.equal(nulls(insertGapNulls(zip(grid(20, 10)), g.gapInterval)), 0);
    assert.equal(nulls(insertGapNulls(zip(grid(20, 10, 7)), g.gapInterval)), 1);
});

test('compare: a decimated baseline and a native experiment both draw unbroken', () => {
    const base = grid(20, 10, 7); // one bucket missing
    const exp = grid(200, 1);
    const chart = fakeChart({
        opts: {},
        multiSeries: [
            { name: 'baseline', timeData: base, valueData: base.map((_, i) => i), color: '#111' },
            { name: 'experiment', timeData: exp, valueData: exp.map((_, i) => i), color: '#222' },
        ],
    });
    configureLineChart(chart);
    const lines = chart.captured.option.series.filter((s) => s.type === 'line' && s.name && !s.stack);
    const byName = Object.fromEntries(lines.map((s) => [s.name, s]));
    assert.equal(nulls(byName.baseline.data), 1, 'only the missing bucket breaks the baseline');
    assert.equal(byName.baseline.step, false);
    assert.equal(nulls(byName.experiment.data), 0);
    assert.equal(byName.experiment.step, 'start');
});

test('multi: a decimated envelope breaks where its line does', () => {
    const t = grid(20, 5, 7); // one bucket missing
    const col = (v) => new Float64Array(t.map(() => v));
    const box = (k) => ({ t, min: col(k), lo: col(k + 1), median: col(k + 2), hi: col(k + 3), max: col(k + 4), decimated: true });
    const chart = fakeChart({
        data: [t, t.map(() => 2), t.map(() => 12)],
        opts: {},
        series_names: ['a', 'b'],
        boxplot: [box(0), box(10)],
        boxplotDecimated: true,
    });
    configureMultiSeriesChart(chart);
    const series = chart.captured.option.series;
    const lines = series.filter((s) => s.name && !s.stack);
    const bands = series.filter((s) => s.stack && String(s.stack).startsWith('mb'));
    assert.ok(bands.length > 0, 'the envelope is drawn');
    for (const l of lines) assert.equal(nulls(l.data), 1, 'the line breaks once');
    for (const b of bands) assert.equal(nulls(b.data), 1, `band ${b.stack} breaks once`);
});
