import assert from 'node:assert/strict';
import test from 'node:test';

// metric_browser.js now imports chart_controls.js (for expandLink), which
// pulls in colormap.js -> reads CSS custom properties at module-load time.
// Stub the browser globals before importing — see compare_display_strip.test.mjs.
globalThis.getComputedStyle = () => ({ getPropertyValue: () => '' });
if (typeof globalThis.document === 'undefined') {
    globalThis.document = { documentElement: {}, body: {} };
}

const { withTimestampRow } = await import('../src/viewer/assets/lib/features/metric_browser.js');

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
