// Points a producer never observed the interval of.
//
// `rate()` spans a hole in a series — the total across it is known even though
// its distribution inside is not — but such a point carries no uncertainty
// band, because the honest bound on an unobserved interval is not a number.
// The `interpolated` flag is the only thing distinguishing it from a measured
// point, and these cover the path from the wire response to the echarts series
// that draws it differently.
import test from 'node:test';
import assert from 'node:assert/strict';

// The chart modules read CSS custom properties at load; stub the DOM so their
// palettes fall back to literals (same shim the other chart tests use).
globalThis.document = globalThis.document || { documentElement: {} };
globalThis.getComputedStyle =
    globalThis.getComputedStyle || (() => ({ getPropertyValue: () => '' }));

const { parseIntervals, parseInterpolated } = await import('../src/viewer/assets/lib/data.js');

// The engine's response for a counter that reads at 3s/4s/5s, goes null, and
// returns at 8s/9s. `bands` is null exactly where nobody observed the interval.
const gapped = {
    values: [[4, 100], [5, 100], [6, 133.33], [7, 133.33], [8, 133.33], [9, 100]],
    bands: [[95.24, 105.26], [95.24, 105.26], null, null, null, [95.24, 105.26]],
    interpolated: [false, false, true, true, true, false],
};

test('bands survive the points that have them when others do not', () => {
    const iv = parseIntervals(gapped);
    assert.equal(iv.length, 6, 'parallel to values');
    assert.deepEqual(
        iv.map((p) => p !== null),
        [true, true, false, false, false, true],
        'a hole nulls its own entry and leaves the rest alone',
    );
    assert.deepEqual(iv[0], [95.24, 105.26]);
});

test('bands is preferred over the all-or-nothing intervals field', () => {
    // A response carrying both: `intervals` is the lossy legacy view and goes
    // absent for the whole series as soon as one point lacks a band, so a
    // reader that trusts it loses bands it could have drawn.
    const both = { ...gapped, intervals: undefined };
    assert.ok(parseIntervals(both), 'bands alone is enough');

    // And an older response with only `intervals` still parses, unchanged.
    const legacy = {
        values: [[1, 5], [2, 6]],
        intervals: [[4, 6], [5, 7]],
    };
    assert.deepEqual(parseIntervals(legacy), [[4, 6], [5, 7]]);
});

test('interpolated parses to a boolean array, or null when nothing is', () => {
    assert.deepEqual(parseInterpolated(gapped), [false, false, true, true, true, false]);
    assert.equal(
        parseInterpolated({ interpolated: [false, false] }),
        null,
        'all-false is the same as absent — nothing to draw',
    );
    assert.equal(parseInterpolated({}), null, 'absent for non-rate queries');
    assert.equal(parseInterpolated({ interpolated: 'nope' }), null, 'malformed');
});

const { buildInterpolatedSeries } = await import('../src/viewer/assets/lib/charts/line.js');

const series = (flags) => ({
    name: 'probe',
    color: '#2E5BFF',
    timeData: [4, 5, 6, 7, 8, 9],
    valueData: [100, 100, 133.33, 133.33, 133.33, 100],
    interpolated: flags,
});

test('the overlay covers the hole and anchors to the measured line', () => {
    const [s] = buildInterpolatedSeries(series(gapped.interpolated), null);
    assert.ok(s, 'an overlay is produced');

    const drawn = s.data.map(([, v]) => v !== null);
    // Indices 2,3,4 are interpolated; 1 and 5 are their measured neighbours and
    // must be drawn too, or the dashed segment floats detached from the line.
    assert.deepEqual(drawn, [false, true, true, true, true, true]);
    assert.equal(s.lineStyle.type, 'dashed');
    assert.ok(s.lineStyle.opacity < 1, 'desaturated relative to the nominal');
    assert.equal(s.silent, true, 'the nominal line owns the tooltip');
    assert.equal(s.color ?? s.lineStyle.color, '#2E5BFF', 'keeps the series identity');
});

