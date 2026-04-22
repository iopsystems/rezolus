import test from 'node:test';
import assert from 'node:assert/strict';
import { migrateSelection, SELECTION_SCHEMA_VERSION, defaultSelection } from '../src/viewer/assets/lib/selection_migration.js';

test('schema version is 2', () => {
    assert.equal(SELECTION_SCHEMA_VERSION, 2);
});

test('old (v1) selection gains default anchors and toggles', () => {
    const old = { version: 1, timeRange: [0, 100] };
    const m = migrateSelection(old);
    assert.equal(m.version, 2);
    assert.deepEqual(m.anchors, { baseline: 0, experiment: 0 });
    assert.deepEqual(m.chartToggles, {});
    // Other fields pass through unchanged.
    assert.deepEqual(m.timeRange, [0, 100]);
});

test('unversioned legacy selection upgrades to v2', () => {
    const legacy = { timeRange: [0, 100], entries: [] };
    const m = migrateSelection(legacy);
    assert.equal(m.version, 2);
    assert.deepEqual(m.anchors, { baseline: 0, experiment: 0 });
    assert.deepEqual(m.chartToggles, {});
});

test('v2 selection unchanged by migration', () => {
    const cur = {
        version: 2,
        anchors: { baseline: 5, experiment: 10 },
        chartToggles: { cpu: { diff: true } },
    };
    const m = migrateSelection(cur);
    assert.deepEqual(m, cur);
});

test('null/undefined input yields a default selection', () => {
    const a = migrateSelection(null);
    const b = migrateSelection(undefined);
    assert.equal(a.version, 2);
    assert.equal(b.version, 2);
    assert.deepEqual(a.anchors, { baseline: 0, experiment: 0 });
    assert.deepEqual(a.chartToggles, {});
    assert.deepEqual(a, defaultSelection());
});

test('malformed anchors are coerced to numeric baseline/experiment', () => {
    const m = migrateSelection({ version: 1, anchors: { baseline: '42', experiment: 'not-a-number' } });
    assert.equal(m.anchors.baseline, 42);
    assert.equal(m.anchors.experiment, 0);
});

test('malformed chartToggles is replaced with empty object', () => {
    const m = migrateSelection({ version: 1, chartToggles: 'not an object' });
    assert.deepEqual(m.chartToggles, {});
});
