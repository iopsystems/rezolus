// The per-view memo of the capture list and per-capture metadata, and the
// bounded pool the N-way overlay fetches through (data.js).
import test from 'node:test';
import assert from 'node:assert/strict';
import {
    listCaptures, captureMetadata, clearCaptureMemo, clearMetadataCache, mapLimit,
} from '../src/viewer/assets/lib/data.js';
import { ViewerApi } from '../src/viewer/assets/lib/viewer_api.js';

// Swap the backend for a counting stub; each test restores it.
const stub = (impl) => {
    const saved = { getCaptures: ViewerApi.getCaptures, getMetadata: ViewerApi.getMetadata };
    Object.assign(ViewerApi, impl);
    return () => Object.assign(ViewerApi, saved);
};

test.beforeEach(() => clearCaptureMemo());

test('concurrent callers share one in-flight capture list', async () => {
    let calls = 0;
    const restore = stub({ getCaptures: async () => { calls++; return [{ id: 'baseline' }]; } });
    try {
        const [a, b] = await Promise.all([listCaptures(), listCaptures()]);
        assert.equal(calls, 1);
        assert.equal(a, b);
        await listCaptures();
        assert.equal(calls, 1);
        clearCaptureMemo();
        await listCaptures();
        assert.equal(calls, 2);
    } finally { restore(); }
});

test('metadata is memoized per capture id and shared by concurrent callers', async () => {
    const calls = [];
    const restore = stub({ getMetadata: async (id) => { calls.push(id); return { data: { minTime: 1, id } }; } });
    try {
        const [a, b, c] = await Promise.all([
            captureMetadata('run3'), captureMetadata('run3'), captureMetadata('baseline'),
        ]);
        assert.deepEqual(calls, ['run3', 'baseline']);
        assert.equal(a, b);
        assert.equal(c.data.id, 'baseline');
        // The default id is the baseline.
        assert.equal(await captureMetadata(), c);
        assert.deepEqual(calls, ['run3', 'baseline']);
    } finally { restore(); }
});

test('a rejection is evicted so the next caller retries', async () => {
    let calls = 0;
    const restore = stub({
        getCaptures: async () => { calls++; if (calls === 1) throw new Error('down'); return []; },
        getMetadata: async () => { calls++; if (calls === 3) throw new Error('down'); return {}; },
    });
    try {
        await assert.rejects(listCaptures(), /down/);
        assert.deepEqual(await listCaptures(), []);
        assert.equal(calls, 2);
        await assert.rejects(captureMetadata('x'), /down/);
        assert.deepEqual(await captureMetadata('x'), {});
        assert.equal(calls, 4);
        // A rejection must not evict a memo that was cleared and refilled
        // while it was in flight.
        let release;
        const gate = new Promise((r) => { release = r; });
        clearCaptureMemo();
        ViewerApi.getCaptures = async () => { await gate; throw new Error('late'); };
        const stale = listCaptures();
        clearCaptureMemo();
        ViewerApi.getCaptures = async () => ['fresh'];
        const fresh = listCaptures();
        release();
        await assert.rejects(stale, /late/);
        assert.equal(await listCaptures(), await fresh);
    } finally { restore(); }
});

test('a synchronous throw from the backend rejects rather than escaping', async () => {
    const restore = stub({ getMetadata: () => { throw new Error('not attached'); } });
    try {
        await assert.rejects(captureMetadata('experiment'), /not attached/);
    } finally { restore(); }
});

test('clearMetadataCache (the file-swap clear) drops the memo too', async () => {
    let calls = 0;
    const restore = stub({ getCaptures: async () => { calls++; return []; } });
    try {
        await listCaptures();
        clearMetadataCache();
        await listCaptures();
        assert.equal(calls, 2);
    } finally { restore(); }
});

test('mapLimit keeps input order and never exceeds the limit in flight', async () => {
    let inFlight = 0;
    let peak = 0;
    const out = await mapLimit([5, 1, 4, 2, 3, 0], 2, async (ms, i) => {
        inFlight++;
        peak = Math.max(peak, inFlight);
        await new Promise((r) => setTimeout(r, ms));
        inFlight--;
        return `${i}:${ms}`;
    });
    assert.deepEqual(out, ['0:5', '1:1', '2:4', '3:2', '4:3', '5:0']);
    assert.equal(peak, 2);
    assert.deepEqual(await mapLimit([], 4, async () => 1), []);
    // A limit above the length, or a nonsense one, still runs everything.
    assert.deepEqual(await mapLimit([1, 2], 10, async (x) => x * 2), [2, 4]);
    assert.deepEqual(await mapLimit([1, 2], 0, async (x) => x * 2), [2, 4]);
    // An escaping rejection rejects the map; callers catch inside fn.
    await assert.rejects(mapLimit([1], 1, async () => { throw new Error('boom'); }), /boom/);
});
