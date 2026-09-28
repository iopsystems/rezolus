import test from 'node:test';
import assert from 'node:assert/strict';
import {
    buildMarkLine,
    buildRangeSpans,
    formatDuration,
    isRangeEvent,
} from '../src/viewer/assets/lib/charts/event_markers.js';

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
    // Hairline is non-interactive; the HTML bubble owns clicks.
    assert.equal(ml.silent, true);
    assert.equal(ml.label.show, false);
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

test('buildRangeSpans returns null when no event is a range', () => {
    assert.equal(buildRangeSpans([]), null);
    assert.equal(buildRangeSpans(null), null);
    assert.equal(buildRangeSpans([{ timestamp: 1_000_000_000, description: 'point' }]), null);
    // A zero duration is a point, not a band.
    assert.equal(buildRangeSpans([{ timestamp: 1_000_000_000, description: 'p', duration_ns: 0 }]), null);
});

test('buildRangeSpans emits one {startMs, endMs} per range event', () => {
    const events = [
        { timestamp: 1715625600000000000, description: 'warmup', duration_ns: 90_000_000_000 },
        { timestamp: 1715625900000000000, description: 'point' },
        { timestamp: 1715626200000000000, description: 'incident', duration_ns: 5_000_000_000 },
    ];
    const spans = buildRangeSpans(events);
    assert.ok(spans);
    assert.equal(spans.length, 2);
    assert.deepEqual(spans[0], { startMs: 1715625600000, endMs: 1715625690000, name: 'warmup' });
    assert.deepEqual(spans[1], { startMs: 1715626200000, endMs: 1715626205000, name: 'incident' });
});

test('a range event still gets its start hairline from buildMarkLine', () => {
    const ml = buildMarkLine([
        { timestamp: 2_000_000_000, description: 'range', duration_ns: 1_000_000_000 },
    ]);
    assert.equal(ml.data.length, 1);
    assert.equal(ml.data[0].xAxis, 2000);
});

test('markers accept a caller-supplied axis conversion (compare mode)', () => {
    // A compare chart's axis is relative ms: subtract the capture anchor.
    const anchorSec = 1715625600;
    const toAxisMs = (ns) => ns / 1_000_000 - anchorSec * 1000;
    const events = [
        { timestamp: 1715625610000000000, description: 'r', duration_ns: 2_000_000_000 },
    ];
    assert.equal(buildMarkLine(events, toAxisMs).data[0].xAxis, 10_000);
    const spans = buildRangeSpans(events, toAxisMs);
    assert.equal(spans[0].startMs, 10_000);
    assert.equal(spans[0].endMs, 12_000);
});

test('isRangeEvent and formatDuration', () => {
    assert.equal(isRangeEvent({ duration_ns: 1 }), true);
    assert.equal(isRangeEvent({ duration_ns: 0 }), false);
    assert.equal(isRangeEvent({}), false);
    assert.equal(isRangeEvent(null), false);
    // A hand-edited localStorage entry can carry anything; only a finite
    // positive number is a range, everything else renders as a point.
    assert.equal(isRangeEvent({ duration_ns: -5 }), false);
    assert.equal(isRangeEvent({ duration_ns: NaN }), false);
    assert.equal(isRangeEvent({ duration_ns: '5000000000' }), false);
    assert.equal(buildRangeSpans([{ timestamp: 1_000_000_000, duration_ns: '5000000000' }]), null);
    assert.equal(formatDuration(250_000_000), '250ms');
    // Round before choosing the unit, or 999.6 ms prints as "1000ms".
    assert.equal(formatDuration(999_600_000), '1s');
    assert.equal(formatDuration(40_000_000_000), '40s');
    assert.equal(formatDuration(150_000_000_000), '2m30s');
    assert.equal(formatDuration(120_000_000_000), '2m');
    assert.equal(formatDuration(3_900_000_000_000), '1h5m');
    assert.equal(formatDuration(3_600_000_000_000), '1h');
    assert.equal(formatDuration(0), '');
});
