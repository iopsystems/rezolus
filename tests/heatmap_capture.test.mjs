// Coverage for the heatmap-range fetch helpers in src/viewer/assets/lib/data.js.
// Stubs ViewerApi.heatmapRange so the helpers run without a backend.

import test from 'node:test';
import assert from 'node:assert/strict';

// `data.js` reads ViewerApi off a singleton, so the test stubs it
// before the module is imported. Per-test cleanup restores the
// original to avoid cross-test bleed.
import { ViewerApi } from '../src/viewer/assets/lib/viewer_api.js';

const originalHeatmapRange = ViewerApi.heatmapRange;

const data = await import('../src/viewer/assets/lib/data.js');
const {
    fetchHeatmapForPlot,
    fetchHeatmapsForGroups,
    fetchQuantileSpectrumForPlot,
    setSelectedNode,
} = data;

const restore = () => {
    ViewerApi.heatmapRange = originalHeatmapRange;
    setSelectedNode(null);
};

test('fetchHeatmapForPlot returns null when plot has no metric tag', async () => {
    ViewerApi.heatmapRange = async () => { throw new Error('should not be called'); };
    const result = await fetchHeatmapForPlot({ opts: { type: 'histogram', id: 'x' } });
    assert.equal(result, null);
    restore();
});

test('fetchHeatmapForPlot forwards metric, kind, node, and captureId', async () => {
    let received;
    ViewerApi.heatmapRange = async (args) => {
        received = args;
        return { time_data: [1, 2], bucket_bounds: [0, 1, 2], data: [], min_value: 0, max_value: 0 };
    };
    setSelectedNode('alpha');
    const plot = { opts: { type: 'histogram', id: 'x', metric: 'syscall_latency/read' } };
    await fetchHeatmapForPlot(plot, { captureId: 'experiment' });
    assert.deepEqual(received, {
        metric: 'syscall_latency/read',
        kind: 'buckets',
        captureId: 'experiment',
        node: 'alpha',
    });
    restore();
});

test('fetchHeatmapsForGroups skips non-histogram plots and unannotated histograms', async () => {
    const seen = [];
    ViewerApi.heatmapRange = async ({ metric }) => {
        seen.push(metric);
        return { time_data: [0], bucket_bounds: [0], data: [], min_value: 0, max_value: 0 };
    };
    const groups = [
        {
            subgroups: [
                { plots: [
                    { opts: { type: 'counter', id: 'a' } },                             // skipped
                    { opts: { type: 'histogram', id: 'b' } },                            // skipped (no metric)
                    { opts: { type: 'histogram', id: 'c', metric: 'tcp_packet_latency' } }, // included
                ] },
            ],
        },
        {
            plots: [
                { opts: { type: 'histogram', id: 'd', metric: 'scheduler_runqueue_latency' } },
            ],
        },
    ];
    const out = await fetchHeatmapsForGroups(groups);
    assert.deepEqual(seen.sort(), ['scheduler_runqueue_latency', 'tcp_packet_latency']);
    assert.equal(out.size, 2);
    assert.ok(out.has('c'));
    assert.ok(out.has('d'));
    restore();
});

test('fetchQuantileSpectrumForPlot defaults to a 100-quantile spectrum including p0', async () => {
    let received;
    ViewerApi.heatmapRange = async (args) => {
        received = args;
        return {
            time_data: [0, 1],
            data: [[0.5, 0.5]],
            series_names: ['p100'],
            color_min_anchor: [0, 0],
        };
    };
    const plot = { opts: { type: 'histogram', id: 'q', metric: 'syscall_latency/read' } };
    const result = await fetchQuantileSpectrumForPlot(plot);
    assert.equal(received.kind, 'quantile_spectrum');
    assert.equal(received.quantiles[0], 0.0);
    assert.equal(received.quantiles.length, 101);
    // Response shape passes through verbatim — same fields
    // `quantile_heatmap.js` consumes.
    assert.deepEqual(result.color_min_anchor, [0, 0]);
    assert.deepEqual(result.series_names, ['p100']);
    restore();
});

test('fetchQuantileSpectrumForPlot honors a user-supplied quantile list', async () => {
    let received;
    ViewerApi.heatmapRange = async (args) => {
        received = args;
        return {
            time_data: [0],
            data: [[1], [2]],
            series_names: ['p50', 'p99'],
            color_min_anchor: null,
        };
    };
    const plot = { opts: { type: 'histogram', id: 'q', metric: 'tcp_packet_latency' } };
    await fetchQuantileSpectrumForPlot(plot, { quantiles: [0.5, 0.99] });
    assert.deepEqual(received.quantiles, [0.5, 0.99]);
    restore();
});
