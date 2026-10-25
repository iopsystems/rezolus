// #1289: a `multi` chart names a lone series on the display path, as the
// JSON path does. multi.js colours a line by its name, so an unnamed series
// was drawn as "Series 1" in another colour after a zoom or refresh.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { applyDisplayToPlot, applyResultToPlot } from '../src/viewer/assets/lib/data.js';

const decoded = (metrics) => ({
    series: metrics.map((metric) => ({
        metric,
        t: new Float64Array([1, 2]),
        median: new Float64Array([0.5, 0.6]),
    })),
});

test('a multi chart names its one series on the display path', () => {
    const plot = { opts: { style: 'multi' } };
    applyDisplayToPlot(plot, decoded([{ name: '/system.slice/ssh.service' }]));
    assert.deepEqual(plot.series_names, ['/system.slice/ssh.service']);
});

test('the display path names a lone series as the JSON path does', () => {
    const metric = { name: '/system.slice/ssh.service' };
    const display = { opts: { style: 'multi' } };
    applyDisplayToPlot(display, decoded([metric]));
    const json = { opts: { style: 'multi' } };
    applyResultToPlot(json, {
        status: 'success',
        data: { resultType: 'matrix', result: [{ metric, values: [[1, '0.5'], [2, '0.6']] }] },
    });
    assert.deepEqual(display.series_names, json.series_names);
});

test("a multi chart's lone series keeps its band where multi.js reads it", () => {
    const plot = { opts: { style: 'multi' } };
    const d = decoded([{ name: 'x' }]);
    d.series[0].uncLo = new Float64Array([0.4, 0.5]);
    d.series[0].uncHi = new Float64Array([0.6, 0.7]);
    applyDisplayToPlot(plot, d);
    assert.deepEqual(plot.series_intervals, [[[0.4, 0.6], [0.5, 0.7]]]);
    assert.equal(plot.intervals, null);
});

test('a single-line chart still leaves its one series unnamed', () => {
    const plot = { opts: {} };
    applyDisplayToPlot(plot, decoded([{ name: 'x' }]));
    assert.deepEqual(plot.series_names, []);
});

test('every individual cgroup chart is a multi chart', () => {
    const section = JSON.parse(readFileSync(new URL('../crates/dashboard/sections/rezolus/cgroups.json', import.meta.url)));
    const plots = [];
    (function walk(o) {
        if (Array.isArray(o)) o.forEach(walk);
        else if (o && typeof o === 'object') {
            if (typeof o.id === 'string' && o.id.startsWith('individual-')) plots.push(o);
            Object.values(o).forEach(walk);
        }
    })(section);
    assert.ok(plots.length > 5);
    for (const p of plots) assert.equal(p.style, 'multi', p.id);
});
