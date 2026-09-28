import test from 'node:test';
import assert from 'node:assert/strict';
import {
    CaptureContext,
    resolveAnchor,
    isKindAnchor,
    eventsFromFileMetadata,
} from '../src/viewer/assets/lib/events/capture_events.js';

const S = 1_700_000_000; // recording start, seconds
const ev = (kind, sec) => ({ kind, timestamp: sec * 1e9, description: kind });

const ctxWith = (entries) => {
    const ctx = new CaptureContext();
    for (const [id, e] of Object.entries(entries)) ctx.set(id, e);
    return ctx;
};

test('eventsFromFileMetadata accepts both wire shapes', () => {
    assert.deepEqual(eventsFromFileMetadata({ events: [ev('a', 1)] }).length, 1);
    assert.deepEqual(eventsFromFileMetadata({ events: { events: [ev('a', 1)] } }).length, 1);
    assert.deepEqual(eventsFromFileMetadata({}), []);
    assert.deepEqual(eventsFromFileMetadata(null), []);
});

test('a kind anchor resolves to the first event of that kind, absolute', () => {
    const ctx = ctxWith({
        experiment: { startSec: S + 100, events: [ev('run_start', S + 130), ev('run_start', S + 140)] },
    });
    const r = resolveAnchor({ kind: 'run_start' }, 'experiment', [S + 100, S + 101], ctx);
    assert.deepEqual(r, { sec: S + 130, resolved: true });
});

test('a missing kind falls back to the recording start, unresolved', () => {
    const ctx = ctxWith({ experiment: { startSec: S + 100, events: [ev('deploy', S + 105)] } });
    const r = resolveAnchor({ kind: 'run_start' }, 'experiment', [S + 100.5], ctx);
    assert.equal(r.sec, S + 100);
    assert.equal(r.resolved, false);
    assert.match(r.reason, /run_start/);
});

test('numeric anchors are offsets from the recording start, or the first sample without one', () => {
    const ctx = ctxWith({ baseline: { startSec: S, events: [] } });
    assert.deepEqual(resolveAnchor(0, 'baseline', [S + 3], ctx), { sec: S, resolved: true });
    assert.deepEqual(resolveAnchor(-1500, 'baseline', [S + 3], ctx), { sec: S - 1.5, resolved: true });
    // No context for this id: the first sample is the base (the legacy rule).
    assert.deepEqual(resolveAnchor(2000, 'other', [S + 3], ctx), { sec: S + 5, resolved: true });
    // No context, no data: zero.
    assert.deepEqual(resolveAnchor(undefined, 'other', [], null), { sec: 0, resolved: true });
    assert.deepEqual(resolveAnchor('abc', 'other', [S], null), { sec: S, resolved: true });
});

test('startSec takes precedence over the first sample', () => {
    const ctx = ctxWith({ baseline: { startSec: S, events: [] } });
    // A zoomed baseline refetches from later; the anchor must not move.
    assert.equal(resolveAnchor(0, 'baseline', [S + 60], ctx).sec, S);
});

test('kindsFor, kindsSharedBy and kindsAcross', () => {
    const ctx = ctxWith({
        baseline: { startSec: S, events: [ev('run_start', S + 1), ev('deploy', S + 2), { timestamp: S * 1e9 }] },
        experiment: { startSec: S, events: [ev('run_start', S + 5), ev('  ', S + 6)] },
        third: { startSec: S, events: [] },
    });
    assert.deepEqual([...ctx.kindsFor('baseline')].sort(), ['deploy', 'run_start']);
    assert.deepEqual(ctx.kindsSharedBy(['baseline', 'experiment']), ['run_start']);
    assert.deepEqual(ctx.kindsSharedBy(['baseline', 'experiment', 'third']), []);
    assert.deepEqual(ctx.kindsSharedBy([]), []);
    assert.deepEqual(ctx.kindsAcross(['baseline', 'experiment']), [
        { kind: 'deploy', missing: ['experiment'] },
        { kind: 'run_start', missing: [] },
    ]);
    ctx.clear();
    assert.deepEqual(ctx.ids(), []);
});

test('isKindAnchor', () => {
    assert.equal(isKindAnchor({ kind: 'run_start' }), true);
    assert.equal(isKindAnchor({ kind: '' }), false);
    assert.equal(isKindAnchor({ kind: 3 }), false);
    assert.equal(isKindAnchor(0), false);
    assert.equal(isKindAnchor(null), false);
});

test('events without a finite timestamp are dropped on set', () => {
    const ctx = ctxWith({ x: { events: [{ kind: 'a' }, ev('a', S)] } });
    assert.equal(ctx.get('x').events.length, 1);
});
