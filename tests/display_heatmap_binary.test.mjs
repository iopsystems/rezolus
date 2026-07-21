// Binary histogram-heatmap wire format (routes.rs encode_heatmap_binary ↔
// data.js decodeHeatmapBinary). Pins the layout:
//   [u32 LE headerLen][JSON header][pad to 8B]
//   [f64 timestamps][f64 counts][u32 timeIdx][u32 bucketIdx]
// The decoder reconstructs the [timeIdx,bucketIdx,count] triples so the output
// shape matches the JSON path exactly.
import { test } from 'node:test';
import assert from 'node:assert';
import { decodeHeatmapBinary } from '../src/viewer/assets/lib/data.js';

// Encode the same way the Rust side does.
function encode({ bucketBounds, minValue, maxValue, timestamps, triples }) {
    const header = JSON.stringify({
        resultType: 'histogram_heatmap',
        bucketBounds, minValue, maxValue,
        nTimestamps: timestamps.length, nTriples: triples.length,
    });
    const headerBytes = new TextEncoder().encode(header);
    let off = 4 + headerBytes.length;
    const pad = (Math.ceil(off / 8) * 8) - off;
    const nTs = timestamps.length, nTr = triples.length;
    const total = 4 + headerBytes.length + pad + nTs * 8 + nTr * 8 + nTr * 4 + nTr * 4;
    const buf = new ArrayBuffer(total);
    const dv = new DataView(buf);
    let p = 0;
    dv.setUint32(p, headerBytes.length, true); p += 4;
    new Uint8Array(buf, p, headerBytes.length).set(headerBytes); p += headerBytes.length;
    p += pad;
    for (const t of timestamps) { dv.setFloat64(p, t, true); p += 8; }
    for (const [, , c] of triples) { dv.setFloat64(p, c, true); p += 8; }
    for (const [ti] of triples) { dv.setUint32(p, ti, true); p += 4; }
    for (const [, bi] of triples) { dv.setUint32(p, bi, true); p += 4; }
    return buf;
}

test('decodeHeatmapBinary round-trips the columnar layout', () => {
    const input = {
        bucketBounds: [1, 2, 4, 8, 16],
        minValue: 1, maxValue: 42,
        timestamps: [1000.5, 1001.5, 1002.5],
        triples: [[0, 1, 5], [0, 2, 9], [1, 3, 42], [2, 0, 1]],
    };
    const d = decodeHeatmapBinary(encode(input));
    assert.deepEqual(Array.from(d.time_data), input.timestamps);
    assert.deepEqual(d.bucket_bounds, input.bucketBounds);
    assert.equal(d.min_value, 1);
    assert.equal(d.max_value, 42);
    assert.equal(d.data.length, 4);
    assert.deepEqual(d.data[0], [0, 1, 5]);
    assert.deepEqual(d.data[2], [1, 3, 42]);
});

test('decodeHeatmapBinary rejects a non-heatmap resultType', () => {
    // Header with the wrong resultType, no columns.
    const header = JSON.stringify({ resultType: 'series', nTimestamps: 0, nTriples: 0 });
    const hb = new TextEncoder().encode(header);
    const off = 4 + hb.length;
    const buf = new ArrayBuffer(Math.ceil(off / 8) * 8);
    const dv = new DataView(buf);
    dv.setUint32(0, hb.length, true);
    new Uint8Array(buf, 4, hb.length).set(hb);
    assert.throws(() => decodeHeatmapBinary(buf), /resultType/);
});
