// Parity guard: the WASM viewer must classify a non-Rezolus "simple capture"
// parquet the same way the axum server does — surfacing a `source:` nav entry
// and suppressing the empty Rezolus built-in sections. Both backends share
// `dashboard::source_kind::classify_sources`; this test pins the WASM side so a
// future change can't silently regress it back to "built-ins only" (Bug 2).
//
// Requires the WASM bundle to be built first: `./crates/viewer/build.sh`.
// The server side is covered by the `simple-capture` block in
// tests/viewer_smoke.sh.
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
    test('WASM simple-capture parity (bundle not built — skipped)', { skip: true }, () => {});
} else {
    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'rezolus-wasm-simple-'));
    const wasmJsCopy = path.join(tmpDir, 'wasm_viewer.mjs');
    fs.copyFileSync(pkgJs, wasmJsCopy);

    const { initSync, WasmCaptureRegistry } = await import(pathToFileURL(wasmJsCopy).href);
    initSync({ module: fs.readFileSync(pkgWasm) });

    const parquet = fs.readFileSync(path.join(repoRoot, 'site/viewer/data/simple_capture.parquet'));

    const sectionsFor = (setup) => {
        const registry = new WasmCaptureRegistry();
        registry.attach('baseline', new Uint8Array(parquet), 'simple_capture.parquet');
        setup(registry);
        const json = registry.get_sections('baseline');
        return JSON.parse(json);
    };

    const assertSimpleCaptureShape = (sections, label) => {
        const routes = sections.map((s) => s.route);
        assert.ok(
            routes.some((r) => r.startsWith('/source/')),
            `${label}: expected a source: nav entry, got ${JSON.stringify(routes)}`,
        );
        assert.ok(
            !routes.includes('/cpu'),
            `${label}: Rezolus built-in sections must be suppressed for a simple capture, got ${JSON.stringify(routes)}`,
        );
    };

    // Path 1: no templates loaded (Viewer::new classification) — this is the
    // flow when the static site ships no service templates.
    test('WASM simple-capture shows a source: section without templates', () => {
        assertSimpleCaptureShape(sectionsFor(() => {}), 'no-templates');
    });

    // Path 2: templates loaded but none match (init_templates classification) —
    // the deployed static site always ships templates, none of which bind to a
    // foreign source, so service_exts is empty and the source: entry must remain.
    test('WASM simple-capture shows a source: section after init_templates', () => {
        assertSimpleCaptureShape(
            sectionsFor((registry) => registry.init_templates('baseline', '[]')),
            'empty-templates',
        );
    });
}
