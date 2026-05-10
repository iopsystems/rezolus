import test from 'node:test';
import assert from 'node:assert/strict';
import { migrateSelection, SELECTION_SCHEMA_VERSION, defaultSelection } from '../src/viewer/assets/lib/selection_migration.js';

test('schema version is 3', () => {
    assert.equal(SELECTION_SCHEMA_VERSION, 3);
});

test('null/undefined input yields a default v3 selection', () => {
    const s = migrateSelection(null);
    assert.equal(s.version, 3);
    assert.deepEqual(s.entries, []);
    assert.deepEqual(s.anchors, { baseline: 0, experiment: 0 });
    assert.deepEqual(s.chartToggles, {});
    assert.equal(s.compare, undefined);
});

test('v3 selection passes through unchanged', () => {
    const v3 = {
        version: 3,
        tagline: 'hi',
        entries: [{ chartId: 'x' }],
        anchors: { baseline: 1, experiment: 2 },
        chartToggles: { x: { diff: true } },
    };
    const s = migrateSelection(v3);
    assert.equal(s.version, 3);
    assert.equal(s.tagline, 'hi');
    assert.deepEqual(s.entries, [{ chartId: 'x' }]);
});

test('v3 selection preserves optional compare field', () => {
    const v3 = {
        version: 3,
        entries: [],
        compare: { baseline_alias: 'vllm', experiment_alias: 'sglang' },
    };
    const s = migrateSelection(v3);
    assert.deepEqual(s.compare, { baseline_alias: 'vllm', experiment_alias: 'sglang' });
});

test('v2 input throws explicit unsupported error', () => {
    assert.throws(
        () => migrateSelection({ version: 2, entries: [] }),
        /unsupported.*version.*2/i,
    );
});

test('v1 (versionless) input throws explicit unsupported error', () => {
    assert.throws(
        () => migrateSelection({ entries: [] }),
        /unsupported.*version/i,
    );
});

test('malformed anchors are coerced to numeric on v3', () => {
    const s = migrateSelection({
        version: 3,
        entries: [],
        anchors: { baseline: 'oops', experiment: '5' },
    });
    assert.deepEqual(s.anchors, { baseline: 0, experiment: 5 });
});

test('missing chartToggles becomes empty object on v3', () => {
    const s = migrateSelection({ version: 3, entries: [] });
    assert.deepEqual(s.chartToggles, {});
});
