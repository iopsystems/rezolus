import test from 'node:test';
import assert from 'node:assert/strict';

// The line chart's total toggle and the scatter chart's spectrum checkboxes
// fetch through charts/host.js. With no host registered they are left out,
// so a page that embeds the charts without data.js shows no control that
// does nothing. This file never loads data.js and relies on node --test's per-file
// process, so the host starts empty.
//
// The configure functions touch a few DOM globals; a shim stands in for them,
// as in html_escape.test.mjs.
globalThis.document = {
    documentElement: { getAttribute: () => 'dark' },
    createElement: () => fakeElement(),
    addEventListener() {},
    removeEventListener() {},
};
globalThis.getComputedStyle = globalThis.getComputedStyle || (() => ({ getPropertyValue: () => '' }));
const { setChartHost, resetChartHost } = await import('../src/viewer/assets/lib/charts/host.js');
const { configureLineChart } = await import('../src/viewer/assets/lib/charts/line.js');
const { configureScatterChart } = await import('../src/viewer/assets/lib/charts/scatter.js');

// An element whose querySelector finds a direct child by `.className`, which
// is how both controls look themselves up.
function fakeElement() {
    const el = {
        className: '',
        classList: { add() {}, remove() {}, contains: () => false, toggle() {} },
        style: {},
        children: [],
        querySelector: (sel) => el.children.find((c) => '.' + c.className === sel) ?? null,
        querySelectorAll: () => [],
        appendChild: (c) => { el.children.push(c); return c; },
        insertBefore: (c) => { el.children.push(c); return c; },
        removeChild() {},
        remove() { },
        addEventListener() {},
        removeEventListener() {},
        setAttribute() {},
        getBoundingClientRect: () => ({ width: 500, height: 300, left: 0, top: 0 }),
        getContext: () => new Proxy({}, { get: () => () => ({ addColorStop() {} }), set: () => true }),
        clientWidth: 500,
        clientHeight: 300,
    };
    return el;
}

function fakeChart(spec) {
    let option = null;
    return {
        spec,
        chartId: 'c1',
        domNode: fakeElement(),
        chartsState: {},
        interval: 1,
        pinnedSet: new Set(),
        _tooltipFrozen: false,
        echart: {
            setOption(o) { option = option ? { ...option, ...o } : o; },
            getOption: () => option,
            getModel: () => null,
            dispatchAction() {},
            on() {},
            off() {},
            getWidth: () => 500,
            getHeight: () => 300,
            convertToPixel: () => [0, 0],
            resize() {},
        },
    };
}

const has = (chart, cls) => chart.domNode.querySelector('.' + cls) !== null;

const lineSpec = () => ({
    opts: { title: 'cpu', format: {} },
    data: [[1, 2, 3], [4, 5, 6]],
    promql_query_total: 'sum(cpu)',
});

const scatterSpec = () => ({
    opts: { title: 'latency', format: {} },
    data: [[1, 2, 3], [4, 5, 6], [7, 8, 9]],
    series_names: ['p50', 'p99'],
});

test('without a host the line chart leaves out its total toggle', () => {
    resetChartHost();
    const chart = fakeChart(lineSpec());
    configureLineChart(chart);
    assert.equal(has(chart, 'total-toggle'), false);
});

test('with a range query and applyResultToPlot the line chart draws its total toggle', () => {
    setChartHost({ executePromQLRangeQuery: async () => null, applyResultToPlot: () => {} });
    try {
        const chart = fakeChart(lineSpec());
        configureLineChart(chart);
        assert.equal(has(chart, 'total-toggle'), true);
    } finally {
        resetChartHost();
    }
});

test('a range query alone does not draw the total toggle', () => {
    setChartHost({ executePromQLRangeQuery: async () => null });
    try {
        const chart = fakeChart(lineSpec());
        configureLineChart(chart);
        assert.equal(has(chart, 'total-toggle'), false);
    } finally {
        resetChartHost();
    }
});

test('without a host the scatter chart leaves out its spectrum checkboxes', () => {
    resetChartHost();
    const chart = fakeChart(scatterSpec());
    configureScatterChart(chart);
    assert.equal(has(chart, 'spectrum-controls'), false);
});

test('with a spectrum fetch the scatter chart draws its spectrum checkboxes', () => {
    setChartHost({ fetchQuantileSpectrumForPlot: async () => null });
    try {
        const chart = fakeChart(scatterSpec());
        configureScatterChart(chart);
        assert.equal(has(chart, 'spectrum-controls'), true);
    } finally {
        resetChartHost();
    }
});

test('the scatter chart passes its spec\'s scope to the spectrum fetch', () => {
    const plots = [];
    setChartHost({
        fetchQuantileSpectrumForPlot: async (plot) => { plots.push(plot); return null; },
    });
    try {
        const chart = fakeChart({ ...scatterSpec(), promql_query: 'lat', scope: ['u1'] });
        chart.spectrumKind = 'full';
        configureScatterChart(chart);
        assert.equal(plots.length, 1);
        assert.equal(plots[0].promql_query, 'lat');
        assert.deepEqual(plots[0].scope, ['u1']);
    } finally {
        resetChartHost();
    }
});

// A line chart that redraws itself when the toggle asks it to.
function toggleableChart(spec) {
    const chart = fakeChart(spec);
    chart.configureChartByType = () => configureLineChart(chart);
    return chart;
}

