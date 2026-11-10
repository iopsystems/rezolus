// A histogram quantile past the highest finite bucket is +Inf (metriken-query
// 0.47), written in a query result's JSON as the string "+Inf" and in the
// display binary as an f64 Infinity. A percentile chart draws it in its
// out-of-bounds band (markOverflow); without that it draws nothing there.
// Every other chart gets a gap. metriken-charts 0.1.3 keeps ±Infinity out of
// every series and axis computation itself; data.js does not depend on it.
import test from 'node:test';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { join } from 'node:path';

// echarts measures text on a canvas when it sees a `document`, and the DOM
// stub below is one; node has no canvas, so text is measured by length.
const echarts = createRequire(import.meta.url)(
    join(process.env.METRIKEN_CHARTS_ASSETS, 'charts/echarts.min.js'),
);
echarts.setPlatformAPI({ measureText: (text) => ({ width: String(text).length * 7 }) });

// The chart modules read CSS custom properties at load and scatter.js adds
// key listeners to `document`; stub the DOM (as the other chart tests do).
const stubElement = () => ({
    style: {}, classList: { add() {}, remove() {}, toggle() {} },
    appendChild() {}, addEventListener() {}, removeEventListener() {},
    querySelector: () => null, querySelectorAll: () => [], setAttribute() {}, remove() {},
    getBoundingClientRect: () => ({ x: 0, y: 0, width: 600, height: 300 }),
});
globalThis.document = globalThis.document || { documentElement: {} };
globalThis.document.documentElement.getAttribute ??= () => null;
globalThis.document.addEventListener ??= () => {};
globalThis.document.removeEventListener ??= () => {};
globalThis.document.createElement ??= stubElement;
globalThis.getComputedStyle =
    globalThis.getComputedStyle || (() => ({ getPropertyValue: () => '' }));

const {
    parseNumeric, parseIntervals, applyResultToPlot, applyDisplayResult,
    promqlResultToLinePair, withoutInfinities,
} = await import('../src/viewer/assets/lib/data.js');
const { configureScatterChart } = await import('../src/viewer/assets/lib/charts/scatter.js');
const { configureLineChart } = await import('../src/viewer/assets/lib/charts/line.js');
const { Chart } = await import('../src/viewer/assets/lib/charts/chart.js');


// 1 s in nanoseconds: the highest finite bound, the band's lower end.
const BOUND = 1e9;

const percentilePlot = (format = undefined) => ({
    opts: { id: 'p', type: 'histogram', subtype: 'percentiles', ...(format ? { format } : {}) },
});

// A histogram_quantiles result: p50 finite, p99 "+Inf" at the second point
// with the band (BOUND, "+Inf").
const percentileResult = () => ({
    status: 'success',
    data: {
        resultType: 'matrix',
        result: [
            {
                metric: { quantile: '0.5' },
                values: [[1, '100000000'], [2, '100000000'], [3, '120000000']],
            },
            {
                metric: { quantile: '0.99' },
                values: [[1, '500000000'], [2, '+Inf'], [3, '600000000']],
                bands: [['400000000', '500000000'], [String(BOUND), '+Inf'], ['500000000', '600000000']],
            },
        ],
    },
});

test('parseNumeric reads the strings a query result writes', () => {
    assert.equal(parseNumeric('+Inf'), Infinity);
    assert.equal(parseNumeric('-Inf'), -Infinity);
    assert.equal(parseNumeric('NaN'), null);
    assert.equal(parseNumeric(null), null);
    assert.equal(parseNumeric('1.5'), 1.5);
    assert.equal(parseNumeric(2), 2);
    assert.equal(parseNumeric('nope'), null);
});

test('a band with an infinite end has no top to draw', () => {
    assert.deepEqual(
        parseIntervals({ bands: [[1, 2], [String(BOUND), '+Inf'], null] }),
        [[1, 2], null, null],
    );
});

test('a percentile chart keeps +Inf and gets a range.max above every finite value', () => {
    const plot = percentilePlot();
    applyResultToPlot(plot, percentileResult());
    assert.equal(plot._resolvedStyle, 'scatter');
    assert.deepEqual(plot.data[2], [500000000, Infinity, 600000000]);
    // The largest finite value; the open band (BOUND, +Inf) is dropped.
    assert.equal(plot.opts.format.range.max, 6e8);
    assert.equal(plot.series_intervals[1][1], null);
});

