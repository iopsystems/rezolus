// Event-anchored alignment: two captures recorded ten seconds apart, each
// with a `run_start` event ten seconds apart, overlay to coincident relative
// time when both anchors name that kind; a capture without the kind falls
// back to its start.
import test from 'node:test';
import assert from 'node:assert/strict';

globalThis.document = globalThis.document || { documentElement: {} };
globalThis.getComputedStyle = globalThis.getComputedStyle || (() => ({ getPropertyValue: () => '' }));
const { renderCompareChart, diffTimeShift } = await import('../src/viewer/assets/lib/charts/compare.js');
const { CaptureContext } = await import('../src/viewer/assets/lib/events/capture_events.js');
const { CAPTURE_BASELINE, CAPTURE_EXPERIMENT } = await import('../src/viewer/assets/lib/data.js');

const S = 1_700_000_000;
const ev = (kind, sec) => ({ kind, timestamp: sec * 1e9, description: kind });
const cap = (id, start) => ({
    id,
    timeData: [start, start + 1, start + 2, start + 3],
    valueData: [1, 2, 3, 4],
});

const multiSeriesOf = (out) => {
    assert.equal(out.kind, 'spec');
    return out.spec.multiSeries;
};

test('both captures anchored on run_start overlay to the same relative time', () => {
    const ctx = new CaptureContext();
    ctx.set(CAPTURE_BASELINE, { startSec: S, events: [ev('run_start', S + 1)] });
    ctx.set(CAPTURE_EXPERIMENT, { startSec: S + 10, events: [ev('run_start', S + 11)] });
    const out = renderCompareChart({
        spec: { opts: { style: 'line' } },
        captures: [cap(CAPTURE_BASELINE, S), cap(CAPTURE_EXPERIMENT, S + 10)],
        anchors: { baseline: { kind: 'run_start' }, experiment: { kind: 'run_start' } },
        captureContext: ctx,
        captureLabels: {},
    });
    const [b, e] = multiSeriesOf(out);
    assert.deepEqual(b.timeData, e.timeData);
    // The event sits one second into each capture, so +0s is one second in.
    assert.deepEqual(b.timeData, [-1, 0, 1, 2]);
    assert.ok(out.spec.divergenceBand, 'coincident grids get a divergence band');
    // Event markers for the overlay are placed by the first capture's anchor.
    assert.equal(out.spec.eventTimeOriginSec, S + 1);
});

test('a capture without the kind falls back to its recording start', () => {
    const ctx = new CaptureContext();
    ctx.set(CAPTURE_BASELINE, { startSec: S, events: [ev('run_start', S + 1)] });
    ctx.set(CAPTURE_EXPERIMENT, { startSec: S + 10, events: [ev('deploy', S + 12)] });
    const out = renderCompareChart({
        spec: { opts: { style: 'line' } },
        captures: [cap(CAPTURE_BASELINE, S), cap(CAPTURE_EXPERIMENT, S + 10)],
        anchors: { baseline: { kind: 'run_start' }, experiment: { kind: 'run_start' } },
        captureContext: ctx,
        captureLabels: {},
    });
    const [b, e] = multiSeriesOf(out);
    assert.deepEqual(b.timeData, [-1, 0, 1, 2]);
    assert.deepEqual(e.timeData, [0, 1, 2, 3]);
});

test('numeric anchors are measured from the recording start, not the first fetched sample', () => {
    const ctx = new CaptureContext();
    ctx.set(CAPTURE_BASELINE, { startSec: S, events: [] });
    ctx.set(CAPTURE_EXPERIMENT, { startSec: S + 10, events: [] });
    // The baseline was zoomed and refetched from two seconds in.
    const zoomedBaseline = { id: CAPTURE_BASELINE, timeData: [S + 2, S + 3], valueData: [3, 4] };
    const out = renderCompareChart({
        spec: { opts: { style: 'line' } },
        captures: [zoomedBaseline, cap(CAPTURE_EXPERIMENT, S + 10)],
        anchors: { baseline: 0, experiment: 1000 },
        captureContext: ctx,
        captureLabels: {},
    });
    const [b, e] = multiSeriesOf(out);
    assert.deepEqual(b.timeData, [2, 3]);
    assert.deepEqual(e.timeData, [-1, 0, 1, 2]);
});

