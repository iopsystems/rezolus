import test from 'node:test';
import assert from 'node:assert/strict';
import { decodeDisplayBinary, displayInterpolated } from '../src/viewer/assets/lib/data.js';

// Build a display-mode binary buffer exactly as routes.rs
// `encode_display_binary` does: [u32 LE headerLen][JSON header][pad to 8B]
// [f64 LE column blobs], columns per series in order t,min,lo,median,hi,max.
function encode(series, budget = 500) {
    // A series carries a measurement-uncertainty band iff it has uncLo/uncHi
    // columns; the header `unc` flag tells the decoder to read the two extra
    // columns after the six boxplot columns.
    const hasUnc = (s) => Array.isArray(s.uncLo) && Array.isArray(s.uncHi);
    // ...and one more column when the series has interpolated points. Written
    // AFTER the unc pair, matching the Rust encoder's fixed order.
    const hasInterp = (s) => Array.isArray(s.interpCol);
    const header = {
        resultType: 'series',
        budget,
        series: series.map((s) => ({
            metric: s.metric,
            nativeInterval: s.nativeInterval,
            rawPoints: s.rawPoints,
            reducer: s.reducer,
            band: s.band,
            decimated: s.decimated,
            unc: hasUnc(s),
            interp: hasInterp(s),
            n: s.t.length,
        })),
    };
    const headerBytes = new TextEncoder().encode(JSON.stringify(header));
    const padded = Math.ceil((4 + headerBytes.length) / 8) * 8;
    const totalFloats = series.reduce(
        (a, s) => a + s.t.length * (6 + (hasUnc(s) ? 2 : 0) + (hasInterp(s) ? 1 : 0)),
        0,
    );
    const buf = new ArrayBuffer(padded + totalFloats * 8);
    const dv = new DataView(buf);
    dv.setUint32(0, headerBytes.length, true);
    new Uint8Array(buf, 4, headerBytes.length).set(headerBytes);
    let off = padded;
    for (const s of series) {
        const cols = [
            't', 'min', 'lo', 'median', 'hi', 'max',
            ...(hasUnc(s) ? ['uncLo', 'uncHi'] : []),
            ...(hasInterp(s) ? ['interpCol'] : []),
        ];
        for (const name of cols) {
            for (const v of s[name]) {
                dv.setFloat64(off, v, true);
                off += 8;
            }
        }
    }
    return buf;
}

test('decodeDisplayBinary: single series round-trips header + columns', () => {
    const s = {
        metric: { __name__: 'memory_free', node: 'test3' },
        nativeInterval: 1,
        rawPoints: 88951,
        reducer: 'boxplot',
        band: [0.25, 0.75],
        decimated: true,
        t: [100, 101, 102],
        min: [1, 2, 3],
        lo: [1.5, 2.5, 3.5],
        median: [2, 3, 4],
        hi: [2.5, 3.5, 4.5],
        max: [10, 3.9, 4.9],
    };
    const out = decodeDisplayBinary(encode([s]));

    assert.equal(out.resultType, 'series');
    assert.equal(out.budget, 500);
    assert.equal(out.series.length, 1);
    const d = out.series[0];
    assert.deepEqual(d.metric, s.metric);
    assert.equal(d.rawPoints, 88951);
    assert.equal(d.reducer, 'boxplot');
    assert.deepEqual(d.band, [0.25, 0.75]);
    assert.equal(d.decimated, true);
    assert.equal(d.n, 3);
    assert.ok(d.t instanceof Float64Array, 'columns are Float64Array views');
    assert.deepEqual([...d.t], [100, 101, 102]);
    assert.deepEqual([...d.median], [2, 3, 4]);
    assert.deepEqual([...d.max], [10, 3.9, 4.9]);
    // the spike (10 in max) survives decode
    assert.equal(Math.max(...d.max), 10);
});