test('a percentile chart with its own range.max keeps it', () => {
    const plot = percentilePlot({ range: { max: 5e8 } });
    applyResultToPlot(plot, percentileResult());
    assert.equal(plot.opts.format.range.max, 5e8);
    assert.equal(plot._formatBeforeOverflow, undefined);
});

test("the next result without +Inf restores the plot's own format", () => {
    const own = { unit_system: 'time' };
    const plot = percentilePlot(own);
    applyResultToPlot(plot, percentileResult());
    assert.equal(plot.opts.format.unit_system, 'time');
    assert.equal(plot.opts.format.range.max, 6e8);
    const finite = percentileResult();
    finite.data.result[1].values[1][1] = '550000000';
    finite.data.result[1].bands[1] = ['500000000', '550000000'];
    applyResultToPlot(plot, finite);
    assert.equal(plot.opts.format, own);
});

test('a line chart draws +Inf as a gap', () => {
    const plot = { opts: { id: 'l', type: 'gauge' } };
    applyResultToPlot(plot, {
        status: 'success',
        data: { resultType: 'matrix', result: [{ metric: {}, values: [[1, '1'], [2, '+Inf'], [3, '3']] }] },
    });
    assert.equal(plot._resolvedStyle, 'line');
    assert.ok(Number.isNaN(plot.data[1][1]));
    assert.equal(plot.opts.format, undefined);
});

test('the compare path reads +Inf as a gap', () => {
    const pair = promqlResultToLinePair([{ values: [[1, '1'], [2, '+Inf']] }]);
    assert.deepEqual(pair.valueData, [1, null]);
});

// The display binary decodes to Float64Arrays; Infinity arrives as itself.
const decodedWithInfinity = () => ({
    series: [{
        metric: {},
        t: new Float64Array([1, 2, 3]),
        min: new Float64Array([1, 2, 3]),
        lo: new Float64Array([1, 2, 3]),
        median: new Float64Array([1, Infinity, 3]),
        hi: new Float64Array([1, Infinity, 3]),
        max: new Float64Array([1, Infinity, 3]),
        decimated: true,
    }],
});

test('a display line chart draws Infinity as a gap without changing the decoded tile', () => {
    const plot = { opts: { id: 'l', type: 'gauge' }, promql_query: 'x' };
    const decoded = decodedWithInfinity();
    applyDisplayResult(plot, decoded);
    assert.ok(Number.isNaN(plot.data[1][1]));
    assert.ok(Number.isNaN(plot.boxplot[0].max[1]));
    assert.ok(plot.boxplot[0].max instanceof Float64Array);
    // The tile cache holds `decoded`; it is not mutated.
    assert.equal(decoded.series[0].max[1], Infinity);
});

test('a display percentile chart puts its range.max at the largest finite value', () => {
    const plot = { ...percentilePlot(), promql_query: 'x' };
    applyDisplayResult(plot, decodedWithInfinity());
    assert.equal(plot._resolvedStyle, 'scatter');
    assert.equal(plot.opts.format.range.max, 3);
    assert.equal(plot.boxplot[0].max[1], Infinity);
});

test('withoutInfinities returns its argument when there is nothing to replace', () => {
    const a = [1, 2, NaN];
    assert.equal(withoutInfinities(a), a);
});

// Render through scatter.js and echarts (server-side rendering, no DOM): the
// option scatter.js builds, given to echarts, must keep every finite point on
// the axis and put the +Inf point above range.max, in the out-of-bounds band.
const renderedYAxis = (plot, configure = configureScatterChart) => {
    const dom = stubElement();
    let option = null;
    const chart = {
        spec: plot,
        domNode: dom,
        interval: 1,
        chartsState: {},
        echart: {
            setOption: (o) => { option = o; },
            dispatchAction() {},
            on() {}, off() {},
            getOption: () => option,
            convertToPixel: () => [0, 0],
        },
    };
    configure(chart);
    assert.ok(option, 'the chart module set an option');
    const ec = echarts.init(null, null, { renderer: 'svg', ssr: true, width: 600, height: 300 });
    try {
        ec.setOption(option);
        return { option, extent: ec.getModel().getComponent('yAxis').axis.scale.getExtent() };
    } finally {
        ec.dispose();
    }
};