test('no overlay when nothing is interpolated', () => {
    assert.deepEqual(buildInterpolatedSeries(series([false, false, false, false, false, false]), null), []);
    assert.deepEqual(buildInterpolatedSeries(series(null), null), []);
    assert.deepEqual(
        buildInterpolatedSeries(series([true, true]), null),
        [],
        'a flags array that does not match the data length is ignored',
    );
});

// --- the display path -----------------------------------------------------
//
// These exist because the feature shipped broken on this path once already:
// line.js draws a plain line ONLY when the plot carries no boxplot columns, and
// the display path always carries them. So the overlay was wired into the
// branch users never hit, and every unit test above still passed. The default
// path needs its own coverage.

const { buildBoxplotSeries, buildInterpolatedOverlay } =
    await import('../src/viewer/assets/lib/charts/boxplot.js');

// One decimated series: measured at 4s/5s, unobserved across 6s/7s/8s, measured
// again at 9s — the same shape as `gapped` above, in decoded-column form.
const cols = {
    t: [4, 5, 6, 7, 8, 9],
    min: [100, 100, 133, 133, 133, 100],
    lo: [100, 100, 133, 133, 133, 100],
    median: [100, 100, 133, 133, 133, 100],
    hi: [100, 100, 133, 133, 133, 100],
    max: [100, 100, 133, 133, 133, 100],
    uncLo: [95, 95, NaN, NaN, NaN, 95],
    uncHi: [105, 105, NaN, NaN, NaN, 105],
};
const FLAGS = [false, false, true, true, true, false];

const medianOf = (out) => out.find((s) => s.name && s.lineStyle?.type !== 'dashed');
const overlayOf = (out) => out.find((s) => s.lineStyle?.type === 'dashed');
const yOf = (d) => (d && d.value ? d.value[1] : d ? d[1] : null);

test('the display path draws a dashed overlay, not just the plain-line path', () => {
    const out = buildBoxplotSeries(cols, { name: 'cpu_gap', interpolated: FLAGS });
    const overlay = overlayOf(out);
    assert.ok(overlay, 'boxplot render must emit the interpolated overlay');
    assert.equal(overlay.lineStyle.opacity, 0.35);
    assert.equal(overlay.silent, true);
});

test('interpolated points are cut out of the median line', () => {
    const out = buildBoxplotSeries(cols, { name: 'cpu_gap', interpolated: FLAGS });
    const ys = medianOf(out).data.map(yOf);
    assert.deepEqual(ys, [100, 100, null, null, null, 100]);
});

test('a measured point stranded between holes stays visible', () => {
    // Index 5 is measured but both neighbours are gone, and a lone point on a
    // `symbol: 'none'` line draws nothing — so it must carry its own symbol or
    // the only observed sample there disappears.
    const out = buildBoxplotSeries(cols, { name: 'cpu_gap', interpolated: FLAGS });
    const last = medianOf(out).data[5];
    assert.equal(last.symbol, 'circle', 'stranded measured point needs a symbol');
    assert.equal(yOf(last), 100);
    // A point with a measured neighbour must NOT get one, or every series
    // sprouts dots.
    assert.equal(medianOf(out).data[0].symbol, undefined);
});

test('no flags means the median is untouched and no overlay appears', () => {
    const out = buildBoxplotSeries(cols, { name: 'cpu_gap' });
    assert.equal(overlayOf(out), undefined);
    assert.deepEqual(medianOf(out).data.map(yOf), [100, 100, 133, 133, 133, 100]);
});

test('a flags array of the wrong length is ignored rather than misapplied', () => {
    // Defensive: mis-indexing flags against values would mislabel which points
    // were observed, which is worse than showing no overlay at all.
    const out = buildBoxplotSeries(cols, { name: 'cpu_gap', interpolated: [true, false] });
    assert.equal(overlayOf(out), undefined);
    assert.deepEqual(medianOf(out).data.map(yOf), [100, 100, 133, 133, 133, 100]);
});

test('the overlay anchors one point either side of the run', () => {
    const overlay = buildInterpolatedOverlay({
        t: cols.t, values: cols.median, flags: FLAGS, name: 'x', color: '#fff',
    })[0];
    // index 0 is two steps from the run → null; 1 and 5 are the anchors.
    assert.deepEqual(overlay.data.map((d) => d[1]), [null, 100, 133, 133, 133, 100]);
});

