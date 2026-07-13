// LOD tile cache: a drill-down is served from a cached tile (no network) when
// the tile covers the window at sufficient resolution; the tile is clipped to
// the window. Coarse tiles (too few points in the window) must miss so the
// caller refetches finer detail.
import { test } from 'node:test';
import assert from 'node:assert';
import {
    clipDecoded, tileLookup, tileStore, clearDisplayTiles,
} from '../src/viewer/assets/lib/data.js';

// Build a decoded response with one series over [start, end] at `n` evenly
// spaced points, matching decodeDisplayBinary's shape.
const mkDecoded = (start, end, n) => {
    const t = new Float64Array(n);
    const col = (f) => { const a = new Float64Array(n); for (let i = 0; i < n; i++) a[i] = f(i); return a; };
    for (let i = 0; i < n; i++) t[i] = start + (end - start) * (i / (n - 1));
    return {
        resultType: 'series', budget: n,
        series: [{
            metric: { __name__: 'm' }, n, decimated: true,
            t, min: col(() => 0), lo: col(() => 1), median: col((i) => i),
            hi: col(() => 3), max: col(() => 4),
        }],
    };
};

test('clipDecoded slices columns to the [ns,ne] window', () => {
    const d = mkDecoded(0, 100, 101); // t = 0,1,...,100
    const c = clipDecoded(d, 20, 30);
    const s = c.series[0];
    assert.equal(s.t[0], 20);
    assert.equal(s.t[s.t.length - 1], 30);
    assert.equal(s.n, s.t.length);
    // median[i] == original index, so first clipped median is 20
    assert.equal(s.median[0], 20);
});

test('tileLookup serves a covering, high-resolution tile (clipped)', () => {
    clearDisplayTiles();
    // Tile over [0,100] with 101 points (~1 pt/sec).
    tileStore('q', 0, 100, mkDecoded(0, 100, 101));
    // Zoom to [40,60] wanting budget 15 — the tile has 21 pts there ≥ 0.9*15.
    const hit = tileLookup('q', 40, 60, 15);
    assert.ok(hit, 'expected a cache hit');
    assert.equal(hit.series[0].t[0], 40);
    assert.equal(hit.series[0].t[hit.series[0].t.length - 1], 60);
});

test('tileLookup misses when the covering tile is too coarse', () => {
    clearDisplayTiles();
    // Coarse tile over [0,100] with only 11 points (~1 pt / 10s).
    tileStore('q', 0, 100, mkDecoded(0, 100, 11));
    // Zoom to [40,60] wanting budget 15: the tile has ~3 pts there < 0.9*15.
    assert.equal(tileLookup('q', 40, 60, 15), null);
});

test('tileLookup misses when no tile covers the window', () => {
    clearDisplayTiles();
    tileStore('q', 0, 50, mkDecoded(0, 50, 51));
    assert.equal(tileLookup('q', 40, 80, 10), null); // 80 > tile end 50
});

test('clearDisplayTiles empties the cache', () => {
    clearDisplayTiles();
    tileStore('q', 0, 100, mkDecoded(0, 100, 101));
    assert.ok(tileLookup('q', 10, 20, 5));
    clearDisplayTiles();
    assert.equal(tileLookup('q', 10, 20, 5), null);
});