test('a +Inf percentile is drawn in the out-of-bounds band with the finite points on the axis', () => {
    const plot = percentilePlot();
    applyResultToPlot(plot, percentileResult());
    const { option, extent } = renderedYAxis(plot);
    // Every finite value is inside the axis.
    assert.ok(extent[0] <= 1e8 && extent[1] >= 6e8, `extent ${extent}`);
    assert.ok(Number.isFinite(extent[1]), `extent ${extent}`);
    // The p99 point at t=2 is the clamped one: parked above range.max, its raw
    // value kept for the tooltip.
    const p99 = option.series.find((s) => s.type === 'scatter' && s.name === 'p99');
    const clamped = p99.data.find((d) => d && d.value && d.value[2] != null);
    assert.ok(clamped, 'the +Inf point is drawn');
    assert.equal(clamped.value[2], Infinity);
    assert.ok(clamped.value[1] > 6e8 && clamped.value[1] <= extent[1], `${clamped.value}`);
});

test('without markOverflow the +Inf percentile is not drawn', () => {
    // The mutation this guards: the same plot with its range removed.
    const plot = percentilePlot();
    applyResultToPlot(plot, percentileResult());
    plot.opts = { ...plot.opts, format: undefined };
    const { option } = renderedYAxis(plot);
    const p99 = option.series.find((s) => s.type === 'scatter' && s.name === 'p99');
    const holdsInfinity = (d) => (Array.isArray(d) ? d : d?.value || []).includes(Infinity);
    assert.ok(!p99.data.some(holdsInfinity), 'no p99 item holds Infinity');
    assert.ok(!p99.data.some((d) => d && d.value && d.value[2] != null), 'nothing parked');
});

const lineResult = {
    status: 'success',
    data: {
        resultType: 'matrix',
        result: [{ metric: {}, values: [[1, '1'], [2, '+Inf'], [3, '3'], [4, '4']] }],
    },
};

test('a line chart with a +Inf value keeps its finite points on the axis', () => {
    const plot = { opts: { id: 'l', type: 'gauge' } };
    applyResultToPlot(plot, lineResult);
    const { extent } = renderedYAxis(plot, configureLineChart);
    assert.ok(extent[0] <= 1 && extent[1] >= 4, `extent ${extent}`);
});

test('a line chart given Infinity keeps its finite points on the axis', () => {
    const plot = { opts: { id: 'l', type: 'gauge' } };
    applyResultToPlot(plot, lineResult);
    plot.data = [plot.data[0], [1, Infinity, 3, 4]];
    const { extent } = renderedYAxis(plot, configureLineChart);
    assert.ok(extent[0] <= 1 && extent[1] >= 4, `extent ${extent}`);
});

// A zoom and a legend pin rescale the y-axis from the chart's data
// (Chart._rescaleYAxis), which a +Inf quantile must not take to an infinite
// extent. Through the plot applyResultToPlot builds and the Chart class the
// viewer's ChartView drives, with echarts stubbed to record the y-axis.
const rescaled = (act) => {
    const plot = percentilePlot();
    applyResultToPlot(plot, percentileResult());
    const yAxes = [];
    const chart = Object.create(Chart.prototype);
    Object.assign(chart, {
        spec: plot,
        domNode: stubElement(),
        interval: 1,
        pinnedSet: new Set(),
        chartsState: {
            zoomLevel: { start: 25, end: 100 },
            isDefaultZoom: () => false,
        },
        echart: {
            setOption: (o) => { if (!o.series && o.yAxis) yAxes.push(o.yAxis); },
            dispatchAction() {}, on() {}, off() {},
            getOption: () => ({ yAxis: [{ type: 'value' }] }),
            convertToPixel: () => [0, 0],
        },
    });
    configureScatterChart(chart);
    act(chart);
    assert.ok(yAxes.length > 0, 'the y-axis was rescaled');
    return yAxes.at(-1);
};

test('a zoom over a +Inf percentile keeps the y-axis finite', () => {
    const y = rescaled((chart) => chart._applyZoom({ start: 25, end: 100 }));
    assert.ok(Number.isFinite(y.min) && Number.isFinite(y.max), JSON.stringify(y));
    assert.ok(y.max >= 6e8, JSON.stringify(y));
});

test('pinning the +Inf percentile keeps the y-axis finite', () => {
    const y = rescaled((chart) => {
        chart.pinnedSet.add('p99');
        chart._rescaleYAxis();
    });
    assert.ok(Number.isFinite(y.min) && Number.isFinite(y.max), JSON.stringify(y));
    assert.ok(y.min <= 6e8 && y.max >= 6e8, JSON.stringify(y));
});
