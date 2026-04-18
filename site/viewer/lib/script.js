// script.js — WASM viewer bootstrap stub.
// Handles parquet loading (demo or upload), WASM init, and template loading.
// Delegates all UI/routing to app.js via initDashboard().

import { ViewerApi } from './viewer_api.js';
import { FileUpload } from './landing.js';
import { setStorageScope } from './selection.js';
import { initDashboard } from './app.js';

// ── Module-level state ──────────────────────────────────────────────

let loading = false;
let loadError = null;

// ── WASM + template initialization ─────────────────────────────────

const loadTemplates = async () => {
    const templateNames = ['cachecannon', 'llm-perf', 'sglang', 'valkey', 'vllm'];
    const results = await Promise.allSettled(
        templateNames.map(name => fetch(`templates/${name}.json`).then(r => r.ok ? r.json() : null))
    );
    const templates = results
        .filter(r => r.status === 'fulfilled' && r.value)
        .map(r => r.value);
    if (templates.length > 0) {
        window.viewer.init_templates(JSON.stringify(templates));
    }
};

const initWasmViewer = async (data, filename) => {
    const wasmModule = await import('../pkg/wasm_viewer.js');
    await wasmModule.default();
    window.viewer = new wasmModule.Viewer(data, filename);
    ViewerApi.setViewer(window.viewer);
    await loadTemplates();

    const info = JSON.parse(window.viewer.info());
    setStorageScope({
        filename: info.filename,
        minTime: info.minTime,
        maxTime: info.maxTime,
        numSeries: (info.counter_names?.length || 0) +
                   (info.gauge_names?.length || 0) +
                   (info.histogram_names?.length || 0),
    });
};

const fetchInitialState = async () => {
    const [sysResult, fmResult, selResult] = await Promise.allSettled([
        ViewerApi.getSystemInfo(),
        ViewerApi.getFileMetadata(),
        ViewerApi.getSelection(),
    ]);
    return {
        systemInfo: sysResult.status === 'fulfilled' ? sysResult.value : null,
        fileMetadata: fmResult.status === 'fulfilled' ? fmResult.value : null,
        selectionPayload: selResult.status === 'fulfilled' ? selResult.value : null,
    };
};

// ── Common loader ───────────────────────────────────────────────────

async function loadParquet(data, filename) {
    await initWasmViewer(data, filename);
    const state = await fetchInitialState();
    initDashboard({
        systemInfo: state.systemInfo,
        fileMetadata: state.fileMetadata,
        selectionPayload: state.selectionPayload,
    });
}

// ── Load demo parquet ──────────────────────────────────────────────

async function loadDemo(filename = 'demo.parquet') {
    loading = true;
    loadError = null;
    m.redraw();

    try {
        const resp = await fetch('data/' + filename);
        if (!resp.ok) throw new Error(`Failed to fetch ${filename}: ${resp.status}`);
        const data = new Uint8Array(await resp.arrayBuffer());

        await loadParquet(data, filename);
        loading = false;

        // Ensure ?demo is in the URL so bookmarks/refreshes auto-load the demo
        const url = new URL(window.location);
        const currentDemo = new URLSearchParams(window.location.search).get('demo');
        if (currentDemo !== filename) {
            url.searchParams.set('demo', filename);
            window.history.replaceState(null, '', url);
        }
    } catch (e) {
        loading = false;
        loadError = `Failed to load demo: ${e.message || e}`;
        m.redraw();
    }
}

// ── Load uploaded file ─────────────────────────────────────────────

async function loadFile(file) {
    loading = true;
    loadError = null;
    m.redraw();

    try {
        const data = new Uint8Array(await file.arrayBuffer());
        await loadParquet(data, file.name);
        loading = false;
    } catch (e) {
        loading = false;
        loadError = `Failed to load file: ${e.message || e}`;
        m.redraw();
    }
}

// ── Initial mount ──────────────────────────────────────────────────

const _demoParam = new URLSearchParams(window.location.search).get('demo');
if (_demoParam !== null) {
    loadDemo(_demoParam || 'demo.parquet');
} else {
    m.mount(document.body, {
        view: () => m(FileUpload, {
            onFile: loadFile,
            onDemo: loadDemo,
            demos: [
                { label: 'vLLM + System', file: 'vllm.parquet' },
                { label: 'Cachecannon + System', file: 'cachecannon.parquet' },
                { label: 'System Metrics', file: 'demo.parquet' },
            ],
            loading,
            error: loadError,
        }),
    });
}
