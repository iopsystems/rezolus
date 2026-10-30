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
