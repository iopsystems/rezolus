// The Mean/Total toggle on a display-mode line chart. Such a chart renders
// from `spec.boxplot` (the envelope) whenever it is non-empty, so the toggle
// has to carry `boxplot` and `boxplotDecimated` along with `data`: Total is a
// plain JSON line with no envelope, and Mean gets its envelope back.
import test from 'node:test';
import assert from 'node:assert/strict';

// The chart modules read CSS custom properties at load; stub the DOM so their
// palettes fall back to literals (same shim the other chart tests use).
globalThis.document = globalThis.document || { documentElement: {} };
globalThis.getComputedStyle =
    globalThis.getComputedStyle || (() => ({ getPropertyValue: () => '' }));

const { toggleTotal } = await import('../src/viewer/assets/lib/charts/line.js');

const MEAN_BOXPLOT = [{
    t: [1, 2], min: [0, 0], lo: [1, 1], median: [2, 2], hi: [3, 3], max: [4, 4],
    decimated: true,
}];

const totalResult = () => ({
    status: 'success',
    data: {
        resultType: 'matrix',
        result: [{ metric: { __name__: 'x' }, values: [[1, '10'], [2, '20']] }],
    },
});

function stubChart() {
    const meanData = [[1, 2], [2, 2]];
    return {
        spec: {
            opts: { type: 'line', style: 'line' },
            promql_query_total: 'sum(rate(x[1m]))',
            data: meanData,
            boxplot: MEAN_BOXPLOT,
            boxplotDecimated: true,
        },
        domNode: null,
        configured: 0,
        configureChartByType() { this.configured++; },
    };
}

const hasEnvelope = (spec) => Array.isArray(spec.boxplot) && spec.boxplot.length > 0;

test('Mean -> Total -> Mean, then cached toggles, carry the envelope', async () => {
    const chart = stubChart();
    const meanData = chart.spec.data;
    let fetches = 0;
    const query = async () => { fetches++; return totalResult(); };

    await toggleTotal(chart, query);
    assert.equal(chart._showTotal, true);
    assert.ok(!hasEnvelope(chart.spec), 'Total renders as a plain line');
    assert.equal(chart.spec.boxplotDecimated, false);
    assert.notEqual(chart.spec.data, meanData);
    const totalData = chart.spec.data;

    await toggleTotal(chart, query);
    assert.equal(chart._showTotal, false);
    assert.equal(chart.spec.boxplot, MEAN_BOXPLOT, 'Mean gets its envelope back');
    assert.equal(chart.spec.boxplotDecimated, true);
    assert.equal(chart.spec.data, meanData);

    for (let i = 0; i < 2; i++) {
        await toggleTotal(chart, query);
        assert.equal(chart._showTotal, true);
        assert.ok(!hasEnvelope(chart.spec));
        assert.equal(chart.spec.data, totalData);

        await toggleTotal(chart, query);
        assert.equal(chart._showTotal, false);
        assert.equal(chart.spec.boxplot, MEAN_BOXPLOT);
        assert.equal(chart.spec.boxplotDecimated, true);
        assert.equal(chart.spec.data, meanData);
    }
    assert.equal(fetches, 1, 'later toggles come from the cache');
});

test('a failed Total fetch keeps the Mean envelope', async () => {
    const chart = stubChart();
    const warn = console.warn;
    console.warn = () => {};
    try {
        await toggleTotal(chart, async () => { throw new Error('boom'); });
    } finally {
        console.warn = warn;
    }
    assert.equal(chart._showTotal, undefined);
    assert.equal(chart._totalPending, false);
    assert.equal(chart.spec.boxplot, MEAN_BOXPLOT);
    assert.equal(chart.spec.boxplotDecimated, true);
});

test('an empty Total result keeps the Mean envelope', async () => {
    const chart = stubChart();
    await toggleTotal(chart, async () => ({ status: 'success', data: { result: [] } }));
    assert.equal(chart._showTotal, undefined);
    assert.equal(chart.spec.boxplot, MEAN_BOXPLOT);
    assert.equal(chart.spec.boxplotDecimated, true);
    assert.equal(chart._meanDataCache, null);
});
