import test from 'node:test';
import assert from 'node:assert/strict';

// The chart modules (metriken-charts' charts/) fetch through charts/host.js.
// Loading the viewer's data.js registers its data layer there.

test('loading data.js registers the viewer data layer', async () => {
    const host = await import('../src/viewer/assets/lib/charts/host.js');
    const data = await import('../src/viewer/assets/lib/data.js');
    const h = host.chartHost();
    assert.equal(h.executePromQLRangeQuery, data.executePromQLRangeQuery);
    assert.equal(h.applyResultToPlot, data.applyResultToPlot);
    assert.equal(h.fetchQuantileSpectrumForPlot, data.fetchQuantileSpectrumForPlot);
    data.setStepOverride(30);
    try {
        assert.equal(h.getStepOverride(), 30);
    } finally {
        data.setStepOverride(null);
    }
});
