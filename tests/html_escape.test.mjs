import test from 'node:test';
import assert from 'node:assert/strict';

// Strings from a recording (plot titles, label values, capture labels,
// template formats) reach HTML through ECharts tooltip formatters, whose
// return value ECharts sets as innerHTML, and through the swatch row's
// innerHTML. These tests feed each one markup and check it comes out as text.
//
// colormap.js reads CSS custom properties at module load, and the configure
// functions read the theme attribute and create legend elements; stub those
// DOM globals before importing (a shim, not jsdom, as scatter_bands.test.mjs
// does). Each test file runs in its own process, so the stub does not leak.
globalThis.document = {
    documentElement: { getAttribute: () => 'dark' },
    createElement: () => fakeElement(),
    addEventListener() {},
    removeEventListener() {},
};
globalThis.getComputedStyle = globalThis.getComputedStyle || (() => ({ getPropertyValue: () => '' }));
const { escapeHtml } = await import('../src/viewer/assets/lib/html.js');
const { getTooltipFormatter, buildFreezeFooterContent } = await import('../src/viewer/assets/lib/charts/base.js');
const { swatchRowHtml } = await import('../src/viewer/assets/lib/charts/swatches.js');
const { configureLineChart } = await import('../src/viewer/assets/lib/charts/line.js');
const { configureHeatmap } = await import('../src/viewer/assets/lib/charts/heatmap.js');
const { configureQuantileHeatmap } = await import('../src/viewer/assets/lib/charts/quantile_heatmap.js');

const PAYLOAD = '<img src=x onerror=alert(1)>';

function assertEscaped(html, what) {
    assert.ok(html.includes('&lt;img'), `${what}: payload missing or not escaped:\n${html}`);
    assert.ok(!html.includes('<img'), `${what}: raw <img reached the HTML:\n${html}`);
}

test('escapeHtml escapes the five HTML metacharacters', () => {
    assert.equal(escapeHtml(`<a href="x" title='y'>&</a>`),
        '&lt;a href=&quot;x&quot; title=&#39;y&#39;&gt;&amp;&lt;/a&gt;');
    assert.equal(escapeHtml(null), '');
    assert.equal(escapeHtml(undefined), '');
    assert.equal(escapeHtml(42), '42');
});

// A minimal Chart: the configure functions only need a DOM node that accepts
// class changes and children, and an echart that records the option.
function fakeElement() {
    const el = {
        classList: { add() {}, remove() {}, contains: () => false, toggle() {} },
        style: {},
        children: [],
        querySelector: () => null,
        querySelectorAll: () => [],
        appendChild: (c) => { el.children.push(c); return c; },
        insertBefore: (c) => { el.children.push(c); return c; },
        removeChild() {},
        remove() {},
        addEventListener() {},
        removeEventListener() {},
        setAttribute() {},
        getBoundingClientRect: () => ({ width: 500, height: 300, left: 0, top: 0 }),
        // Color-legend gradient canvas: every 2D-context call is a no-op.
        getContext: () => new Proxy({}, {
            get: () => () => ({ addColorStop() {} }),
            set: () => true,
        }),
        clientWidth: 500,
        clientHeight: 300,
    };
    return el;
}