test('diff views shift the experiment index by the anchor difference in steps', () => {
    const ctx = new CaptureContext();
    ctx.set(CAPTURE_BASELINE, { startSec: S, events: [ev('run_start', S + 1)] });
    ctx.set(CAPTURE_EXPERIMENT, { startSec: S + 10, events: [ev('run_start', S + 13)] });
    const aTime = [S, S + 1, S + 2, S + 3];
    const bTime = [S + 10, S + 11, S + 12, S + 13];
    // Set the context the way renderCompareChart does for the module.
    renderCompareChart({ spec: { opts: { style: 'line' } }, captures: [cap(CAPTURE_BASELINE, S), cap(CAPTURE_EXPERIMENT, S + 10)], anchors: {}, captureContext: ctx, captureLabels: {} });
    // Default anchors (recording start): both first samples sit at +0s; no shift.
    assert.deepEqual(diffTimeShift(CAPTURE_BASELINE, CAPTURE_EXPERIMENT, {}, aTime, bTime), { k: 0, ok: true });
    // Event anchors 1 s and 3 s into their recordings: experiment column c+2 pairs with baseline column c.
    const kind = { baseline: { kind: 'run_start' }, experiment: { kind: 'run_start' } };
    assert.deepEqual(diffTimeShift(CAPTURE_BASELINE, CAPTURE_EXPERIMENT, kind, aTime, bTime), { k: 2, ok: true });
    // A half-step difference has no cell pairing.
    assert.equal(diffTimeShift(CAPTURE_BASELINE, CAPTURE_EXPERIMENT, { baseline: 0, experiment: 500 }, aTime, bTime).ok, false);
    // No data on one side: nothing to pair, nothing to refuse.
    assert.deepEqual(diffTimeShift(CAPTURE_BASELINE, CAPTURE_EXPERIMENT, kind, aTime, []), { k: 0, ok: true });
});

test('a family baseline draws one band plus its mean, and the experiment over it', () => {
    const ctx = new CaptureContext();
    const three = [cap(CAPTURE_BASELINE, S), cap(CAPTURE_EXPERIMENT, S + 10), { ...cap('third', S + 20), valueData: [3, 4, 5, 6] }];
    const out = renderCompareChart({
        spec: { opts: { style: 'line' } },
        captures: three,
        anchors: {},
        family: { kind: 'envelope' },
        captureContext: ctx,
        captureLabels: {},
    });
    assert.equal(out.kind, 'spec');
    const ms = out.spec.multiSeries;
    assert.equal(ms.length, 2);
    assert.equal(ms[0].name, 'family min..max (2 members)');
    assert.equal(ms[1].name, 'experiment');
    // Members baseline [1,2,3,4] and third [3,4,5,6] on the same relative grid.
    assert.deepEqual(out.spec.familyBand.lower, [1, 2, 3, 4]);
    assert.deepEqual(out.spec.familyBand.upper, [3, 4, 5, 6]);
    assert.deepEqual(ms[0].valueData, [2, 3, 4, 5]);
    assert.equal(out.spec.divergenceBand, undefined);
    // Two captures: the setting is ignored and the plain overlay stands.
    const two = renderCompareChart({
        spec: { opts: { style: 'line' } },
        captures: three.slice(0, 2),
        anchors: {},
        family: { kind: 'envelope' },
        captureContext: ctx,
        captureLabels: {},
    });
    assert.equal(two.spec.multiSeries.length, 2);
    assert.equal(two.spec.familyBand, undefined);
});

test('without a context, the legacy first-sample rule holds', () => {
    const out = renderCompareChart({
        spec: { opts: { style: 'line' } },
        captures: [cap(CAPTURE_BASELINE, S), cap(CAPTURE_EXPERIMENT, S + 10)],
        anchors: { baseline: 0, experiment: 0 },
        captureContext: new CaptureContext(),
        captureLabels: {},
    });
    const [b, e] = multiSeriesOf(out);
    assert.deepEqual(b.timeData, [0, 1, 2, 3]);
    assert.deepEqual(e.timeData, [0, 1, 2, 3]);
});
