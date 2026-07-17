import assert from 'node:assert/strict';
import test from 'node:test';
import { withTimestampRow } from '../src/viewer/assets/lib/features/metric_browser.js';

test('withTimestampRow prepends a synthetic timestamp metric', () => {
    const rows = withTimestampRow([{ name: 'queue_depth', metric_type: 'gauge' }]);
    assert.equal(rows[0].name, 'timestamp');
    assert.equal(rows[0].metric_type, 'timestamp');
    assert.equal(rows.length, 2);
});

test('withTimestampRow does not mutate the input array', () => {
    const input = [{ name: 'queue_depth', metric_type: 'gauge' }];
    const rows = withTimestampRow(input);
    assert.equal(input.length, 1);
    assert.notEqual(rows, input);
});
