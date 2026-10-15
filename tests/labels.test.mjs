import test from 'node:test';
import assert from 'node:assert/strict';
import {
    STORAGE_KEYS,
    isStorageKey,
    isInternalLabel,
    visibleLabels,
    firstVisibleLabelValue,
    varyingLabelKeys,
    seriesNamer,
    namerForKeys,
    pairedSeriesNamer,
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

// A recording's labels (`host`, `source`) are on every series of an archive
// and never name one; the labels that differ between the series do.
const rec = { host: 'web-01', source: 'rezolus' };

test('series are named by the one label that differs between them', () => {
    const metrics = [
        { __name__: 'syscall', ...rec, op: 'read' },
        { ...rec, op: 'write', __name__: 'syscall' },
    ];
    const nameOf = seriesNamer(metrics);
    assert.deepStrictEqual(metrics.map(nameOf), ['read', 'write']);
    assert.deepStrictEqual(varyingLabelKeys(metrics), ['op']);
});

test('series differing in several labels are named by each, sorted', () => {
    const metrics = [
        { ...rec, state: 'user', id: '0' },
        { ...rec, id: '1', state: 'system' },
    ];
    assert.deepStrictEqual(metrics.map(seriesNamer(metrics)), ['id=0, state=user', 'id=1, state=system']);
});

test('a label one series lacks still tells the series apart', () => {
    const metrics = [{ ...rec, op: 'read' }, { ...rec }];
    const nameOf = seriesNamer(metrics);
    assert.strictEqual(nameOf(metrics[0]), 'read');
    assert.strictEqual(nameOf(metrics[1]), null);
});

test('a single series is named by its first visible label', () => {
    const metrics = [{ __name__: 'x', quantile: '0.5' }];
    assert.strictEqual(seriesNamer(metrics)(metrics[0]), '0.5');
});

// Compare mode names the experiment's series by the labels that differ
// between the baseline's, so a one-series experiment still pairs.
test('one series is named by keys taken from another result', () => {
    const baseline = [{ ...rec, op: 'read' }, { ...rec, op: 'write' }];
    const experiment = { host: 'web-02', source: 'rezolus', op: 'read' };
    assert.strictEqual(namerForKeys(varyingLabelKeys(baseline))(experiment), 'read');
});

test('an experiment is named by the baseline\'s keys only when each name is its own', () => {
    const baseline = [{ ...rec, code: '200' }, { ...rec, code: '500' }];
    const one = [{ ...rec, code: '200' }];
    assert.deepStrictEqual(one.map(pairedSeriesNamer(baseline, one)), ['200']);
    // Two experiment series per code: the baseline's keys would collide.
    const two = ['i1', 'i2'].flatMap((instance) => [
        { ...rec, code: '200', instance }, { ...rec, code: '500', instance },
    ]);
    assert.strictEqual(new Set(two.map(pairedSeriesNamer(baseline, two))).size, 4);
    // The experiment lacks the baseline's key: named by its own labels.
    const ops = [{ ...rec, op: 'read' }, { ...rec, op: 'write' }];
    assert.deepStrictEqual(ops.map(pairedSeriesNamer(baseline, ops)), ['read', 'write']);
    // One such series is named as a single series is, so it is still drawn.
    const oneOp = [{ ...rec, op: 'read' }];
    assert.notStrictEqual(pairedSeriesNamer(baseline, oneOp)(oneOp[0]), null);
});
