import test from 'node:test';
import assert from 'node:assert/strict';
import { EventsStore } from '../src/viewer/assets/lib/events_store.js';

test('seedFromMetadata: empty / null / non-array all yield empty', () => {
    const s = new EventsStore();
    s.seedFromMetadata(null);
    assert.deepEqual(s.all(), []);
    s.seedFromMetadata({});
    assert.deepEqual(s.all(), []);
    s.seedFromMetadata({ events: 'not an array' });
    assert.deepEqual(s.all(), []);
});

test('seedFromMetadata: parses events array', () => {
    const s = new EventsStore();
    s.seedFromMetadata({
        events: [
            { timestamp: 1, description: 'a' },
            { timestamp: 2, description: 'b', chart_id: 'c1' },
        ],
    });
    assert.equal(s.all().length, 2);
    assert.equal(s.all()[1].chart_id, 'c1');
});

test('seedFromMetadata: parses wrapped {events:[...]} shape (actual parquet wire format)', () => {
    const s = new EventsStore();
    s.seedFromMetadata({
        events: {
            events: [
                { timestamp: 1, description: 'a' },
                { timestamp: 2, description: 'b', chart_id: 'c1' },
            ],
        },
    });
    assert.equal(s.all().length, 2);
    assert.equal(s.all()[1].chart_id, 'c1');
});

test('seedFromMetadata: replaces prior contents (idempotent re-seed)', () => {
    const s = new EventsStore();
    s.add({ timestamp: 99, description: 'pre-existing' });
    s.seedFromMetadata({ events: [{ timestamp: 1, description: 'a' }] });
    assert.equal(s.all().length, 1);
    assert.equal(s.all()[0].description, 'a');
});

test('add: appends and notifies subscribers', () => {
    const s = new EventsStore();
    let calls = 0;
    s.subscribe(() => { calls += 1; });
    s.add({ timestamp: 1, description: 'a' });
    s.add({ timestamp: 2, description: 'b' });
    assert.equal(s.all().length, 2);
    assert.equal(calls, 2);
});

test('subscribe: returns unsubscribe', () => {
    const s = new EventsStore();
    let calls = 0;
    const off = s.subscribe(() => { calls += 1; });
    s.add({ timestamp: 1, description: 'a' });
    off();
    s.add({ timestamp: 2, description: 'b' });
    assert.equal(calls, 1);
});

test('filterForChart: chart_id mismatch excludes', () => {
    const s = new EventsStore();
    s.add({ timestamp: 1, description: 'a', chart_id: 'c1' });
    s.add({ timestamp: 2, description: 'b', chart_id: 'c2' });
    const out = s.filterForChart({ chartId: 'c1', scope: {} });
    assert.equal(out.length, 1);
    assert.equal(out[0].description, 'a');
});

test('filterForChart: chart_id absent on event = global', () => {
    const s = new EventsStore();
    s.add({ timestamp: 1, description: 'global' });
    const out = s.filterForChart({ chartId: 'anything', scope: {} });
    assert.equal(out.length, 1);
});

test('filterForChart: source/node/instance scope respected', () => {
    const s = new EventsStore();
    s.add({ timestamp: 1, description: 'svc-a', source: 'a' });
    s.add({ timestamp: 2, description: 'svc-b', source: 'b' });
    s.add({ timestamp: 3, description: 'global' });
    const out = s.filterForChart({ chartId: 'c', scope: { source: 'a' } });
    const descs = out.map((e) => e.description).sort();
    assert.deepEqual(descs, ['global', 'svc-a']);
});

test('filterForChart: event scope omits field = matches everything', () => {
    const s = new EventsStore();
    s.add({ timestamp: 1, description: 'no-node', source: 'svc' });
    const out = s.filterForChart({
        chartId: 'c',
        scope: { source: 'svc', node: 'gpu01', instance: '0' },
    });
    assert.equal(out.length, 1);
});

test('clear: empties store and notifies', () => {
    const s = new EventsStore();
    s.add({ timestamp: 1, description: 'a' });
    let calls = 0;
    s.subscribe(() => { calls += 1; });
    s.clear();
    assert.equal(s.all().length, 0);
    assert.equal(calls, 1);
});
