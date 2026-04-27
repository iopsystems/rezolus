import test from 'node:test';
import assert from 'node:assert/strict';
import {
    createHeatmapResolutionStore,
    ensureHeatmapResolution,
} from '../src/viewer/assets/lib/charts/util/heatmap_data.js';

test('ensureHeatmapResolution materializes full-resolution sparse heatmap cells', () => {
    const store = createHeatmapResolutionStore(
        [
            [0, 0, 5],
            [2, 1, 9],
        ],
        [10, 20, 30],
        false,
    );

    const resolution = ensureHeatmapResolution(store, 1);
    assert.equal(resolution.factor, 1);
    assert.deepEqual(resolution.data, [
        [10000, 0, 0, null, 5],
        [30000, 1, 2, null, 9],
    ]);
});

test('ensureHeatmapResolution downsampled output preserves min/max per bin and replaces old resolution', () => {
    const store = createHeatmapResolutionStore(
        [
            [0, 0, 5],
            [1, 0, 3],
            [2, 0, 9],
            [3, 0, 7],
        ],
        [10, 20, 30, 40],
        false,
    );

    const first = ensureHeatmapResolution(store, 1);
    const second = ensureHeatmapResolution(store, 2);

    assert.notEqual(second, first);
    assert.equal(store.current, second);
    assert.deepEqual(second.data, [
        [10000, 0, 0, 3, 5],
        [30000, 0, 2, 7, 9],
    ]);
});

test('emitNullCells fills missing cells in the current resolution only', () => {
    const store = createHeatmapResolutionStore(
        [
            [1, 0, 4],
        ],
        [10, 20, 30],
        true,
    );

    const resolution = ensureHeatmapResolution(store, 1);
    assert.deepEqual(resolution.data, [
        [10000, 0, 0, null, null],
        [20000, 0, 1, null, 4],
        [30000, 0, 2, null, null],
    ]);
});