function fakeChart(spec) {
    let option = null;
    const chart = {
        spec,
        chartId: 'c1',
        domNode: fakeElement(),
        chartsState: {},
        interval: 1,
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
    return { chart, option: () => option };
}

test('line/multi tooltip escapes the series name', () => {
    const fmt = getTooltipFormatter((v) => String(v), null, { _tooltipFrozen: false });
    const html = fmt([{ seriesName: PAYLOAD, marker: '', value: [0, 1, null] }]);
    assertEscaped(html, 'seriesName');
});

test('scatter tooltip escapes the series name', () => {
    const fmt = getTooltipFormatter((v) => String(v), new Set(), { _tooltipFrozen: false }, 'scatter');
    const html = fmt([{ seriesName: PAYLOAD, marker: '', value: [0, 1, null] }]);
    assertEscaped(html, 'scatter seriesName');
});

test('a single-series line chart puts the plot title into the tooltip as text', () => {
    // configureLineChart names its one series after opts.title; ECharts hands
    // that back to the formatter as seriesName.
    const { chart, option } = fakeChart({
        opts: { title: PAYLOAD, format: {} },
        data: [[1, 2, 3], [4, 5, 6]],
    });
    configureLineChart(chart);
    const opt = option();
    assert.ok(opt, 'configureLineChart set an option');
    const named = opt.series.find((s) => s.name === PAYLOAD);
    assert.ok(named, 'the series carries the title as its name');
    const html = opt.tooltip.formatter([{ seriesName: named.name, marker: '', value: [1000, 4, null] }]);
    assertEscaped(html, 'line title');
});

test('the freeze footer escapes the chart id', () => {
    const prev = globalThis.m;
    globalThis.m = { route: { get: () => '/notebook' } };
    try {
        const html = buildFreezeFooterContent({ _tooltipFrozen: true, chartId: `"><img src=x onerror=alert(1)>` });
        assertEscaped(html, 'chartId');
    } finally {
        globalThis.m = prev;
    }
});

test('swatch row escapes title, label and kind', () => {
    assertEscaped(swatchRowHtml([{ kind: 'inner', label: PAYLOAD, title: 'ok' }]), 'swatch label');
    const titled = swatchRowHtml([{ kind: 'inner', label: 'ok', title: `"${PAYLOAD}` }]);
    assertEscaped(titled, 'swatch title');
    assert.ok(!titled.includes('title=""'), 'title attribute not broken out of');
    assertEscaped(swatchRowHtml([{ kind: `x">${PAYLOAD}`, label: 'ok', title: 'ok' }]), 'swatch kind');
});

function heatmapTooltip(specExtra, params) {
    const { chart, option } = fakeChart({
        opts: { title: 't', format: {} },
        time_data: [1, 2],
        data: [[0, 0, 1], [1, 0, 2]],
        min_value: 1,
        max_value: 2,
        ...specExtra,
    });
    configureHeatmap(chart);
    return option().tooltip.formatter(params);
}

test('heatmap tooltip escapes a supplied row label', () => {
    const html = heatmapTooltip({ row_labels: [PAYLOAD] }, { data: [1000, 0, 0, null, 1] });
    assertEscaped(html, 'heatmap row label');
});

test('heatmap tooltip escapes the row entity and value label from the template format', () => {
    const html = heatmapTooltip(
        { opts: { title: 't', format: { row_label: PAYLOAD, value_label: PAYLOAD } } },
        { data: [1000, 0, 0, null, 1] },
    );
    assertEscaped(html, 'heatmap row entity / value label');
    assert.equal(html.split('&lt;img').length - 1, 2, 'both fields present and escaped');
});

test('heatmap no-data tooltip escapes the row label', () => {
    const html = heatmapTooltip({ row_labels: [PAYLOAD] }, { data: [1000, 0, 0, null, null] });
    assertEscaped(html, 'heatmap no-data row label');
});

test('diff heatmap tooltip escapes capture labels', () => {
    const html = heatmapTooltip(
        {
            row_labels: [PAYLOAD],
            diffMatrices: { baseline: [[1, 2]], experiment: [[3, 4]] },
            diffCaptureLabels: { baseline: PAYLOAD, experiment: PAYLOAD },
        },
        { data: [1000, 0, 0, null, 1] },
    );
    assertEscaped(html, 'diff heatmap row and capture labels');
});

function quantileTooltip(specExtra, params) {
    const { chart, option } = fakeChart({
        opts: { title: 't', format: {} },
        data: [[1, 2], [5, 6]],
        series_names: [PAYLOAD],
        ...specExtra,
    });
    configureQuantileHeatmap(chart);
    return option().tooltip.formatter(params);
}

test('quantile heatmap tooltip escapes the series label', () => {
    assertEscaped(quantileTooltip({}, { data: [1000, 0, 5] }), 'quantile label');
});

test('quantile diff tooltip escapes capture labels', () => {
    const html = quantileTooltip({
        diffMatrices: { baseline: [[1, 2]], experiment: [[3, 4]] },
        diffCaptureLabels: { baseline: PAYLOAD, experiment: PAYLOAD },
    }, { data: [1000, 0, 5] });
    assertEscaped(html, 'quantile diff series and capture labels');
});

test('quantile compare-pair tooltip escapes capture labels', () => {
    const html = quantileTooltip({
        compareCounterpartData: { data: [[1, 2], [7, 8]] },
        compareCaptureLabels: { baseline: PAYLOAD, experiment: PAYLOAD },
        compareSelfRole: 'baseline',
    }, { data: [1000, 0, 5] });
    assertEscaped(html, 'quantile compare-pair series and capture labels');
});

// An unknown `unit_system` (from a KPI or a section template) is returned by
// units.js as the unit suffix, so it reaches the tooltip with each value.
test('line tooltip escapes an unknown unit_system in the value', () => {
    const { chart, option } = fakeChart({
        opts: { title: 'ok', format: { unit_system: PAYLOAD } },
        data: [[1, 2, 3], [4, 5, 6]],
    });
    configureLineChart(chart);
    const html = option().tooltip.formatter([{ seriesName: 'ok', marker: '', value: [1000, 4, null] }]);
    assertEscaped(html, 'line unit_system');
});

test('line tooltip escapes an unknown unit_system in the clamped raw value', () => {
    const fmt = getTooltipFormatter((v) => `${v} ${PAYLOAD}`, null, { _tooltipFrozen: false });
    const html = fmt([{ seriesName: 'ok', marker: '', value: [0, 1, 2] }]);
    assertEscaped(html, 'raw value');
    assert.equal(html.split('&lt;img').length - 1, 2, 'value and raw value both escaped');
});

test('scatter tooltip escapes an unknown unit_system in the clamped value', () => {
    const fmt = getTooltipFormatter((v) => `${v} ${PAYLOAD}`, new Set(), { _tooltipFrozen: false }, 'scatter');
    assertEscaped(fmt([{ seriesName: 'ok', marker: '', value: [0, 1, 2] }]), 'scatter clamped value');
});

test('heatmap tooltip escapes an unknown unit_system in the value', () => {
    const fmtSpec = { opts: { title: 't', format: { unit_system: PAYLOAD } } };
    assertEscaped(heatmapTooltip(fmtSpec, { data: [1000, 0, 0, null, 1] }), 'heatmap value');
    assertEscaped(heatmapTooltip(fmtSpec, { data: [1000, 0, 0, 1, 2] }), 'heatmap min-max value');
    assertEscaped(heatmapTooltip({
        ...fmtSpec,
        diffMatrices: { baseline: [[1, 2]], experiment: [[3, 4]] },
        diffCaptureLabels: { baseline: 'a', experiment: 'b' },
    }, { data: [1000, 0, 0, null, 1] }), 'diff heatmap values');
});

test('quantile heatmap tooltip escapes an unknown unit_system in every branch', () => {
    const base = { opts: { title: 't', format: { unit_system: PAYLOAD } }, series_names: ['0.5'] };
    assertEscaped(quantileTooltip(base, { data: [1000, 0, 5] }), 'quantile value');
    assertEscaped(quantileTooltip({
        ...base,
        diffMatrices: { baseline: [[1, 2]], experiment: [[3, 4]] },
        diffCaptureLabels: { baseline: 'a', experiment: 'b' },
    }, { data: [1000, 0, 5] }), 'quantile diff values');
    assertEscaped(quantileTooltip({
        ...base,
        compareCounterpartData: { data: [[1, 2], [7, 8]] },
        compareCaptureLabels: { baseline: 'a', experiment: 'b' },
        compareSelfRole: 'baseline',
    }, { data: [1000, 0, 5] }), 'quantile compare-pair values');
});

test('heatmap tooltip escapes an unknown unit_system in the clamped raw values', () => {
    const fmtSpec = { opts: { title: 't', format: { unit_system: PAYLOAD, range: { min: 1.5, max: 1.6 } } } };
    assertEscaped(heatmapTooltip(fmtSpec, { data: [1000, 0, 0, null, 2] }), 'heatmap clamped raw value');
    assertEscaped(heatmapTooltip(fmtSpec, { data: [1000, 0, 0, 1, 2] }), 'heatmap clamped raw min-max');
});
