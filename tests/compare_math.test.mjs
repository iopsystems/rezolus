import test from 'node:test';
import assert from 'node:assert/strict';
import {
    nullDiff,
    intersectLabels,
    unifyHistogramRange,
    buildDeltaSpectrum,
    composeScatterLabel,
} from '../src/viewer/assets/lib/charts/util/compare_math.js';

test('nullDiff: numbers', () => {
    assert.equal(nullDiff(5, 3), 2);
    assert.equal(nullDiff(0, 0), 0);
    assert.equal(nullDiff(-1, 1), -2);
});

test('nullDiff: null propagates from either side', () => {
    assert.equal(nullDiff(null, 3), null);
    assert.equal(nullDiff(5, null), null);
    assert.equal(nullDiff(null, null), null);
});

test('nullDiff: undefined treated same as null', () => {
    assert.equal(nullDiff(undefined, 3), null);
    assert.equal(nullDiff(5, undefined), null);
});

test('nullDiff: NaN treated as null', () => {
    assert.equal(nullDiff(Number.NaN, 3), null);
    assert.equal(nullDiff(5, Number.NaN), null);
});

test('intersectLabels: common subset', () => {
    const a = new Set(['a', 'b', 'c']);
    const b = new Set(['b', 'c', 'd']);
    assert.deepEqual([...intersectLabels(a, b)].sort(), ['b', 'c']);
});

test('intersectLabels: disjoint sets yield empty', () => {
    assert.deepEqual([...intersectLabels(new Set(['x']), new Set(['y']))], []);
});

const fakeSpectrum = (cols) => ({
    data: [[/* times unused */ 0, 1, 2], ...cols],
});

test('unifyHistogramRange: anchors win when present, else natural min', () => {
    const a = { ...fakeSpectrum([[1, 2, 3], [4, 5, 6]]), color_min_anchor: 0.5 };
    const b = { ...fakeSpectrum([[2, 3, 4], [5, 6, 7]]), color_min_anchor: 0.7 };
    const r = unifyHistogramRange(a, b);
    assert.equal(r.colorMin, 0.5);  // min(0.5, 0.7)
    assert.equal(r.colorMax, 7);    // max(6, 7)
});

test('unifyHistogramRange: missing anchor falls back to scanned min', () => {
    const a = { ...fakeSpectrum([[2, 3, 4]]), color_min_anchor: null };
    const b = { ...fakeSpectrum([[1, 5, 6]]), color_min_anchor: null };
    const r = unifyHistogramRange(a, b);
    assert.equal(r.colorMin, 1);
    assert.equal(r.colorMax, 6);
});

test('unifyHistogramRange: skips null/NaN/non-positive cells in scan', () => {
    const a = { ...fakeSpectrum([[null, 0, -1, 5]]), color_min_anchor: null };
    const b = { ...fakeSpectrum([[NaN, 2, null, 8]]), color_min_anchor: null };
    const r = unifyHistogramRange(a, b);
    assert.equal(r.colorMin, 2);
    assert.equal(r.colorMax, 8);
});

test('unifyHistogramRange: empty data falls back to (0, 1)', () => {
    const a = { data: [[]], color_min_anchor: null };
    const b = { data: [[]], color_min_anchor: null };
    const r = unifyHistogramRange(a, b);
    assert.equal(r.colorMin, 0);
    assert.equal(r.colorMax, 1);
});

test('unifyHistogramRange: collapsed range gets a non-zero ceiling', () => {
    const a = { ...fakeSpectrum([[5, 5, 5]]), color_min_anchor: 5 };
    const b = { ...fakeSpectrum([[5, 5, 5]]), color_min_anchor: 5 };
    const r = unifyHistogramRange(a, b);
    assert.equal(r.colorMin, 5);
    assert.ok(r.colorMax > r.colorMin);  // padded so log-scale doesn't collapse
});

test('unifyHistogramRange: asymmetric anchors — capture without anchor still pulls colorMin via its own scanned min', () => {
    const a = { ...fakeSpectrum([[10, 20, 30]]), color_min_anchor: 10 };
    const b = { ...fakeSpectrum([[0.1, 5, 8]]), color_min_anchor: null };
    const r = unifyHistogramRange(a, b);
    // B's natural min (0.1) is lower than A's anchor (10); colorMin
    // must be 0.1 so B's bottom cells aren't clipped on the shared scale.
    assert.equal(r.colorMin, 0.1);
    assert.equal(r.colorMax, 30);
});

