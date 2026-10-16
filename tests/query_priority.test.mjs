import test from 'node:test';
import assert from 'node:assert/strict';
import { createDataApi } from '../src/viewer/assets/lib/data.js';

// A section's counter and gauge charts are queried before its histogram
// charts, and the section can render between the two: the histogram
// charts are marked pending until their own results land.

const minTime = 1_700_000_000;

function section() {
    const counter = {
        promql_query: 'irate(cpu_cycles[5m])',
        opts: { id: 'cycles', title: 'Cycles', type: 'delta_counter' },
    };
    const gauge = {
        promql_query: 'memory_used',
        opts: { id: 'mem', title: 'Memory', type: 'gauge' },
    };
    const histogram = {
        promql_query: 'syscall_latency',
        opts: { id: 'lat', title: 'Latency', type: 'histogram', subtype: 'percentiles', percentiles: [0.5, 0.99] },
    };
    return {
        data: { groups: [{ name: 'g', subgroups: [{ name: 's', plots: [counter, gauge, histogram] }] }] },
        counter,
        gauge,
        histogram,
    };
}

const matrix = (labels = {}) => ({
    status: 'success',
    data: { resultType: 'matrix', result: [{ metric: labels, values: [[minTime, '1'], [minTime + 1, '2']] }] },
});

test('histogram queries are sent after the counter and gauge results land', async () => {
    const events = [];
    const api = createDataApi({
        getMetadata: async () => ({ status: 'success', data: { minTime, maxTime: minTime + 60, interval: 1 } }),
        queryRange: async (query) => {
            events.push(`sent ${query}`);
            await new Promise((r) => setTimeout(r, 5));
            events.push(`done ${query}`);
            return query.includes('histogram_')
                ? matrix({ quantile: '0.5' })
                : matrix();
        },
        logHeatmapErrors: false,
    });
    const { data, histogram } = section();
    let pendingAtPrimary = null;
    let doneAtPrimary = null;
    await api.processDashboardData(data, null, '/overview', {
        onPrimary: () => {
            pendingAtPrimary = histogram.pending;
            doneAtPrimary = events.filter((e) => e.startsWith('done')).length;
        },
    });

    const firstHistogram = events.findIndex((e) => e.startsWith('sent') && e.includes('histogram_'));
    const lastOther = events.reduce(
        (at, e, i) => (e.startsWith('done') && !e.includes('histogram_') ? i : at),
        -1,
    );
    assert.ok(firstHistogram > lastOther, events.join('\n'));
    assert.equal(pendingAtPrimary, true, 'the histogram chart is pending when the section first renders');
    assert.equal(doneAtPrimary, 2, 'the counter and the gauge had landed');
    assert.equal(histogram.pending, false, 'pending is cleared once its result lands');
});

test('a section without histogram charts does not call onPrimary', async () => {
    const api = createDataApi({
        getMetadata: async () => ({ status: 'success', data: { minTime, maxTime: minTime + 60, interval: 1 } }),
        queryRange: async () => matrix(),
        logHeatmapErrors: false,
    });
    const { data } = section();
    data.groups[0].subgroups[0].plots.pop();
    let called = false;
    await api.processDashboardData(data, null, '/overview', { onPrimary: () => { called = true; } });
    assert.equal(called, false);
});

test('a histogram chart that already has data is not marked pending on a refetch', async () => {
    const api = createDataApi({
        getMetadata: async () => ({ status: 'success', data: { minTime, maxTime: minTime + 60, interval: 1 } }),
        queryRange: async (query) => (query.includes('histogram_') ? matrix({ quantile: '0.5' }) : matrix()),
        logHeatmapErrors: false,
    });
    const { data, histogram } = section();
    await api.processDashboardData(data, null, '/overview');
    let pendingDuringRefetch = null;
    await api.processDashboardData(data, null, '/overview', {
        onPrimary: () => { pendingDuringRefetch = histogram.pending; },
    });
    assert.equal(pendingDuringRefetch, false, 'the chart keeps its data on screen');
});

test('a failed histogram query clears pending', async () => {
    const api = createDataApi({
        getMetadata: async () => ({ status: 'success', data: { minTime, maxTime: minTime + 60, interval: 1 } }),
        queryRange: async (query) => {
            if (query.includes('histogram_')) throw new Error('boom');
            return matrix();
        },
        logHeatmapErrors: false,
    });
    const { data, histogram } = section();
    const error = console.error;
    console.error = () => {};
    try {
        await api.processDashboardData(data, null, '/overview');
    } finally {
        console.error = error;
    }
    assert.equal(histogram.pending, false);
});

