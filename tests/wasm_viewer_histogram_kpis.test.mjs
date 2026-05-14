import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath, pathToFileURL } from 'node:url';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'rezolus-wasm-viewer-'));
const wasmJsCopy = path.join(tmpDir, 'wasm_viewer.mjs');

fs.copyFileSync(path.join(repoRoot, 'site/viewer/pkg/wasm_viewer.js'), wasmJsCopy);

const { initSync, WasmCaptureRegistry } = await import(pathToFileURL(wasmJsCopy).href);

initSync({
    module: fs.readFileSync(path.join(repoRoot, 'site/viewer/pkg/wasm_viewer_bg.wasm')),
});

test('static WASM viewer keeps vLLM latency histograms available', () => {
    const registry = new WasmCaptureRegistry();
    const parquet = fs.readFileSync(path.join(repoRoot, 'site/viewer/data/vllm.parquet'));
    const template = JSON.parse(
        fs.readFileSync(path.join(repoRoot, 'config/templates/vllm.json'), 'utf8'),
    );

    registry.attach('baseline', new Uint8Array(parquet), 'vllm.parquet');
    registry.init_templates('baseline', JSON.stringify([template]));

    const sectionJson = registry.get_section('baseline', 'service/vllm');
    assert.ok(sectionJson, 'expected /service/vllm section');

    const section = JSON.parse(sectionJson);
    const unavailableTitles = new Set(
        (section?.metadata?.unavailable_kpis || []).map((kpi) => kpi.title),
    );

    for (const title of [
        'Time to First Token (TTFT)',
        'Inter-Token Latency (ITL)',
        'Prefill Time',
        'End-to-End Request Latency',
    ]) {
        assert.ok(
            !unavailableTitles.has(title),
            `expected ${title} to stay available in the static WASM viewer`,
        );
    }
});
