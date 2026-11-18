// The dashboard fetch loop's display path handles a +Inf quantile as the
// JSON path does (data.js applyDisplayResult): through the real
// processDashboardData, with the display response a binary body as
// crates/dashboard/src/display_wire.rs encodes it, a percentile chart gets
// its out-of-bounds range and a line chart a gap.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
    createDataApi, setDisplayMode, setStepOverride, clearDisplayTiles,
} from '../src/viewer/assets/lib/data.js';

// [u32 LE headerLen][JSON header][pad to 8B][six f64 LE columns per series]
const displayBody = (series) => {
    const header = new TextEncoder().encode(JSON.stringify({
        resultType: 'matrix',
        budget: 100,
        series: series.map(({ metric, cols }) => ({ metric, n: cols.t.length })),
    }));
    const start = Math.ceil((4 + header.length) / 8) * 8;
    const n = series.reduce((a, s) => a + 6 * s.cols.t.length, 0);
    const buf = new ArrayBuffer(start + n * 8);
    new DataView(buf).setUint32(0, header.length, true);
    new Uint8Array(buf, 4).set(header);
    const f = new Float64Array(buf, start);
    let i = 0;
    for (const { cols } of series) {
        for (const c of ['t', 'min', 'lo', 'median', 'hi', 'max']) for (const v of cols[c]) f[i++] = v;
    }
    return buf;
};

const column = (values) => ({
    t: [1, 2, 3], min: values, lo: values, median: values, hi: values, max: values,
});

const body = (query) => displayBody(query.startsWith('histogram_quantiles')
    ? [
        { metric: { quantile: '0.5' }, cols: column([1, 1, 1]) },
        { metric: { quantile: '0.99' }, cols: column([5, Infinity, 4]) },
    ]
    : [{ metric: {}, cols: column([1, Infinity, 3]) }]);

const api = () => createDataApi({
    getMetadata: async () => ({ status: 'success', data: { minTime: 0, maxTime: 3, interval: 1 } }),
    queryRange: async () => { throw new Error('the display path was not taken'); },
});

test('the fetch loop gives a display +Inf the out-of-bounds range, or a gap', async () => {
    const saved = globalThis.fetch;
    globalThis.fetch = async (url) => {
        const query = new URL(url, 'http://x').searchParams.get('query');
        const buf = body(query);
        return {
            ok: true,
            headers: { get: () => 'application/octet-stream' },
            arrayBuffer: async () => buf,
        };
    };
    setDisplayMode(true);
    setStepOverride(null);
    clearDisplayTiles();
    try {
        const percentile = {
            promql_query: 'latency',
            opts: { id: 'p', type: 'histogram', subtype: 'percentiles' },
        };
        const line = { promql_query: 'load', opts: { id: 'l', type: 'gauge' } };
        await api().processDashboardData(
            { groups: [{ name: 'g', subgroups: [{ plots: [percentile, line] }] }] }, null, '/x');
        assert.equal(percentile._resolvedStyle, 'scatter');
        assert.equal(percentile.boxplot[1].max[1], Infinity);
        assert.equal(percentile.opts.format.range.max, 5);
        assert.equal(line._resolvedStyle, 'line');
        assert.ok(Number.isNaN(line.data[1][1]), String(line.data[1]));
        assert.equal(line.opts.format, undefined);
    } finally {
        globalThis.fetch = saved;
        setDisplayMode(false);
        clearDisplayTiles();
    }
});