test('a total that fails after the scope changed leaves the chart its data', async () => {
    let fail;
    const answer = new Promise((_, r) => { fail = r; });
    setChartHost({ executePromQLRangeQuery: () => answer, applyResultToPlot: () => {} });
    try {
        const chart = toggleableChart({ ...lineSpec(), scope: ['u1'] });
        configureLineChart(chart);
        const toggling = chart.domNode.querySelector('.total-toggle').onclick();
        const rerun = [[1, 2, 3], [7, 8, 9]];
        chart.spec = { ...chart.spec, scope: ['u2'], data: rerun };
        fail(new Error('down'));
        await toggling;
        assert.equal(chart.spec.data, rerun);
        assert.equal(!!chart._showTotal, false);
    } finally {
        resetChartHost();
    }
});

test('a total cached for another scope is fetched again', async () => {
    const scopes = [];
    setChartHost({
        executePromQLRangeQuery: async (q, a, b, mode, scope) => {
            scopes.push(scope);
            return { status: 'success', data: { result: [{}] } };
        },
        applyResultToPlot: (spec) => { spec.data = [[1, 2, 3], [scopes.length, 0, 0]]; },
    });
    try {
        const chart = toggleableChart({ ...lineSpec(), scope: ['u1'] });
        configureLineChart(chart);
        const toggle = () => chart.domNode.querySelector('.total-toggle').onclick();
        await toggle(); // Total, fetched for u1
        await toggle(); // Mean
        chart.spec = { ...chart.spec, scope: ['u2'] };
        await toggle(); // Total again, under u2
        assert.deepEqual(scopes, [['u1'], ['u2']]);
    } finally {
        resetChartHost();
    }
});

test('a total that lands after the scope changed is dropped', async () => {
    let release;
    const answer = new Promise((r) => { release = r; });
    const total = [[1, 2, 3], [40, 50, 60]];
    setChartHost({
        executePromQLRangeQuery: () => answer,
        applyResultToPlot: (spec) => { spec.data = total; },
    });
    try {
        const chart = toggleableChart({ ...lineSpec(), scope: ['u1'] });
        configureLineChart(chart);
        const toggling = chart.domNode.querySelector('.total-toggle').onclick();
        // The view reruns under another scope while the total is in flight.
        const rerun = [[1, 2, 3], [7, 8, 9]];
        chart.spec = { ...chart.spec, scope: ['u2'], data: rerun };
        configureLineChart(chart);
        release({ status: 'success', data: { result: [{ metric: {}, values: [[1, '40']] }] } });
        await toggling;
        assert.equal(chart.spec.data, rerun);
        assert.equal(!!chart._showTotal, false);
        assert.equal(chart._totalDataCache ?? null, null);
    } finally {
        resetChartHost();
    }
});

test('a chart whose spec gains or drops its total query is configured again over the same data', async () => {
    globalThis.window = globalThis.window || { addEventListener() {}, removeEventListener() {} };
    const { Chart } = await import('../src/viewer/assets/lib/charts/chart.js');
    const { chartHost } = await import('../src/viewer/assets/lib/charts/host.js');
    const chart = Object.create(Chart.prototype);
    let configured = 0;
    chart.configureChartByType = () => { configured++; };
    chart._applyEventMarkers = () => {};
    chart.chartsState = { zoomLevel: null };
    chart.echart = { dispatchAction() {} };
    chart._themeVersion = chartHost().getThemeVersion();
    const opts = { title: 'cpu', format: {} };
    const data = () => [[1, 2, 3], [4, 5, 6]];
    chart.spec = { opts, data: data(), promql_query_total: 'sum(cpu)' };
    chart.update({ spec: { opts, data: data(), promql_query_total: 'sum(cpu)' }, interval: 1 });
    assert.equal(configured, 0, 'the same spec is not configured again');
    chart.update({ spec: { opts, data: data() }, interval: 1 });
    assert.equal(configured, 1);
    chart.update({ spec: { opts, data: data(), promql_query_total: 'sum(cpu)' }, interval: 1 });
    assert.equal(configured, 2);
});

test('a line chart that loses its total toggle draws its data unticked', () => {
    setChartHost({ executePromQLRangeQuery: async () => null, applyResultToPlot: () => {} });
    try {
        const chart = toggleableChart(lineSpec());
        configureLineChart(chart);
        chart._showTotal = true;
        chart.spec = { ...chart.spec, promql_query_total: undefined };
        configureLineChart(chart);
        // (The fake element's remove() keeps it; the toggle's state is what
        // is checked.)
        assert.equal(chart._showTotal, false);
    } finally {
        resetChartHost();
    }
});

test('a chart that loses its total query with its data is unticked, and comes back as the mean', () => {
    setChartHost({ executePromQLRangeQuery: async () => null, applyResultToPlot: () => {} });
    try {
        const chart = toggleableChart(lineSpec());
        chart.echart.clear = () => {};
        configureLineChart(chart);
        chart._showTotal = true;
        // A run that returned no series, of a query without the chart's
        // total: no data and no total query.
        chart.spec = { ...chart.spec, data: [], promql_query_total: undefined };
        configureLineChart(chart);
        assert.equal(chart._showTotal, false);
        // Reset to the chart's own query: the toggle is back, unticked, and
        // the series is the mean.
        chart.spec = lineSpec();
        configureLineChart(chart);
        assert.equal(chart._showTotal, false);
    } finally {
        resetChartHost();
    }
});