test('unifyHistogramRange: asymmetric anchors, reversed — anchor on B, scan on A', () => {
    const a = { ...fakeSpectrum([[2, 5, 9]]), color_min_anchor: null };
    const b = { ...fakeSpectrum([[100, 200, 300]]), color_min_anchor: 100 };
    const r = unifyHistogramRange(a, b);
    assert.equal(r.colorMin, 2);   // A's natural min
    assert.equal(r.colorMax, 300); // B's natural max
});

const spectrum = (times, qSeries, names) => ({
    time_data: times,
    data: [times, ...qSeries],
    series_names: names,
});

test('buildDeltaSpectrum: per-cell experiment − baseline', () => {
    const baseline = spectrum([0, 1], [[1, 2], [3, 4]], ['p50', 'p99']);
    const experiment = spectrum([0, 1], [[2, 5], [4, 7]], ['p50', 'p99']);
    const r = buildDeltaSpectrum(baseline, experiment);
    assert.deepEqual(r.data[0], [0, 1]);
    assert.deepEqual(r.data[1], [1, 3]);   // p50 deltas: 2−1, 5−2
    assert.deepEqual(r.data[2], [1, 3]);   // p99 deltas: 4−3, 7−4
    assert.deepEqual(r.series_names, ['p50', 'p99']);
});

test('buildDeltaSpectrum: dMin/dMax over non-null deltas', () => {
    const baseline = spectrum([0, 1], [[1, 5]], ['p50']);
    const experiment = spectrum([0, 1], [[2, 3]], ['p50']);
    const r = buildDeltaSpectrum(baseline, experiment);
    assert.equal(r.dMin, -2);  // 3 − 5
    assert.equal(r.dMax, 1);   // 2 − 1
});

test('buildDeltaSpectrum: null on either side propagates', () => {
    const baseline = spectrum([0, 1], [[null, 2]], ['p50']);
    const experiment = spectrum([0, 1], [[5, null]], ['p50']);
    const r = buildDeltaSpectrum(baseline, experiment);
    assert.deepEqual(r.data[1], [null, null]);
    assert.equal(r.dMin, null);
    assert.equal(r.dMax, null);
});

test('buildDeltaSpectrum: returns matrices keyed by qIdx then tIdx for tooltip lookup', () => {
    const baseline = spectrum([0, 1], [[1, 2], [3, 4]], ['p50', 'p99']);
    const experiment = spectrum([0, 1], [[2, 5], [4, 7]], ['p50', 'p99']);
    const r = buildDeltaSpectrum(baseline, experiment);
    assert.equal(r.matrices.baseline[0][1], 2);    // p50 at t=1 → 2
    assert.equal(r.matrices.experiment[1][0], 4);  // p99 at t=0 → 4
});

test('buildDeltaSpectrum: undetermined step (single-sample side) returns null', () => {
    const baseline = spectrum([0, 1], [[1, 2]], ['p50']);
    const experiment = spectrum([0], [[5]], ['p50']);
    const r = buildDeltaSpectrum(baseline, experiment);
    assert.equal(r, null);
});

test('buildDeltaSpectrum: timestamp count mismatch with matching step truncates to common prefix', () => {
    // Captures with matching step but different lengths pair by index
    // after compare-mode rebases each to its own first sample.
    const baseline = spectrum([100, 101, 102, 103], [[1, 2, 3, 4]], ['p50']);
    const experiment = spectrum([200, 201, 202, 203, 204, 205], [[10, 12, 14, 16, 18, 20]], ['p50']);
    const r = buildDeltaSpectrum(baseline, experiment);
    assert.notEqual(r, null);
    assert.equal(r.data[1].length, 4);
    assert.deepEqual(r.data[1], [9, 10, 11, 12]);
    assert.deepEqual(r.time_data, [100, 101, 102, 103]);
    assert.equal(r.dMin, 9);
    assert.equal(r.dMax, 12);
});

