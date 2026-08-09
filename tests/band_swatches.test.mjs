import assert from 'node:assert/strict';
import test from 'node:test';
import { chartSwatches } from '../src/viewer/assets/lib/charts/swatches.js';

// Decoded display series shape (data.js decodeDisplayBinary): parallel columns
// t/min/lo/median/hi/max, optional uncLo/uncHi, plus the wire header's `band`
// quantile levels. Plain arrays here — the builder must not require typed arrays.
const spread = {
    t: [1, 2, 3],
    min: [80, 82, 79],
    lo: [95, 96, 94],
    median: [100, 101, 99],
    hi: [107, 108, 106],
    max: [400, 120, 115],
    band: [0.25, 0.75],
};

const collapsed = {
    t: [1, 2, 3],
    min: [100, 101, 99],
    lo: [100, 101, 99],
    median: [100, 101, 99],
    hi: [100, 101, 99],
    max: [100, 101, 99],
    band: [0.25, 0.75],
};

const kinds = (sw) => sw.map((s) => s.kind);

test('no data -> no swatches', () => {
    assert.deepEqual(chartSwatches({}), []);
    assert.deepEqual(chartSwatches({ boxplot: [], intervals: [] }), []);
});

test('native resolution (all bands collapsed) -> no swatches', () => {
    assert.deepEqual(chartSwatches({ boxplot: [collapsed] }), []);
});

test('decimated spread -> median, inner, outer in inside-out order', () => {
    const sw = chartSwatches({ boxplot: [spread] });
    assert.deepEqual(kinds(sw), ['median', 'inner', 'outer']);
});

test('inner label derives from band quantiles (default IQR)', () => {
    const sw = chartSwatches({ boxplot: [spread] });
    const inner = sw.find((s) => s.kind === 'inner');
    assert.match(inner.label, /p25–p75/);
});

test('inner label follows custom band quantiles', () => {
    const custom = { ...spread, band: [0.05, 0.95] };
    const sw = chartSwatches({ boxplot: [custom] });
    assert.match(sw.find((s) => s.kind === 'inner').label, /p5–p95/);
});

test('outer spread with collapsed inner -> no inner swatch', () => {
    const outerOnly = { ...spread, lo: spread.median, hi: spread.median };
    const sw = chartSwatches({ boxplot: [outerOnly] });
    assert.deepEqual(kinds(sw), ['median', 'outer']);
});

test('uncertainty columns with real width -> bounds swatch last', () => {
    const withUnc = { ...spread, uncLo: [98, 99, 97], uncHi: [102, 103, 101] };
    const sw = chartSwatches({ boxplot: [withUnc] });
    assert.deepEqual(kinds(sw), ['median', 'inner', 'outer', 'bounds']);
});

test('NaN-only or collapsed uncertainty columns -> no bounds swatch', () => {
    const nanUnc = { ...spread, uncLo: [NaN, NaN, NaN], uncHi: [NaN, NaN, NaN] };
    assert.deepEqual(kinds(chartSwatches({ boxplot: [nanUnc] })), ['median', 'inner', 'outer']);
    const flatUnc = { ...spread, uncLo: [100, 101, 99], uncHi: [100, 101, 99] };
    assert.deepEqual(kinds(chartSwatches({ boxplot: [flatUnc] })), ['median', 'inner', 'outer']);
});

test('native-resolution uncertainty band (intervals, no boxplot) -> bounds only', () => {
    const intervals = [[[98, 102], null, [97, 101]]];
    assert.deepEqual(kinds(chartSwatches({ intervals })), ['bounds']);
});

test('collapsed or absent intervals -> no swatches', () => {
    assert.deepEqual(chartSwatches({ intervals: [[[100, 100], null]] }), []);
    assert.deepEqual(chartSwatches({ intervals: [null] }), []);
});

test('every swatch carries a label and an explanatory title', () => {
    const withUnc = { ...spread, uncLo: [98, 99, 97], uncHi: [102, 103, 101] };
    for (const s of chartSwatches({ boxplot: [withUnc] })) {
        assert.ok(s.label && s.label.length > 0, `label for ${s.kind}`);
        assert.ok(s.title && s.title.length > 10, `title for ${s.kind}`);
    }
});

test('bounds title disclaims statistical reading', () => {
    const sw = chartSwatches({ intervals: [[[98, 102]]] });
    assert.match(sw[0].title, /not a statistical confidence interval/i);
});
