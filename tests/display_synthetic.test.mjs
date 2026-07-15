// Backend-data validation of the display pipeline against SYNTHETIC data with
// known properties (see examples/gen_display_testdata.rs). Asserts the core
// decimation guarantees exactly. Skipped unless VIEWER_URL points at a viewer
// serving the synthetic baseline parquet — the tests/display_synthetic.sh
// harness generates the data, starts the viewer, and sets the env.
//
// A/B assertions run only when VIEWER_URL is a compare-mode viewer (baseline +
// experiment loaded), signalled by COMPARE=1.
import { test } from 'node:test';
import assert from 'node:assert';
import { decodeDisplayBinary, decodeHeatmapBinary } from '../src/viewer/assets/lib/data.js';

const BASE = process.env.VIEWER_URL;
const skip = BASE ? false : 'set VIEWER_URL (see tests/display_synthetic.sh)';
const compareSkip = process.env.COMPARE ? skip : 'compare-mode viewer only';

// Recording spans [START, START+3600) at 1 s; gauge spikes to 900 at +907/1823/
// 2731 (off the sampling grid), latency tail spikes at +1237/2411.
const START = 1_700_000_000;
const END = START + 3600;
const GAUGE_SPIKE_ABS = START + 907;

const qs = (query, { start = START, end = END, points = 500, step = 1, capture } = {}) => {
    const p = new URLSearchParams({ query, start: String(start), end: String(end), step: String(step) });
    if (capture) p.set('capture', capture);
    return p;
};

const display = async (query, opts = {}) => {
    const p = qs(query, opts);
    p.set('format', 'display');
    p.set('points', String(opts.points ?? 500));
    const res = await fetch(`${BASE}/api/v1/query_range?${p}`);
    const ct = res.headers.get('content-type') || '';
    if (!ct.includes('octet-stream')) return { json: await res.json() };
    return decodeHeatmapOrSeries(await res.arrayBuffer());
};

// Both display series and heatmaps come back as octet-stream from format=display;
// dispatch on the header resultType.
const decodeHeatmapOrSeries = (buf) => {
    const dv = new DataView(buf);
    const hl = dv.getUint32(0, true);
    const rt = JSON.parse(new TextDecoder().decode(new Uint8Array(buf, 4, hl))).resultType;
    return rt === 'histogram_heatmap' ? decodeHeatmapBinary(buf) : decodeDisplayBinary(buf);
};

const arr = (a) => Array.from(a);

test('gauge spike survives decimation (min/max envelope)', { skip }, async () => {
    const d = await display('synth_gauge', { points: 500 });
    const s = d.series[0];
    assert.ok(s.decimated, 'expected decimated (3600 native > 500 budget)');
    assert.ok(s.n <= 500, `n=${s.n} within budget`);
    const max = arr(s.max), med = arr(s.median);
    assert.equal(Math.max(...max), 900, 'max envelope preserves the 900 spike a point-sample would step over');
    const spikeBuckets = max.filter((v) => v >= 850).length;
    assert.ok(spikeBuckets >= 3 && spikeBuckets <= 6, `~3 spike buckets (one per spike), got ${spikeBuckets}`);
    assert.ok(Math.max(...med) <= 200, `median stays near the 100 floor (spike is 1 sample/bucket), got ${Math.max(...med)}`);
});

test('envelope ordering min<=lo<=median<=hi<=max holds everywhere', { skip }, async () => {
    const d = await display('synth_gauge', { points: 500 });
    const s = d.series[0];
    const E = 1e-6;
    for (let i = 0; i < s.n; i++) {
        assert.ok(
            s.min[i] <= s.lo[i] + E && s.lo[i] <= s.median[i] + E
            && s.median[i] <= s.hi[i] + E && s.hi[i] <= s.max[i] + E,
            `ordering violated at ${i}: ${s.min[i]},${s.lo[i]},${s.median[i]},${s.hi[i]},${s.max[i]}`,
        );
    }
});

test('point budget is honored (n scales with points)', { skip }, async () => {
    const a = await display('synth_gauge', { points: 300 });
    const b = await display('synth_gauge', { points: 500 });
    assert.ok(a.series[0].n <= 300, `points=300 -> n=${a.series[0].n}`);
    assert.ok(b.series[0].n <= 500 && b.series[0].n > 300, `points=500 -> n=${b.series[0].n} (finer than 300)`);
});

test('narrow window refetch is finer and shows the raw spike', { skip }, async () => {
    const win = await display('synth_gauge', { start: GAUGE_SPIKE_ABS - 100, end: GAUGE_SPIKE_ABS + 100, points: 500 });
    const s = win.series[0];
    // 200 native points <= 500 budget → native resolution, not decimated.
    assert.equal(s.decimated, false, 'a 200 s window fits the budget → native resolution');
    assert.ok(s.n > 100, `window returns ~native points, got ${s.n}`);
    assert.equal(Math.max(...arr(s.max)), 900, 'the spike is present at native resolution in the window');
});

test('a spike-free window has a flat envelope near the floor', { skip }, async () => {
    const win = await display('synth_gauge', { start: START + 100, end: START + 300, points: 500 });
    const s = win.series[0];
    assert.ok(Math.max(...arr(s.max)) <= 150, `no spike in [100,300]s → max stays near floor, got ${Math.max(...arr(s.max))}`);
});

test('latency percentiles are ordered and plausible (p50~1ms << p99)', { skip }, async () => {
    const d = await display('histogram_quantiles([0.5, 0.9, 0.99], synth_latency)', { points: 500 });
    assert.ok(d.series.length >= 3, `3 percentile series, got ${d.series.length}`);
    const medOf = (i) => arr(d.series[i].median);
    const meanMed = (i) => medOf(i).reduce((x, y) => x + y, 0) / medOf(i).length;
    const p50 = meanMed(0), p99 = meanMed(2);
    assert.ok(p50 > 0.5e6 && p50 < 2e6, `p50 ~1ms in ns, got ${p50}`);
    assert.ok(p99 > p50, `p99 (${p99}) above p50 (${p50})`);
});

test('histogram bucket heatmap binary decodes with real cells', { skip }, async () => {
    const d = await display('histogram_heatmap(synth_latency)', { points: 500 });
    assert.ok(d.time_data.length > 0, 'has time columns');
    assert.ok(d.data.length > 0, 'has non-zero cells');
    assert.ok(d.bucket_bounds.length > 0, 'has bucket bounds');
    assert.ok(d.max_value > 0, 'has a positive max count for color scaling');
});

test('A/B: experiment regression is detectable (higher gauge floor)', { skip: compareSkip }, async () => {
    const b = await display('synth_gauge', { capture: 'baseline', points: 500 });
    const e = await display('synth_gauge', { capture: 'experiment', points: 500 });
    const floor = (d) => Math.min(...arr(d.series[0].median));
    assert.ok(floor(e) > floor(b), `experiment floor (${floor(e)}) above baseline (${floor(b)})`);
});
