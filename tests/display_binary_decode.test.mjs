import test from 'node:test';
import assert from 'node:assert/strict';
import { decodeDisplayBinary } from '../src/viewer/assets/lib/data.js';

// Build a display-mode binary buffer exactly as routes.rs
// `encode_display_binary` does: [u32 LE headerLen][JSON header][pad to 8B]
// [f64 LE column blobs], columns per series in order t,min,lo,median,hi,max.
function encode(series, budget = 500) {
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
            n: s.t.length,
        })),
    };
    const headerBytes = new TextEncoder().encode(JSON.stringify(header));
    const padded = Math.ceil((4 + headerBytes.length) / 8) * 8;
    const totalFloats = series.reduce((a, s) => a + s.t.length * 6, 0);
    const buf = new ArrayBuffer(padded + totalFloats * 8);
    const dv = new DataView(buf);
    dv.setUint32(0, headerBytes.length, true);
    new Uint8Array(buf, 4, headerBytes.length).set(headerBytes);
    let off = padded;
    for (const s of series) {
        for (const name of ['t', 'min', 'lo', 'median', 'hi', 'max']) {
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
