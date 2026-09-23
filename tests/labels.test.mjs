import test from 'node:test';
import assert from 'node:assert/strict';
import {
    STORAGE_KEYS,
    isStorageKey,
    isInternalLabel,
    visibleLabels,
    firstVisibleLabelValue,
} from '../src/viewer/assets/lib/labels.js';

// The same list metriken_query::STORAGE_KEYS pins on the Rust side.
test('storage keys match the query engine list', () => {
    assert.deepEqual(STORAGE_KEYS, ['metric', 'metric_type', 'unit', 'grouping_power', 'max_value_power']);
    for (const k of STORAGE_KEYS) assert.equal(isStorageKey(k), true, k);
    assert.equal(isStorageKey('cpu'), false);
});

test('the double-underscore prefix is the whole rule', () => {
    for (const k of ['__name__', '__run__', '__incarnation__', '__x']) assert.equal(isInternalLabel(k), true, k);
    for (const k of ['name', '_name', 'x__', 'cpu', '']) assert.equal(isInternalLabel(k), false, k);
});

// The failure this exists to prevent: the server emits labels sorted, so an
// internal label sorts before every letter, and "the first label that is
// not __name__" would have printed the incarnation id as every legend.
test('a series is named by its first visible label, never an internal one', () => {
    const metric = { __incarnation__: '9f3a', __name__: 'cpu_usage', cpu: '3', mode: 'user' };
    assert.equal(firstVisibleLabelValue(metric), '3');
    assert.deepEqual(visibleLabels(metric), [['cpu', '3'], ['mode', 'user']]);
});

test('a series with only internal labels has no visible name', () => {
    assert.equal(firstVisibleLabelValue({ __name__: 'up' }), null);
    assert.equal(firstVisibleLabelValue(undefined), null);
    assert.deepEqual(visibleLabels(null), []);
});

test('storage keys handed through raw metadata are not shown either', () => {
    const metric = { metric: 'latency', metric_type: 'histogram', unit: 'ns', op: 'read' };
    assert.deepEqual(visibleLabels(metric), [['op', 'read']]);
});
