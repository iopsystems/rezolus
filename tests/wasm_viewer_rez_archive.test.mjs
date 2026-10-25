// The static-site viewer opens `.dendro` archives, single- and
// multi-recording, and refuses a `.rez` or parquet file with the command that
// converts it on the user's machine. `rezolus` is a binary-only crate, so
// nothing could depend on its archive reader; it lives in `crates/rez`, which
// this bundle links.
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
    test('WASM archive support (bundle not built — skipped)', { skip: true }, () => {});
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
    // Nothing writes a `.rez` any more; these are committed, written by the
    // last `.rez` writer (see `crates/rez/src/fixtures.rs` `committed_rez`).
    const committedRez = (name) => new Uint8Array(fs.readFileSync(
        path.join(repoRoot, 'crates/rez/tests/fixtures/rez', name)));
    // The same files after `rezolus recording upgrade`, committed in
    // `tests/fixtures/upgraded` (see its README).
    const upgraded = (name) => new Uint8Array(fs.readFileSync(
        path.join(repoRoot, 'tests/fixtures/upgraded', name)));

    test('a .rez or parquet upload is refused with the upgrade command', () => {
        const parquet = new Uint8Array(fs.readFileSync(
            path.join(repoRoot, 'tests/fixtures/legacy/simple_capture.parquet')));
        for (const [bytes, name] of [
            [committedRez('v3.rez'), 'old.rez'],
            [committedRez('v2.rez'), 'tar.rez'],
            [parquet, 'run.parquet'],
        ]) {
            const registry = new WasmCaptureRegistry();
            assert.throws(
                () => registry.attach('baseline', bytes, name),
                (e) => String(e).includes(`rezolus recording upgrade ${name}`),
                name,
            );
            assert.equal(registry.has('baseline'), false, name);
        }
    });

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

    // An experiment is one recording. Beside a one-recording baseline or a
    // comparison it fills the experiment slot; beside a combined view of
    // several recordings it is refused and the view is unchanged.
    test('an experiment is one recording, refused beside a combined view', () => {
        const one = fixture('exp-one.dendro', 1);
        const two = fixture('exp-two.dendro', 2);

        const single = new WasmCaptureRegistry();
        single.attach('baseline', fixture('base-one.dendro', 1), 'base.dendro');
        single.attach('experiment', one, 'exp.dendro');
        assert.equal(single.has('experiment'), true);
        assert.throws(
            () => single.attach('experiment', two, 'exp2.dendro'),
            (e) => String(e).startsWith('exp2.dendro holds 2 recordings; the experiment')
                && String(e).includes('rezolus recording combine base.dendro exp2.dendro -o both.dendro'),
        );

        const combined = new WasmCaptureRegistry();
        combined.attach('baseline', two, 'fleet.dendro');
        assert.throws(
            () => combined.attach('experiment', one, 'exp.dendro'),
            (e) => String(e).startsWith('fleet.dendro holds 2 recordings; the baseline')
                && String(e).includes('rezolus recording combine fleet.dendro exp.dendro -o both.dendro'),
        );
        assert.equal(combined.has('experiment'), false);
        // A one-recording baseline replacing it takes an experiment again.
        combined.attach('baseline', fixture('base-one.dendro', 1), 'base.dendro');
        combined.attach('experiment', one, 'exp.dendro');
        assert.equal(combined.has('experiment'), true);

        // The file replaces a comparison's experiment arm (valkey).
        const ab = new WasmCaptureRegistry();
        ab.set_compare_requested(true);
        ab.attach('baseline', two, 'ab.dendro');
        assert.match(ab.file_metadata_json('experiment'), /valkey/);
        ab.attach('experiment', one, 'exp.dendro');
        assert.doesNotMatch(ab.file_metadata_json('experiment'), /valkey/);
        assert.match(ab.file_metadata_json('baseline'), /redis/);
    });

    // An A/B archive (`recording combine --ab`) of two one-recording files.
    const abArchive = () => {
        const a = path.join(tmpDir, 'arm-a.dendro');
        const b = path.join(tmpDir, 'arm-b.dendro');
        const out = path.join(tmpDir, 'arms.dendro');
        fs.writeFileSync(a, fixture('arm-a.dendro', 1));
        fs.writeFileSync(b, fixture('arm-b.dendro', 1));
        fs.rmSync(out, { force: true });
        execFileSync(
            'cargo',
            ['run', '-q', '--bin', 'rezolus', '--', 'recording', 'combine', a, b,
             '--ab', `baseline=${a}`, `experiment=${b}`, '-o', out],
            { cwd: repoRoot, stdio: ['ignore', 'ignore', 'inherit'] },
        );
        return new Uint8Array(fs.readFileSync(out));
    };

    // Beside an A/B archive an experiment replaces its arm, after which no
    // report is saved (a report copies whole files); the refusals of
    // several recordings leave out the `--ab` step, which cannot relabel an
    // arm.
    test('an experiment beside an A/B archive, and A/B archives refused', () => {
        const ab = abArchive();
        const one = fixture('exp-one.dendro', 1);

        const registry = new WasmCaptureRegistry();
        registry.attach('baseline', ab, 'ab.dendro');
        assert.equal(registry.has('experiment'), true);
        assert.ok(registry.save_with_selection('{"entries":[]}').length > 0);
        assert.throws(
            () => registry.attach('experiment', fixture('exp-two.dendro', 2), 'exp2.dendro'),
            (e) => String(e).startsWith('exp2.dendro holds 2 recordings; the experiment')
                && String(e).includes('carry `arm` labels')
                && !String(e).includes('--ab baseline='),
        );
        registry.attach('experiment', one, 'exp.dendro');
        assert.throws(
            () => registry.save_with_selection('{"entries":[]}'),
            (e) => String(e).includes(
                'Save as Report cannot save this comparison: ab.dendro holds several recordings'),
        );
        // A new baseline clears it.
        registry.attach('baseline', fixture('base-one.dendro', 1), 'base.dendro');
        assert.ok(registry.save_with_selection('{"entries":[]}').length > 0);

        const single = new WasmCaptureRegistry();
        single.attach('baseline', fixture('base-one.dendro', 1), 'base.dendro');
        assert.throws(
            () => single.attach('experiment', ab, 'ab.dendro'),
            (e) => String(e).startsWith('ab.dendro holds 2 recordings; the experiment')
                && String(e).includes('carry `arm` labels'),
        );
    });

    // The committed v3 `.rez`, upgraded: two recordings, host=web-01 and
    // host=web-02, three ticks of the gauge mem_free each, (i + 1) * 1000 + t.
    test('an upgraded two-recording .rez of one run is one combined capture', () => {
        const registry = new WasmCaptureRegistry();
        registry.attach('baseline', upgraded('v3-multi.dendro'), 'fleet.dendro');
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

    test('an upgraded two-recording .rez loads as an A/B comparison on request', () => {
        const registry = new WasmCaptureRegistry();
        registry.set_compare_requested(true);
        registry.attach('baseline', upgraded('v3-multi.dendro'), 'fleet.dendro');
        assert.equal(registry.has('baseline'), true);
        assert.equal(registry.has('experiment'), true, 'both arms of the archive load');
        const info = JSON.parse(registry.info('baseline'));
        assert.deepEqual(info.gauge_names, ['mem_free']);
        assert.ok(info.maxTime > info.minTime, `empty range: ${JSON.stringify(info)}`);
    });

    test('an upgraded v2 tar .rez is one capture', () => {
        const registry = new WasmCaptureRegistry();
        registry.attach('baseline', upgraded('v2.dendro'), 'one.dendro');
        assert.equal(registry.has('baseline'), true);
        const info = JSON.parse(registry.info('baseline'));
        assert.deepEqual(info.counter_names, ['ops']);
    });

    // Save as Report writes a .dendro, built in the browser
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

    test('Save as Report of an archive is a .dendro', () => {
        assert.deepEqual(reopened(saved(upgraded('v2.dendro'), 'one.dendro')).counter_names, ['ops']);
        const simple = upgraded('simple_capture.dendro');
        assert.ok(reopened(saved(simple, 'simple.dendro')).gauge_names.includes('queue_depth'));
        assert.deepEqual(
            reopened(saved(fixture('report-src.dendro', 1), 'one.dendro')).counter_names,
            ['cpu_cycles'],
        );
    });

    test('Save as Report of an upgraded v3 .rez is a .dendro', () => {
        assert.deepEqual(reopened(saved(upgraded('v3.dendro'), 'one.dendro')).counter_names, ['ops']);
        // Two recordings: the report holds both, the selection on the first.
        const registry = new WasmCaptureRegistry();
        registry.attach('baseline', saved(upgraded('v3-multi.dendro'), 'multi.dendro'), 'r.dendro');
        assert.match(registry.file_metadata_json('baseline'), /selection/);
        // Every row the upgraded archive reads comes across: the live
        // archive's segments and WAL rows, and WAL rows alone
        // (`v3-wal-only.rez`, the live archive with its segments deleted).
        const rows = (bytes, name) => {
            const registry = new WasmCaptureRegistry();
            registry.attach('baseline', bytes, name);
            const info = JSON.parse(registry.info('baseline'));
            const result = JSON.parse(registry.query_range(
                'baseline', 'irate(ops[5s])', info.minTime, info.maxTime, 1, 'grid',
            ));
            return result.data.result.reduce((n, s) => n + s.values.length, 0);
        };
        for (const name of ['v3-live.dendro', 'v3-wal-only.dendro']) {
            const bytes = upgraded(name);
            const want = rows(bytes, name);
            assert.ok(want > 0, `${name}: the fixture holds rows`);
            assert.equal(rows(saved(bytes, name), 'r.dendro'), want, name);
        }
    });
}
