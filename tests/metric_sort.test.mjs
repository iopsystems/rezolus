import test from 'node:test';
import assert from 'node:assert/strict';
import {
    DEFAULT_SORT, cycleSortKeys, sortMetrics,
} from '../src/viewer/assets/lib/features/metric_sort.js';

const rows = [
    { name: 'zeta', metric_type: 'gauge', series_count: 2, label_keys: ['b'], description: '' },
    { name: 'alpha', metric_type: 'histogram', series_count: 10, label_keys: ['a', 'c'], description: 'x' },
    { name: 'mid', metric_type: 'counter', series_count: 1, label_keys: [], description: '' },
    { name: 'beta', metric_type: 'gauge', series_count: 3, label_keys: ['a'], description: '' },
];
const names = (rs) => rs.map((r) => r.name);

test('series sorts numerically, not lexically (10 after 2)', () => {
    const sorted = sortMetrics(rows, [{ col: 'series', dir: 'asc' }]);
    assert.deepEqual(sorted.map((r) => r.series_count), [1, 2, 3, 10]);
});

test('name ascending / descending', () => {
    assert.deepEqual(names(sortMetrics(rows, [{ col: 'name', dir: 'asc' }])), ['alpha', 'beta', 'mid', 'zeta']);
    assert.deepEqual(names(sortMetrics(rows, [{ col: 'name', dir: 'desc' }])), ['zeta', 'mid', 'beta', 'alpha']);
});

test('multi-key: type asc then name asc groups by type', () => {
    const sorted = sortMetrics(rows, [{ col: 'type', dir: 'asc' }, { col: 'name', dir: 'asc' }]);
    // counter, gauge, gauge, histogram — and within gauge: beta before zeta
    assert.deepEqual(names(sorted), ['mid', 'beta', 'zeta', 'alpha']);
});

test('empty keys falls back to DEFAULT_SORT (type then name)', () => {
    assert.deepEqual(names(sortMetrics(rows, [])), names(sortMetrics(rows, DEFAULT_SORT)));
});

test('sortMetrics does not mutate the input array', () => {
    const before = names(rows);
    sortMetrics(rows, [{ col: 'name', dir: 'desc' }]);
    assert.deepEqual(names(rows), before);
});

test('cycleSortKeys single: new col -> asc -> desc -> default', () => {
    let k = cycleSortKeys(DEFAULT_SORT, 'series', false);
    assert.deepEqual(k, [{ col: 'series', dir: 'asc' }]);
    k = cycleSortKeys(k, 'series', false);
    assert.deepEqual(k, [{ col: 'series', dir: 'desc' }]);
    k = cycleSortKeys(k, 'series', false);
    assert.deepEqual(k, DEFAULT_SORT);
});

test('cycleSortKeys shift: adds secondary key, preserves order', () => {
    let k = [{ col: 'type', dir: 'asc' }];
    k = cycleSortKeys(k, 'name', true);
    assert.deepEqual(k, [{ col: 'type', dir: 'asc' }, { col: 'name', dir: 'asc' }]);
    // shift again toggles that key to desc, keeping priority order
    k = cycleSortKeys(k, 'name', true);
    assert.deepEqual(k, [{ col: 'type', dir: 'asc' }, { col: 'name', dir: 'desc' }]);
    // shift a third time removes it
    k = cycleSortKeys(k, 'name', true);
    assert.deepEqual(k, [{ col: 'type', dir: 'asc' }]);
});

test('cycleSortKeys shift: removing the last key reverts to DEFAULT_SORT', () => {
    let k = [{ col: 'name', dir: 'desc' }];
    k = cycleSortKeys(k, 'name', true); // remove -> empty -> default
    assert.deepEqual(k, DEFAULT_SORT);
});
