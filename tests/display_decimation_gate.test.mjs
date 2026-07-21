// Display mode collapses a line/percentile series to a median + min/max spread,
// which is the right rendering when the window is wide and the server must
// aggregate many native samples per pixel. But that path drops the per-observation
// measurement-uncertainty intervals — so when the user zooms in far enough that NO
// aggregation is needed, we want the JSON matrix path instead (it carries the
// intervals, so the uncertainty bands render). `willDecimate` is the gate: true
// when the display fetch would aggregate (wide window → spread), false when the
// window is narrow enough to return native resolution (→ matrix path → bands).
import { test } from 'node:test';
import assert from 'node:assert';
import { willDecimate, setRangeOverride } from '../src/viewer/assets/lib/data.js';

// interval = 1s. MIN_DISPLAY_BUCKETS is 48, so a window of ≤48 native samples
// never decimates regardless of pixel budget.
const META = { minTime: 0, maxTime: 100000, interval: 1 };

test('narrow window (native ≤ bucket floor) does not decimate', () => {
    setRangeOverride({ start: 1000, end: 1040 }); // 40 native samples < 48 floor
    try {
        assert.equal(willDecimate(META), false);
    } finally {
        setRangeOverride(null);
    }
});

test('wide window (native far above budget) decimates', () => {
    setRangeOverride({ start: 0, end: 100000 }); // 100k native samples
    try {
        assert.equal(willDecimate(META), true);
    } finally {
        setRangeOverride(null);
    }
});

test('missing/invalid interval falls back to 1s and still gates', () => {
    setRangeOverride({ start: 0, end: 10 }); // 10 native samples, tiny
    try {
        assert.equal(willDecimate({ minTime: 0, maxTime: 10 }), false);
    } finally {
        setRangeOverride(null);
    }
});
