// The static-site viewer must open a multi-recording archive — a `.dendro`,
// which is what 6.x writes, and a `.rez`, which earlier releases wrote — not
// just a parquet. `rezolus` is a binary-only crate, so nothing could depend on
// its archive reader; it lives in `crates/rez`, which this bundle links.
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

    // The dendro archive is written by the format crate's fixture builder,
    // through metriken-storage's writer, so it tracks the container 6.x
    // writes.
    const fixture = (name, recordings) => {
        const out = path.join(tmpDir, name);
        execFileSync(
            'cargo',
            ['run', '-q', '-p', 'rez', '--features', 'test-support',
             '--example', 'write_fixture', '--', out, String(recordings)],
            { cwd: repoRoot, stdio: ['ignore', 'ignore', 'inherit'] },
        );
        return new Uint8Array(fs.readFileSync(out));
    };
    // Nothing writes a `.rez` any more; this one is committed, written by the
    // last `.rez` writer (see `crates/rez/src/fixtures.rs` `committed_rez`).
    const committedRez = (name) => new Uint8Array(fs.readFileSync(
        path.join(repoRoot, 'crates/rez/tests/fixtures/rez', name)));

    // The fixture's recordings are of one run (one time range), so they
    // attach as one combined capture unless a comparison is asked for, as
    // `rezolus view --baseline .. --experiment ..` asks for one.
    test('a two-recording archive of one run is one combined capture', () => {
        const registry = new WasmCaptureRegistry();
        registry.attach('baseline', fixture('two-combined.dendro', 2), 'fleet.dendro');
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

    test('a two-recording archive loads as an A/B comparison on request', () => {
        const registry = new WasmCaptureRegistry();
        registry.set_compare_requested(true);
        registry.attach('baseline', fixture('two.dendro', 2), 'fleet.dendro');

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
        registry.attach('baseline', fixture('three.dendro', 3), 'fleet.dendro');

        assert.deepEqual(JSON.parse(registry.notices()),
            ['beside the comparison, each in its own sections: web-01']);
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

    test('a single-recording archive is one capture', () => {
        const registry = new WasmCaptureRegistry();
        registry.attach('baseline', fixture('one.dendro', 1), 'one.dendro');
        assert.equal(registry.has('baseline'), true);
        assert.equal(registry.has('experiment'), false);
    });

    // The committed v3 `.rez`: two recordings, host=web-01 and host=web-02,
    // three ticks of the gauge mem_free each, (i + 1) * 1000 + t.
    test('a two-recording .rez of one run is one combined capture', () => {
        const registry = new WasmCaptureRegistry();
        registry.attach('baseline', committedRez('v3-multi.rez'), 'fleet.rez');
        assert.equal(registry.has('baseline'), true);
        assert.equal(registry.has('experiment'), false);
        const info = JSON.parse(registry.info('baseline'));
        const result = JSON.parse(registry.query_range(
            'baseline', 'mem_free', info.minTime, info.maxTime, 1, 'grid',
        ));
        assert.equal(result.status, 'success', JSON.stringify(result) + JSON.stringify(info));
        const hosts = result.data.result.map((s) => s.metric.host).sort();
        assert.deepEqual(hosts, ['web-01', 'web-02'], JSON.stringify(result));
    });

    test('a two-recording .rez loads as an A/B comparison on request', () => {
        const registry = new WasmCaptureRegistry();
        registry.set_compare_requested(true);
        registry.attach('baseline', committedRez('v3-multi.rez'), 'fleet.rez');
        assert.equal(registry.has('baseline'), true);
        assert.equal(registry.has('experiment'), true, 'both arms of the archive load');
        const info = JSON.parse(registry.info('baseline'));
        assert.deepEqual(info.gauge_names, ['mem_free']);
        assert.ok(info.maxTime > info.minTime, `empty range: ${JSON.stringify(info)}`);
    });

    test('a v2 tar .rez is one capture', () => {
        const registry = new WasmCaptureRegistry();
        registry.attach('baseline', committedRez('v2.rez'), 'one.rez');
        assert.equal(registry.has('baseline'), true);
        const info = JSON.parse(registry.info('baseline'));
        assert.deepEqual(info.counter_names, ['ops']);
    });

    // Save as Report writes a .dendro whatever the source, built in the
    // browser: a `.rez` (tar or v3) and a parquet are converted in memory
    // (`ArchiveMut::create_in_memory`), which is what this checks in the
    // shipped WASM.
    const SQLITE_MAGIC = 'SQLite format 3\0';
    const saved = (bytes, name) => {
        const registry = new WasmCaptureRegistry();
        registry.attach('baseline', bytes, name);
        assert.equal(registry.report_extension(), '.dendro');
        return registry.save_with_selection('{"entries":[],"trim_columns":false}');
    };
    const reopened = (report) => {
        assert.equal(Buffer.from(report.slice(0, 16)).toString('latin1'), SQLITE_MAGIC);
        const registry = new WasmCaptureRegistry();
        registry.attach('baseline', report, 'rezolus-report.dendro');
        assert.match(registry.file_metadata_json('baseline'), /selection/);
        return JSON.parse(registry.info('baseline'));
    };

    test('Save as Report of a tar .rez, a parquet or a .dendro is a .dendro', () => {
        assert.deepEqual(reopened(saved(committedRez('v2.rez'), 'one.rez')).counter_names, ['ops']);
        const parquet = new Uint8Array(fs.readFileSync(
            path.join(repoRoot, 'site/viewer/data/simple_capture.parquet')));
        assert.ok(reopened(saved(parquet, 'simple.parquet')).gauge_names.includes('queue_depth'));
        assert.deepEqual(
            reopened(saved(fixture('report-src.dendro', 1), 'one.dendro')).counter_names,
            ['cpu_cycles'],
        );
    });

    test('Save as Report of a v3 .rez is a .dendro', () => {
        assert.deepEqual(reopened(saved(committedRez('v3.rez'), 'one.rez')).counter_names, ['ops']);
        // Two recordings: the report holds both, the selection on the first.
        const registry = new WasmCaptureRegistry();
        registry.attach('baseline', saved(committedRez('v3-multi.rez'), 'multi.rez'), 'r.dendro');
        assert.match(registry.file_metadata_json('baseline'), /selection/);
        // The live archive's rows still in its WAL table come across.
        assert.deepEqual(reopened(saved(committedRez('v3-live.rez'), 'live.rez')).counter_names, ['ops']);
    });
}