test('decodeDisplayBinary: two series decode at correct offsets', () => {
    const mk = (name, base) => ({
        metric: { __name__: name },
        nativeInterval: 1,
        rawPoints: 10,
        reducer: 'boxplot',
        band: [0.25, 0.75],
        decimated: true,
        t: [base, base + 1],
        min: [base, base],
        lo: [base, base],
        median: [base, base],
        hi: [base, base],
        max: [base, base],
    });
    const out = decodeDisplayBinary(encode([mk('a', 1), mk('b', 100)]));
    assert.equal(out.series.length, 2);
    assert.deepEqual([...out.series[0].t], [1, 2]);
    assert.deepEqual([...out.series[1].t], [100, 101]);
    assert.equal(out.series[1].median[0], 100, 'second series columns not misaligned');
});

test('decodeDisplayBinary: reads the uncertainty band columns when present', () => {
    const s = {
        metric: { __name__: 'cpu_usage' },
        nativeInterval: 1,
        rawPoints: 100,
        reducer: 'boxplot',
        band: [0.25, 0.75],
        decimated: true,
        t: [100, 101, 102],
        min: [1, 2, 3],
        lo: [1, 2, 3],
        median: [2, 3, 4],
        hi: [2, 3, 4],
        max: [2, 3, 4],
        uncLo: [1.8, 2.8, 3.8],
        uncHi: [2.2, 3.2, 4.2],
    };
    const out = decodeDisplayBinary(encode([s]));
    const d = out.series[0];
    assert.equal(d.unc, true);
    assert.ok(d.uncLo instanceof Float64Array && d.uncHi instanceof Float64Array);
    assert.deepEqual([...d.uncLo], [1.8, 2.8, 3.8]);
    assert.deepEqual([...d.uncHi], [2.2, 3.2, 4.2]);
    // The six boxplot columns still decode correctly alongside the band.
    assert.deepEqual([...d.median], [2, 3, 4]);
});

test('decodeDisplayBinary: no band → uncLo/uncHi absent, six columns intact', () => {
    const s = {
        metric: { __name__: 'memory_free' },
        nativeInterval: 1,
        rawPoints: 3,
        reducer: 'boxplot',
        band: [0.25, 0.75],
        decimated: false,
        t: [5, 6], min: [5, 6], lo: [5, 6], median: [5, 6], hi: [5, 6], max: [5, 6],
    };
    const out = decodeDisplayBinary(encode([s]));
    const d = out.series[0];
    assert.ok(!d.unc);
    assert.equal(d.uncLo, undefined);
    assert.equal(d.uncHi, undefined);
    assert.deepEqual([...d.median], [5, 6]);
});

test('decodeDisplayBinary: mixed band/no-band series stay aligned', () => {
    const withBand = {
        metric: { __name__: 'a' }, nativeInterval: 1, rawPoints: 2,
        reducer: 'boxplot', band: [0.25, 0.75], decimated: true,
        t: [1, 2], min: [1, 1], lo: [1, 1], median: [1, 1], hi: [1, 1], max: [1, 1],
        uncLo: [0.9, 0.9], uncHi: [1.1, 1.1],
    };
    const noBand = {
        metric: { __name__: 'b' }, nativeInterval: 1, rawPoints: 2,
        reducer: 'boxplot', band: [0.25, 0.75], decimated: true,
        t: [100, 101], min: [7, 7], lo: [7, 7], median: [7, 7], hi: [7, 7], max: [7, 7],
    };
    const out = decodeDisplayBinary(encode([withBand, noBand]));
    assert.deepEqual([...out.series[0].uncHi], [1.1, 1.1]);
    assert.equal(out.series[1].uncLo, undefined);
    assert.equal(out.series[1].median[0], 7, 'second series not misaligned by first series band columns');
});

test('decodeDisplayBinary: odd-length header still 8-byte aligns the floats', () => {
    // A label chosen so the JSON header length is not a multiple of 8; if the
    // decoder mis-aligns, the Float64Array construction throws or values garble.
    const s = {
        metric: { __name__: 'x', pad: 'abc' },
        nativeInterval: 0.5,
        rawPoints: 5,
        reducer: 'boxplot',
        band: [0.1, 0.9],
        decimated: false,
        t: [7], min: [7], lo: [7], median: [7], hi: [7], max: [7],
    };
    const out = decodeDisplayBinary(encode([s]));
    assert.equal(out.series[0].median[0], 7);
    assert.equal(out.series[0].nativeInterval, 0.5);
});

