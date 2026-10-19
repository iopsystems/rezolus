// The static-site viewer must open a `.rez` archive, not just a parquet.
// `rezolus` is a binary-only crate, so nothing could depend on its `.rez`
// reader; the format now lives in `crates/rez`, which this bundle links.
//
// The behaviour is unit-tested natively in `crates/viewer` — this test is the
// one that runs the shipped WASM in a JS runtime, which is where a
// wasm32-incompatible dependency (threads, `linkme`, a filesystem call) shows
// up as a runtime abort rather than a compile error.
//
// Requires the WASM bundle: `./crates/viewer/build.sh`.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath, pathToFileURL } from 'node:url';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const pkgJs = path.join(repoRoot, 'site/viewer/pkg/wasm_viewer.js');
const pkgWasm = path.join(repoRoot, 'site/viewer/pkg/wasm_viewer_bg.wasm');

if (!fs.existsSync(pkgJs) || !fs.existsSync(pkgWasm)) {
    test('WASM .rez support (bundle not built — skipped)', { skip: true }, () => {});
} else {
    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'rezolus-wasm-rez-'));
    const wasmJsCopy = path.join(tmpDir, 'wasm_viewer.mjs');
    fs.copyFileSync(pkgJs, wasmJsCopy);

    const { initSync, WasmCaptureRegistry } = await import(pathToFileURL(wasmJsCopy).href);
    initSync({ module: fs.readFileSync(pkgWasm) });

    // The archive is built by the format crate itself; a committed binary
    // fixture would go stale against the container it is meant to exercise.
    const fixture = (name, recordings) => {
        const out = path.join(tmpDir, name);
        execFileSync(
            'cargo',
            ['run', '-q', '-p', 'rez', '--features', 'test-support',
             '--example', 'write_rez_fixture', '--', out, String(recordings)],
            { cwd: repoRoot, stdio: ['ignore', 'ignore', 'inherit'] },
        );
        return new Uint8Array(fs.readFileSync(out));
    };

    // The fixture's recordings are of one run (one time range), so they
    // attach as one combined capture unless a comparison is asked for, as
    // `rezolus view --baseline .. --experiment ..` asks for one.
    test('a two-recording .rez of one run is one combined capture', () => {
        const registry = new WasmCaptureRegistry();
        registry.attach('baseline', fixture('two-combined.rez', 2), 'fleet.rez');
        assert.equal(registry.has('baseline'), true);
        assert.equal(registry.has('experiment'), false);
        const info = JSON.parse(registry.info('baseline'));
        const result = JSON.parse(registry.query_range(
            'baseline', 'irate(cpu_cycles[2s])', info.minTime, info.maxTime, 1, 'grid',
        ));
        assert.equal(result.status, 'success', JSON.stringify(result) + JSON.stringify(info));
        const sources = result.data.result.map((s) => s.metric.source).sort();
        assert.deepEqual(sources, ['redis', 'valkey'], JSON.stringify(result));
    });

    test('a two-recording .rez loads as an A/B comparison on request', () => {
        const registry = new WasmCaptureRegistry();
        registry.set_compare_requested(true);
        registry.attach('baseline', fixture('two.rez', 2), 'fleet.rez');

        assert.equal(registry.has('baseline'), true);
        assert.equal(registry.has('experiment'), true, 'both arms of the archive load');

        // Identity, not just "two captures loaded": each recording carries its
        // own `source` in its metadata, written by a different path than the
        // labels the slots are chosen by.
        assert.match(registry.file_metadata_json('baseline'), /redis/);
        assert.match(registry.file_metadata_json('experiment'), /valkey/);

        // And real data behind them — the query engine has to work against
        // segments pulled out of an in-memory SQLite catalog.
        const info = JSON.parse(registry.info('baseline'));
        assert.deepEqual(info.counter_names, ['cpu_cycles']);
        assert.ok(info.maxTime > info.minTime, `empty range: ${JSON.stringify(info)}`);
        assert.equal(JSON.parse(registry.notices()).length, 0);
    });

    test('a 3-recording comparison shows the third beside it, not as an arm', () => {
        const registry = new WasmCaptureRegistry();
        registry.set_compare_requested(true);
        registry.attach('baseline', fixture('three.rez', 3), 'fleet.rez');

        assert.equal(JSON.parse(registry.notices()).length, 0);
        const captures = JSON.parse(registry.captures());
        assert.deepEqual(captures.map((c) => c.id), ['baseline', 'experiment']);
        assert.match(registry.file_metadata_json('baseline'), /redis/);
        assert.match(registry.file_metadata_json('experiment'), /valkey/);
        // The baseline's source answers a query that names the third.
        const info = JSON.parse(registry.info('baseline'));
        const result = JSON.parse(registry.query_range(
            'baseline', 'irate(cpu_cycles{__capture__="web-01"}[2s])',
            info.minTime, info.maxTime, 1, 'grid',
        ));
        assert.equal(result.status, 'success', JSON.stringify(result));
        assert.match(JSON.stringify(result.data.result), /envoy/);
    });

    test('a single-recording .rez is one capture', () => {
        const registry = new WasmCaptureRegistry();
        registry.attach('baseline', fixture('one.rez', 1), 'one.rez');
        assert.equal(registry.has('baseline'), true);
        assert.equal(registry.has('experiment'), false);
    });
}
