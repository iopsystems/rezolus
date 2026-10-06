// The bucket widths a chart is drawn at and how a per-entity heatmap is
// fetched: the round-width ladder (mirroring metriken-query's
// `nice_bucket_secs`), the heatmap step for a range, which queries are
// rates, and the display response as a matrix.
import { test } from 'node:test';
import assert from 'node:assert';
import {
    niceSecs, heatmapStep, isRateQuery, isEntityQuery, displayAsMatrix, restampRateColumns,
    displayBucketWidth,
} from '../src/viewer/assets/lib/data.js';

test('niceSecs is the smallest round width at least the raw width', () => {
    for (const [raw, nice] of [
        [0.4, 1], [1, 1], [1.2, 2], [3, 5], [59, 60], [61, 120], [347, 600],
        [601, 900], [3599, 3600], [50_000, 86_400], [100_000, 172_800],
    ]) {
        assert.strictEqual(niceSecs(raw), nice, `raw ${raw}`);
    }
});

test('heatmapStep matches the display buckets for the range', () => {
    const meta = { interval: 1 };
    // A whole 9.6-hour recording at 100 buckets: 10-minute columns.
    assert.strictEqual(heatmapStep(meta, 0, 34_677, 100), 600);
    // An hour: one-minute columns.
    assert.strictEqual(heatmapStep(meta, 0, 3_600, 100), 60);
    // A window holding no more samples than the budget: the native interval.
    assert.strictEqual(heatmapStep(meta, 0, 100, 100), 1);
    assert.strictEqual(heatmapStep(meta, 0, 90, 100), 1);
});

test('heatmapStep is a whole multiple of the recording interval', () => {
    // 3 s recording, an hour at 100 buckets: 36 s rounds to 60 s.
    assert.strictEqual(heatmapStep({ interval: 3 }, 0, 3_600, 100), 60);
    // 7 s recording: 60 s is not a multiple of 7, so 63 s.
    assert.strictEqual(heatmapStep({ interval: 7 }, 0, 3_600, 100), 63);
    // 100 ms recording, a minute: 0.6 s rounds to the 1 s floor.
    assert.strictEqual(heatmapStep({ interval: 0.1 }, 0, 60, 100), 1);
});

test('isRateQuery finds rate and irate, not gauges', () => {
    assert.ok(isRateQuery('sum by (id) (irate(cpu_usage[5m])) / 1000000000'));
    assert.ok(isRateQuery('sum by (id, vendor) (rate(gpu_engine_busy_time[5m]))'));
    assert.ok(!isRateQuery('sum by (id, vendor) (gpu_utilization) / 100'));
    assert.ok(!isRateQuery('max by (id, vendor) (gpu_temperature)'));
    assert.ok(!isRateQuery('sum by (id) (separate(x))'));
});

test('displayAsMatrix gives each series its bucket medians at bucket starts', () => {
    const series = [
        { metric: { id: '0' }, t: new Float64Array([0, 600]), median: new Float64Array([1.5, 2.5]) },
        // A bucket holding one point is stamped at the point's own time.
        { metric: { id: '1' }, t: new Float64Array([0, 1_203]), median: new Float64Array([7, 8]) },
    ];
    assert.deepStrictEqual(displayAsMatrix(series, 600), {
        status: 'success',
        data: {
            resultType: 'matrix',
            result: [
                { metric: { id: '0' }, values: [[0, '1.5'], [600, '2.5']] },
                { metric: { id: '1' }, values: [[0, '7'], [1_200, '8']] },
            ],
        },
    });
});

test('restampRateColumns moves each value to the start of its step', () => {
    const res = { data: { result: [{ metric: {}, values: [[600, '1'], [1_200, '2']] }] } };
    assert.deepStrictEqual(restampRateColumns(res, 600).data.result[0].values, [[0, '1'], [600, '2']]);
});

test('isEntityQuery is a by (id) query with no outer aggregation', () => {
    assert.ok(isEntityQuery('sum by (id) (irate(cpu_usage[5m])) / 1000000000'));
    assert.ok(isEntityQuery('max by (id, vendor) (gpu_temperature)'));
    assert.ok(isEntityQuery('1 - sum by (id) (irate(cpu_l3_miss[5m])) / sum by (id) (irate(cpu_l3_access[5m]))'));
    assert.ok(!isEntityQuery('avg(sum by (id) (irate(core_cstate_residency[5m])) / sum by (id) (irate(cpu_tsc[5m])))'));
    assert.ok(!isEntityQuery('sum(irate(cpu_usage[5m]))'));
});

test('displayBucketWidth follows the server width rule', () => {
    // Step 60 over 9.6 h at 100 buckets: 600 s.
    assert.strictEqual(displayBucketWidth(0, 34_677, 60, 100), 600);
    // A range that fits at its step keeps the step.
    assert.strictEqual(displayBucketWidth(0, 90, 1, 100), 1);
    assert.strictEqual(displayBucketWidth(0, 4, 0.1, 48), 0.1);
});
