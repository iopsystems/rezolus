// Parity guard: the WASM viewer's `timestamps()` passthrough must return the
// same raw, un-gridded per-sample collection timestamps as the server's
// `/api/v1/timestamps` endpoint. Both backends read `MetricsSource::
// sample_timestamps()` off the same `ParquetReader` and wrap it in the same
// `{"source","timestamps"}` shape, so identity holds by construction — this
// test only pins the WASM side's shape and sanity-checks the fixture's
// values (real jitter, not a synthetic grid).
//
// Requires the WASM bundle to be built first: `./crates/viewer/build.sh`.
// The server side is covered by the `timestamps` handler in
// src/viewer/routes.rs (TimestampsResponse).
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath, pathToFileURL } from 'node:url';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const pkgJs = path.join(repoRoot, 'site/viewer/pkg/wasm_viewer.js');
const pkgWasm = path.join(repoRoot, 'site/viewer/pkg/wasm_viewer_bg.wasm');

// Skip cleanly if the bundle hasn't been built — this test can't run without it.
if (!fs.existsSync(pkgJs) || !fs.existsSync(pkgWasm)) {
    test('WASM timestamps (bundle not built — skipped)', { skip: true }, () => {});
} else {
    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'rezolus-wasm-timestamps-'));
    const wasmJsCopy = path.join(tmpDir, 'wasm_viewer.mjs');
    fs.copyFileSync(pkgJs, wasmJsCopy);

    const { initSync, WasmCaptureRegistry } = await import(pathToFileURL(wasmJsCopy).href);
    initSync({ module: fs.readFileSync(pkgWasm) });

    const parquet = fs.readFileSync(path.join(repoRoot, 'site/viewer/data/simple_capture.parquet'));

    test('WASM timestamps returns raw jittered timestamps', () => {
        const registry = new WasmCaptureRegistry();
        registry.attach('baseline', new Uint8Array(parquet), 'simple_capture.parquet');

        const resp = JSON.parse(registry.timestamps('baseline', null));

        assert.equal(typeof resp.source, 'string');
        assert.ok(Array.isArray(resp.timestamps));
        assert.ok(resp.timestamps.length > 1, `expected >1 timestamps, got ${resp.timestamps.length}`);
        assert.ok(
            resp.timestamps.every((t) => typeof t === 'number' && t > 0),
            'every timestamp must be a positive number',
        );

        // Real per-sample jitter, not a synthetic evenly-spaced grid: at
        // least one delta between consecutive timestamps must differ from
        // the rest.
        const deltas = resp.timestamps.slice(1).map((t, i) => t - resp.timestamps[i]);
        const allEqual = deltas.every((d) => d === deltas[0]);
        assert.ok(!allEqual, `expected jittered deltas, got constant deltas: ${JSON.stringify(deltas)}`);
    });
}
