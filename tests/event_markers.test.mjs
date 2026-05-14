import test from 'node:test';
import assert from 'node:assert/strict';
import { buildMarkLine } from '../src/viewer/assets/lib/charts/event_markers.js';

test('returns null when no events', () => {
    assert.equal(buildMarkLine([]), null);
    assert.equal(buildMarkLine(null), null);
});

test('builds one markLine entry per event with xAxis in ms', () => {
    const events = [
        { timestamp: 1715625600000000000, description: 'deploy' },
        { timestamp: 1715625900000000000, description: 'restart' },
    ];
    const ml = buildMarkLine(events);
    assert.ok(ml);
    assert.equal(ml.symbol, 'none');
    assert.equal(ml.silent, false);
    assert.equal(ml.data.length, 2);
    // ns -> ms conversion
    assert.equal(ml.data[0].xAxis, 1715625600000);
    assert.equal(ml.data[1].xAxis, 1715625900000);
});

test('description surfaces as marker label tooltip', () => {
    const ml = buildMarkLine([
        { timestamp: 1_000_000_000, description: 'deploy v2.1.4' },
    ]);
    assert.equal(ml.data[0].name, 'deploy v2.1.4');
});

test('skips events with missing timestamp', () => {
    const ml = buildMarkLine([
        { description: 'no ts' },
        { timestamp: 1_000_000_000, description: 'ok' },
    ]);
    assert.equal(ml.data.length, 1);
    assert.equal(ml.data[0].name, 'ok');
});