// --- interpolated column -------------------------------------------------
//
// The column is optional and written LAST, after the optional unc pair. The
// ordering is the whole risk: a decoder that reads `interp` before `unc`, or
// that reads it for a series that didn't flag it, misaligns every subsequent
// series in the buffer rather than failing loudly. So the cases below cover
// each combination of the two flags, and a mixed multi-series buffer.

const base = (over = {}) => ({
    metric: { __name__: 'cpu_usage' },
    nativeInterval: 1,
    rawPoints: 10,
    reducer: 'boxplot',
    band: [0.25, 0.75],
    decimated: true,
    t: [100, 101, 102],
    min: [1, 2, 3],
    lo: [1, 2, 3],
    median: [1, 2, 3],
    hi: [1, 2, 3],
    max: [1, 2, 3],
    ...over,
});

test('interp column decodes when present without an unc pair', () => {
    const out = decodeDisplayBinary(encode([base({ interpCol: [0, 1, 0] })]));
    assert.deepEqual(Array.from(out.series[0].interpCol), [0, 1, 0]);
    assert.deepEqual(displayInterpolated(out.series[0]), [false, true, false]);
});

test('interp column is read after unc, not before it', () => {
    // The case that catches a decoder reading the columns in the wrong order:
    // both flags set, and every column holds a distinct value so a swap shows up
    // as wrong data rather than as a plausible-looking array.
    const s = base({
        uncLo: [10, 20, 30],
        uncHi: [11, 21, 31],
        interpCol: [1, 0, 1],
    });
    const got = decodeDisplayBinary(encode([s])).series[0];
    assert.deepEqual(Array.from(got.uncLo), [10, 20, 30]);
    assert.deepEqual(Array.from(got.uncHi), [11, 21, 31]);
    assert.deepEqual(Array.from(got.interpCol), [1, 0, 1]);
    assert.deepEqual(displayInterpolated(got), [true, false, true]);
});

test('a series without the flag exposes no interp column and no flags', () => {
    const got = decodeDisplayBinary(encode([base()])).series[0];
    assert.equal(got.interp, false);
    assert.equal(got.interpCol, undefined);
    assert.equal(displayInterpolated(got), null);
});

test('displayInterpolated returns null when the column is present but all clear', () => {
    // Distinct from "no column": the encoder only sets the flag when some point
    // is interpolated, but a decimated bucket could in principle clear them all.
    // Both must read as falsy so the renderer draws no overlay.
    const got = decodeDisplayBinary(encode([base({ interpCol: [0, 0, 0] })])).series[0];
    assert.equal(displayInterpolated(got), null);
});

test('mixed series stay aligned when only some carry the extra columns', () => {
    // The alignment case that matters in practice: three series in one buffer
    // with different flag combinations. If the decoder mis-sizes any of them,
    // the LATER series read garbage — so assert the last one hardest.
    const a = base({ metric: { __name__: 'a' }, interpCol: [1, 1, 0] });
    const b = base({ metric: { __name__: 'b' }, t: [200, 201, 202], median: [4, 5, 6] });
    const c = base({
        metric: { __name__: 'c' },
        t: [300, 301, 302],
        median: [7, 8, 9],
        uncLo: [1, 1, 1],
        uncHi: [2, 2, 2],
        interpCol: [0, 0, 1],
    });
    const out = decodeDisplayBinary(encode([a, b, c])).series;
    assert.deepEqual(displayInterpolated(out[0]), [true, true, false]);
    assert.equal(displayInterpolated(out[1]), null);
    assert.deepEqual(Array.from(out[2].t), [300, 301, 302]);
    assert.deepEqual(Array.from(out[2].median), [7, 8, 9]);
    assert.deepEqual(Array.from(out[2].uncHi), [2, 2, 2]);
    assert.deepEqual(displayInterpolated(out[2]), [false, false, true]);
});
