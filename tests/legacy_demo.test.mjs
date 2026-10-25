// Links to the static site's demos named them `.parquet` before 6.0; the
// site loads the `.dendro` twin instead.
import test from 'node:test';
import assert from 'node:assert/strict';
import { dendroDemoName } from '../site/viewer/lib/legacy_demo.js';

test('a bare .parquet demo name becomes its .dendro twin', () => {
    assert.equal(dendroDemoName('demo.parquet'), 'demo.dendro');
    assert.equal(dendroDemoName('AB_base_pin.parquet'), 'AB_base_pin.dendro');
    assert.equal(dendroDemoName('disagg/disagg-sglang.parquet'), 'disagg/disagg-sglang.dendro');
});

test('a .dendro name, a URL and a missing name are unchanged', () => {
    assert.equal(dendroDemoName('demo.dendro'), 'demo.dendro');
    assert.equal(
        dendroDemoName('https://example.com/run.parquet'),
        'https://example.com/run.parquet',
    );
    assert.equal(dendroDemoName(null), null);
});