test('both render paths style a hole identically', () => {
    // The point of sharing the builder: one recording must not look different
    // depending on which path fetched it.
    const viaDisplay = overlayOf(buildBoxplotSeries(cols, { name: 'x', interpolated: FLAGS }));
    const viaMatrix = buildInterpolatedOverlay({
        t: cols.t, values: cols.median, flags: FLAGS, name: 'x', color: viaDisplay.lineStyle.color,
    })[0];
    assert.deepEqual(viaMatrix.lineStyle, viaDisplay.lineStyle);
    assert.equal(viaMatrix.silent, viaDisplay.silent);
});

// --- stranded measured points, and the compare-mode path ------------------
//
// Both found by systemslab-e3 rendering the feature against its own chart
// stack, after I shipped it. The shared theme: a hole adjacent to another hole
// (or to the series edge) is the case a single-gap fixture never produces, and
// a hand-picked field list drops data that the wire carried correctly.

const { isStranded } = await import('../src/viewer/assets/lib/charts/boxplot.js');
const { promqlResultToLinePair } = await import('../src/viewer/assets/lib/data.js');

test('isStranded finds measured points with holes on both sides', () => {
    //            0      1     2      3     4      5
    const f = [false, true, false, true, false, false];
    assert.equal(isStranded(f, 2, 6), true, 'index 2 sits between two holes');
    assert.equal(isStranded(f, 4, 6), false, 'index 4 has a measured neighbour');
    assert.equal(isStranded(f, 1, 6), false, 'an interpolated point is never stranded');
});

test('isStranded treats the series edges as missing neighbours', () => {
    assert.equal(isStranded([false, true, true], 0, 3), true, 'first point, hole after');
    assert.equal(isStranded([true, true, false], 2, 3), true, 'last point, hole before');
    assert.equal(isStranded([false, false, true], 0, 3), false, 'first point, measured after');
    assert.equal(isStranded(null, 0, 3), false, 'no flags at all');
});

test('the two-hole case: one observed sample between two holes keeps a symbol', () => {
    // The exact shape that produced the bug: 22..29 and 31..37 unobserved,
    // leaving index 30 as the ONLY measured sample in the region. A single
    // contiguous hole never exercises this, which is why it shipped.
    const n = 60;
    const flags = Array.from({ length: n }, (_, i) =>
        (i >= 22 && i < 30) || (i >= 31 && i < 38));
    assert.equal(flags[30], false, 'index 30 is the observed sample');
    assert.equal(isStranded(flags, 30, n), true);
    // and its neighbours are not, so exactly one dot appears here
    assert.equal(isStranded(flags, 29, n), false);
    assert.equal(isStranded(flags, 38, n), false);
    assert.equal(flags.filter((_, i) => isStranded(flags, i, n)).length, 1);
});

test('a single contiguous hole strands nothing — the control case', () => {
    // Renders correctly today, and is the reference the failing case is read
    // against. Kept so a future change can't "fix" the two-hole case by
    // sprinkling dots everywhere.
    const n = 60;
    const flags = Array.from({ length: n }, (_, i) => i >= 24 && i < 36);
    assert.equal(flags.filter((_, i) => isStranded(flags, i, n)).length, 0);
});

test('promqlResultToLinePair carries interpolated, not just intervals', () => {
    // The compare-mode drop: this function rebuilds a series from a hand-picked
    // field list, so a field absent from that list vanishes despite arriving on
    // the wire — with a green build and a correct response.
    const pair = promqlResultToLinePair([{
        values: [[1, '10'], [2, '20'], [3, '30']],
        bands: [[9, 11], null, [29, 31]],
        interpolated: [false, true, false],
    }]);
    assert.deepEqual(pair.interpolated, [false, true, false]);
    assert.equal(pair.intervals.length, 3);
    assert.equal(pair.intervals[1], null, 'no band where interpolated');
});

test('promqlResultToLinePair leaves interpolated null for a plain series', () => {
    const pair = promqlResultToLinePair([{ values: [[1, '10'], [2, '20']] }]);
    assert.equal(pair.interpolated, null);
    assert.equal(pair.intervals, null);
});
