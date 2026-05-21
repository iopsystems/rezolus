import test from 'node:test';
import assert from 'node:assert/strict';
import { migrateSelection, SELECTION_SCHEMA_VERSION, defaultSelection } from '../src/viewer/assets/lib/selection/selection_migration.js';

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

// Round-trip test: a v3 payload shaped like buildPayload's output
// (the JSON-export shape) survives migrateSelection without losing
// the optional compare field. Codifies the contract that any caller
// in the load path (loadPayloadIntoStore, restoreStore, etc.) will
// see compare intact when present. Catches regressions where a new
// field strips or reshapes compare during validation.
test('buildPayload-shaped v3 payload round-trips through migrateSelection with compare intact', () => {
    const v3 = {
        version: 3,
        report_id: '0192f6a8-7c2e-7000-8000-000000000001',
        rezolus_version: '5.13.1-alpha.1',
        saved_at: '2026-05-10T20:00:00.000Z',
        source: 'rezolus',
        filename: 'capture.parquet',
        file_checksum: null,
        time_range: { start_ms: 0, end_ms: 1000 },
        zoom: null,
        step_override: null,
        anchors: { baseline: 100, experiment: 200 },
        chartToggles: { 'cpu/usage': { diff: true } },
        compare: { baseline_alias: 'vllm', experiment_alias: 'sglang' },
        tagline: 'compare run #42',
        entries: [
            { chartId: 'cpu/usage', section: 'cpu', sectionName: 'CPU', groupName: 'usage', sql_query: 'SELECT * FROM cpu_usage', note: '', chartOpts: { id: 'cpu/usage', title: 'CPU' } },
        ],
    };
    const s = migrateSelection(v3);
    assert.deepEqual(s.compare, { baseline_alias: 'vllm', experiment_alias: 'sglang' });
    assert.deepEqual(s.anchors, { baseline: 100, experiment: 200 });
    assert.deepEqual(s.chartToggles, { 'cpu/usage': { diff: true } });
    assert.equal(s.tagline, 'compare run #42');
    assert.equal(s.entries.length, 1);
});
