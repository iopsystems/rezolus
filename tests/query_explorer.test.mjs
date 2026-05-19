import test from 'node:test';
import assert from 'node:assert/strict';
import { trimAndDedupe, readHistory, pushHistory, HISTORY_KEY, LEGACY_HISTORY_KEY } from '../src/viewer/assets/lib/features/sql_history.js';

test('trimAndDedupe keeps most-recent for duplicates and caps at 20', () => {
    const entries = [
        { sql: 'A', at: 3 },
        { sql: 'B', at: 2 },
        { sql: 'A', at: 1 },  // dropped — already saw "A"
    ];
    assert.deepEqual(trimAndDedupe(entries), [
        { sql: 'A', at: 3 },
        { sql: 'B', at: 2 },
    ]);
});

test('trimAndDedupe drops empty / whitespace-only entries', () => {
    const entries = [
        { sql: '', at: 1 },
        { sql: '   \n\t', at: 2 },
        { sql: 'SELECT 1', at: 3 },
    ];
    assert.deepEqual(trimAndDedupe(entries), [{ sql: 'SELECT 1', at: 3 }]);
});

test('trimAndDedupe caps total length at 20', () => {
    const entries = Array.from({ length: 30 }, (_, i) => ({ sql: `q${i}`, at: i }));
    const out = trimAndDedupe(entries);
    assert.equal(out.length, 20);
    // First 20 retained (insertion order).
    assert.equal(out[0].sql, 'q0');
    assert.equal(out[19].sql, 'q19');
});

// Minimal storage stub so the read/write helpers can run under node.
const makeStorage = () => {
    const store = new Map();
    return {
        getItem: (k) => (store.has(k) ? store.get(k) : null),
        setItem: (k, v) => { store.set(k, String(v)); },
        removeItem: (k) => { store.delete(k); },
        _dump: () => Object.fromEntries(store),
    };
};

test('readHistory migrates pre-purge promql_history on first read', () => {
    const storage = makeStorage();
    storage.setItem(LEGACY_HISTORY_KEY, JSON.stringify([
        { sql: 'rate(cpu[5m])', at: 1 },
    ]));
    const got = readHistory(storage);
    assert.deepEqual(got, [{ sql: 'rate(cpu[5m])', at: 1 }]);
    // Migration persists under the canonical key so subsequent reads
    // skip the legacy fallback.
    assert.equal(JSON.parse(storage.getItem(HISTORY_KEY)).length, 1);
});

test('pushHistory de-dupes against existing entries and bubbles to top', () => {
    const storage = makeStorage();
    pushHistory('SELECT 1', storage);
    pushHistory('SELECT 2', storage);
    pushHistory('SELECT 1', storage);  // bubbles to head
    const got = readHistory(storage);
    assert.equal(got.length, 2);
    assert.equal(got[0].sql, 'SELECT 1');
    assert.equal(got[1].sql, 'SELECT 2');
});

test('pushHistory ignores empty / whitespace-only SQL', () => {
    const storage = makeStorage();
    pushHistory('   \n\t', storage);
    pushHistory('', storage);
    assert.deepEqual(readHistory(storage), []);
});