test('buildDeltaSpectrum: experiment shorter than baseline truncates to experiment length', () => {
    const baseline = spectrum([0, 1, 2, 3, 4], [[1, 2, 3, 4, 5]], ['p50']);
    const experiment = spectrum([0, 1, 2], [[10, 12, 14]], ['p50']);
    const r = buildDeltaSpectrum(baseline, experiment);
    assert.notEqual(r, null);
    assert.equal(r.data[1].length, 3);
    assert.deepEqual(r.data[1], [9, 10, 11]);
    assert.deepEqual(r.time_data, [0, 1, 2]);
});

test('buildDeltaSpectrum: mismatched step refuses to pair samples', () => {
    // Same index → different relative time when steps differ; refuse
    // rather than produce a nonsense diff.
    const baseline = spectrum([0, 1, 2, 3], [[1, 2, 3, 4]], ['p50']);
    const experiment = spectrum([0, 2, 4, 6], [[1, 2, 3, 4]], ['p50']);
    const r = buildDeltaSpectrum(baseline, experiment);
    assert.equal(r, null);
});

test('buildDeltaSpectrum: empty inputs return null', () => {
    const r = buildDeltaSpectrum({ data: [[]] }, { data: [[]] });
    assert.equal(r, null);
});

test('buildDeltaSpectrum: identical captures yield dMin === dMax === 0', () => {
    const baseline = spectrum([0, 1], [[1, 2], [3, 4]], ['p50', 'p99']);
    const experiment = spectrum([0, 1], [[1, 2], [3, 4]], ['p50', 'p99']);
    const r = buildDeltaSpectrum(baseline, experiment);
    assert.deepEqual(r.data[1], [0, 0]);
    assert.deepEqual(r.data[2], [0, 0]);
    // Flat-zero is intentionally not padded here — the caller pads
    // before handing off to a diverging palette renderer.
    assert.equal(r.dMin, 0);
    assert.equal(r.dMax, 0);
});

// ── composeScatterLabel ──────────────────────────────────────────
// Builds a stable multi-dim label for compare-mode label matching of
// percentile/scatter charts. Quantile dim → "pXX"; other dims appended
// (alpha-sorted by key, value-only) joined by " · ". Replaces the
// asymmetric label extraction that dropped extra dims (broke compare
// for category-split percentile metrics like inference-library TTFT).

test('composeScatterLabel: pure quantile (fraction form)', () => {
    assert.equal(composeScatterLabel({ quantile: '0.5' }), 'p50');
    assert.equal(composeScatterLabel({ quantile: '0.95' }), 'p95');
    assert.equal(composeScatterLabel({ quantile: '0.999' }), 'p99.9');
});

test('composeScatterLabel: pure quantile (percent form)', () => {
    assert.equal(composeScatterLabel({ quantile: '50' }), 'p50');
    assert.equal(composeScatterLabel({ quantile: 'p99' }), 'p99');
});

test('composeScatterLabel: quantile + one extra dim — sub-chart label includes value', () => {
    assert.equal(
        composeScatterLabel({ quantile: '0.5', category: 'vllm' }),
        'p50 · vllm',
    );
});

test('composeScatterLabel: quantile + multiple extra dims — alpha-sorted by key', () => {
    // Insertion order varies; the label should be deterministic.
    const a = composeScatterLabel({ quantile: '0.95', engine: 'sglang', node: 'gpu-1' });
    const b = composeScatterLabel({ node: 'gpu-1', quantile: '0.95', engine: 'sglang' });
    assert.equal(a, 'p95 · sglang · gpu-1');
    assert.equal(a, b);
});

test('composeScatterLabel: __name__ stripped from extra dims', () => {
    assert.equal(
        composeScatterLabel({ __name__: 'ttft_seconds', quantile: '0.5', category: 'vllm' }),
        'p50 · vllm',
    );
});

test('composeScatterLabel: no quantile — falls back to extra dims only', () => {
    assert.equal(composeScatterLabel({ category: 'vllm' }), 'vllm');
    assert.equal(
        composeScatterLabel({ engine: 'sglang', node: 'gpu-1' }),
        'sglang · gpu-1',
    );
});

test('composeScatterLabel: missing/empty input → null', () => {
    assert.equal(composeScatterLabel(null), null);
    assert.equal(composeScatterLabel(undefined), null);
    assert.equal(composeScatterLabel({}), null);
    assert.equal(composeScatterLabel({ __name__: 'x' }), null);
    assert.equal(composeScatterLabel('not-an-object'), null);
});