test('without onPrimary every query is sent at once', async () => {
    const events = [];
    const api = createDataApi({
        getMetadata: async () => ({ status: 'success', data: { minTime, maxTime: minTime + 60, interval: 1 } }),
        queryRange: async (query) => {
            events.push(`sent ${query}`);
            await new Promise((r) => setTimeout(r, 5));
            events.push(`done ${query}`);
            return matrix();
        },
        logHeatmapErrors: false,
    });
    const { data } = section();
    await api.processDashboardData(data, null, '/overview');
    const firstDone = events.findIndex((e) => e.startsWith('done'));
    const histogramSent = events.findIndex((e) => e.startsWith('sent') && e.includes('histogram_'));
    assert.ok(histogramSent < firstDone, events.join('\n'));
});

test('a subgroup whose counters came back empty holds back its histogram charts until they land', async () => {
    const api = createDataApi({
        getMetadata: async () => ({ status: 'success', data: { minTime, maxTime: minTime + 60, interval: 1 } }),
        queryRange: async (query) => (query.includes('histogram_')
            ? matrix({ quantile: '0.5' })
            : { status: 'success', data: { resultType: 'matrix', result: [] } }),
        logHeatmapErrors: false,
    });
    const { data, counter, gauge, histogram } = section();
    let atPrimary = null;
    await api.processDashboardData(data, null, '/overview', {
        onPrimary: (partial) => {
            const plots = partial.groups.flatMap((g) => g.subgroups.flatMap((sg) => sg.plots));
            atPrimary = { plots, pending: histogram.pending };
        },
    });
    // The empty counter and gauge are gone; the histogram chart, still in
    // flight, stays in the section, held back rather than drawn as pending.
    assert.deepEqual(atPrimary.plots, [histogram]);
    assert.equal(atPrimary.pending, 'hidden');
    // Its result lands in the section.
    assert.equal(histogram.pending, false);
    const plots = data.groups.flatMap((g) => g.subgroups.flatMap((sg) => sg.plots));
    assert.deepEqual(plots, [histogram]);
    assert.ok(histogram.data.length > 0);
    const titles = data.metadata.unavailable_charts.map((c) => c.title);
    assert.deepEqual(titles.sort(), [counter.opts.title, gauge.opts.title].sort());
});

test('the rule is per subgroup: a subgroup with data keeps its placeholders', async () => {
    const api = createDataApi({
        getMetadata: async () => ({ status: 'success', data: { minTime, maxTime: minTime + 60, interval: 1 } }),
        queryRange: async (query) => {
            if (query.includes('histogram_')) return matrix({ quantile: '0.5' });
            if (query.includes('empty_rate')) return { status: 'success', data: { resultType: 'matrix', result: [] } };
            return matrix();
        },
        logHeatmapErrors: false,
    });
    const full = section();
    const emptyCounter = { promql_query: 'irate(empty_rate[5m])', opts: { id: 'e', title: 'Empty', type: 'delta_counter' } };
    const heldHistogram = { promql_query: 'other_latency', opts: { id: 'h2', title: 'Other latency', type: 'histogram', subtype: 'percentiles', percentiles: [0.5] } };
    full.data.groups[0].subgroups.push({ name: 'empty', plots: [emptyCounter, heldHistogram] });
    let seen = null;
    await api.processDashboardData(full.data, null, '/overview', {
        onPrimary: () => { seen = [full.histogram.pending, heldHistogram.pending]; },
    });
    assert.deepEqual(seen, [true, 'hidden']);
});

test('a held-back chart that comes back empty is never drawn', async () => {
    const seen = [];
    const api = createDataApi({
        getMetadata: async () => ({ status: 'success', data: { minTime, maxTime: minTime + 60, interval: 1 } }),
        queryRange: async (query) => {
            if (query.includes('empty')) return { status: 'success', data: { resultType: 'matrix', result: [] } };
            return query.includes('histogram_') ? matrix({ quantile: '0.5' }) : matrix();
        },
        logHeatmapErrors: false,
    });
    const full = section();
    const emptyCounter = { promql_query: 'irate(empty_rate[5m])', opts: { id: 'e', title: 'Empty', type: 'delta_counter' } };
    const emptyHistogram = { promql_query: 'empty_latency', opts: { id: 'h3', title: 'Empty latency', type: 'histogram', subtype: 'percentiles', percentiles: [0.5] } };
    full.data.groups[0].subgroups.push({ name: 'empty', plots: [emptyCounter, emptyHistogram] });
    // Redraws before onPrimary draw nothing: the section is not shown yet.
    let shown = false;
    const origRedraw = globalThis.m;
    globalThis.m = { redraw: () => { if (shown) seen.push(emptyHistogram.pending); } };
    try {
        await api.processDashboardData(full.data, null, '/overview', { onPrimary: () => { shown = true; } });
    } finally {
        globalThis.m = origRedraw;
    }
    assert.ok(seen.length > 0 && seen.every((p) => p === 'hidden'), JSON.stringify(seen));
    const plots = full.data.groups.flatMap((g) => g.subgroups.flatMap((sg) => sg.plots));
    assert.ok(!plots.includes(emptyHistogram));
    assert.ok(plots.every((p) => !p.pending));
});
