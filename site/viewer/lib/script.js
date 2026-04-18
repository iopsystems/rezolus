// script.js — WASM viewer bootstrap stub.
// Handles parquet loading (demo or upload), WASM init, and template loading.
// Delegates all UI/routing to app.js via initDashboard().

import { ViewerApi } from './viewer_api.js';
import { FileUpload } from './landing.js';
import { setStorageScope } from './selection.js';
import { initDashboard } from './app.js';
import { initTheme } from './theme.js';

initTheme();

// ── WASM + template initialization ─────────────────────────────────

const loadTemplates = async () => {
    const templateNames = ['cachecannon', 'llm-perf', 'sglang', 'valkey', 'vllm'];
    const templates = [];
    for (const name of templateNames) {
        try {
            const resp = await fetch(`templates/${name}.json`);
            if (resp.ok) templates.push(await resp.json());
        } catch (e) { /* template not available, skip */ }
    }
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
    let systemInfo = null;
    let fileMetadata = null;
    let selectionPayload = null;

    try { systemInfo = await ViewerApi.getSystemInfo(); } catch { /* ignore */ }
    try { fileMetadata = await ViewerApi.getFileMetadata(); } catch { /* ignore */ }
    try { selectionPayload = await ViewerApi.getSelection(); } catch { /* ignore */ }

    return { systemInfo, fileMetadata, selectionPayload };
};

// ── Load demo parquet ──────────────────────────────────────────────

async function loadDemo(filename = 'demo.parquet') {
    window._loading = true;
    window._loadError = null;
    m.redraw();

    try {
        const resp = await fetch('data/' + filename);
        if (!resp.ok) throw new Error(`Failed to fetch ${filename}: ${resp.status}`);
        const arrayBuffer = await resp.arrayBuffer();
        const data = new Uint8Array(arrayBuffer);

        await initWasmViewer(data, filename);
        const state = await fetchInitialState();

        window._loading = false;

        // Ensure ?demo is in the URL so bookmarks/refreshes auto-load the demo
        const url = new URL(window.location);
        const currentDemo = new URLSearchParams(window.location.search).get('demo');
        if (currentDemo !== filename) {
            url.searchParams.set('demo', filename);
            window.history.replaceState(null, '', url);
        }

        initDashboard({
            systemInfo: state.systemInfo,
            fileMetadata: state.fileMetadata,
            selectionPayload: state.selectionPayload,
        });
    } catch (e) {
        window._loading = false;
        window._loadError = `Failed to load demo: ${e.message || e}`;
        m.redraw();
    }
}

// ── Load uploaded file ─────────────────────────────────────────────

async function loadFile(file) {
    window._loading = true;
    window._loadError = null;
    m.redraw();

    try {
        const arrayBuffer = await file.arrayBuffer();
        const data = new Uint8Array(arrayBuffer);

        await initWasmViewer(data, file.name);
        const state = await fetchInitialState();

        window._loading = false;

        initDashboard({
            systemInfo: state.systemInfo,
            fileMetadata: state.fileMetadata,
            selectionPayload: state.selectionPayload,
        });
    } catch (e) {
        window._loading = false;
        window._loadError = `Failed to load file: ${e.message || e}`;
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
            loading: window._loading,
            error: window._loadError,
        }),
    });
}