test('composeScatterLabel: unparseable quantile + extra dims still surfaces extra dims', () => {
    // Quantile that can't be parsed gets dropped; extra dims still
    // contribute. Better than silently returning null on a malformed
    // metric — partial info is more diagnostic than no info.
    assert.equal(
        composeScatterLabel({ quantile: 'oops', category: 'vllm' }),
        'vllm',
    );
});

test('composeScatterLabel: numeric quantile values', () => {
    assert.equal(composeScatterLabel({ quantile: 0.5 }), 'p50');
    assert.equal(composeScatterLabel({ quantile: 95 }), 'p95');
});

test('composeScatterLabel: baseline+experiment with same dims agree', () => {
    // The original bug: baseline produced "vllm" (or "0.5") and
    // experiment produced "p50" — disjoint sets, "no shared labels".
    // After the fix, both sides yield the same key.
    const baseline = composeScatterLabel({ __name__: 'ttft', quantile: '0.5', category: 'vllm' });
    const experiment = composeScatterLabel({ __name__: 'ttft', quantile: '0.5', category: 'vllm' });
    assert.equal(baseline, experiment);
    assert.equal(baseline, 'p50 · vllm');
});

// ── composeScatterLabel: excludeValues for category-mode compare ─

// In category-mode compare (e.g. inference-library, baseline=vllm vs
// experiment=sglang) the per-side queries produce series whose labels
// intentionally differ on a capture-identity dim like `source=vllm`
// vs `source=sglang`. Including that dim in the match key would put
// baseline and experiment in disjoint sets ("no shared labels").
// The Group component plumbs `category_members` down so the composer
// can drop label values that match category-member names.

test('composeScatterLabel: excludeValues drops the matching label entirely', () => {
    const excludeValues = new Set(['vllm', 'sglang']);
    assert.equal(
        composeScatterLabel(
            { __name__: 'ttft', quantile: '0.5', source: 'vllm' },
            { excludeValues },
        ),
        'p50',
    );
    assert.equal(
        composeScatterLabel(
            { __name__: 'ttft', quantile: '0.5', source: 'sglang' },
            { excludeValues },
        ),
        'p50',
    );
});

test('composeScatterLabel: excludeValues — baseline+experiment agree on match key', () => {
    const excludeValues = new Set(['vllm', 'sglang']);
    const baseline = composeScatterLabel(
        { __name__: 'vllm_ttft', quantile: '0.5', source: 'vllm' },
        { excludeValues },
    );
    const experiment = composeScatterLabel(
        { __name__: 'sglang_ttft', quantile: '0.5', source: 'sglang' },
        { excludeValues },
    );
    assert.equal(baseline, experiment);
    assert.equal(baseline, 'p50');
});

test('composeScatterLabel: excludeValues preserves non-matching dims', () => {
    // A real subseries dim (e.g. cgroup) shouldn't be dropped just
    // because excludeValues is set. Only values matching the exclude
    // set are removed.
    const excludeValues = new Set(['vllm', 'sglang']);
    assert.equal(
        composeScatterLabel(
            { quantile: '0.5', source: 'vllm', cgroup: 'workload-a' },
            { excludeValues },
        ),
        'p50 · workload-a',
    );
});

test('composeScatterLabel: excludeValues empty/missing → no filtering', () => {
    assert.equal(
        composeScatterLabel({ quantile: '0.5', source: 'vllm' }),
        'p50 · vllm',
    );
    assert.equal(
        composeScatterLabel({ quantile: '0.5', source: 'vllm' }, {}),
        'p50 · vllm',
    );
    assert.equal(
        composeScatterLabel(
            { quantile: '0.5', source: 'vllm' },
            { excludeValues: new Set() },
        ),
        'p50 · vllm',
    );
});

test('composeScatterLabel: excludeValues drops all dims → falls back to quantile only', () => {
    const excludeValues = new Set(['vllm']);
    // When the only non-quantile dim is excluded, the result is just
    // the canonical quantile.
    assert.equal(
        composeScatterLabel({ quantile: '0.95', source: 'vllm' }, { excludeValues }),
        'p95',
    );
});
