import test from 'node:test';
import assert from 'node:assert/strict';
import {
    heatmapTriplesMinMax,
    heatmapTriplesToMatrix,
    ensureHeatmapMatrix,
} from '../src/viewer/assets/lib/charts/util/heatmap_data.js';

test('heatmapTriplesMinMax ignores nullish and NaN cells', () => {
    assert.deepEqual(
        heatmapTriplesMinMax([
            [0, 0, null],
            [1, 0, Number.NaN],
            [2, 0, 3],
            [3, 0, -1],
        ]),
        { min: -1, max: 3 },
    );
});

test('heatmapTriplesToMatrix fills gaps with null and respects bin count', () => {
    assert.deepEqual(
        heatmapTriplesToMatrix(
            [
                [0, 0, 5],
                [2, 1, 9],
            ],
            4,
        ),
        [
            [5, null, null, null],
            [null, null, 9, null],
        ],
    );
});

test('ensureHeatmapMatrix computes lazily and memoizes on the capture object', () => {
    const capture = {
        timeData: [10, 20, 30],
        heatmapData: [
            [0, 0, 1],
            [2, 1, 4],
        ],
    };

    assert.equal('heatmapMatrix' in capture, false);

    const first = ensureHeatmapMatrix(capture);
    assert.deepEqual(first, [
        [1, null, null],
        [null, null, 4],
    ]);
    assert.equal(capture.heatmapMatrix, first);

    const second = ensureHeatmapMatrix(capture);
    assert.equal(second, first);
});
