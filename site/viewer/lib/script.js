// script.js — WASM viewer bootstrap stub.
// Handles demo parquet loading, WASM init, and template loading.
// Delegates all UI/routing to app.js via initDashboard().

import { ViewerApi } from './viewer_api.js';
import { FileUpload } from './landing.js';
import { setStorageScope } from './selection.js';
import { initDashboard } from './app.js';

// ── UI state ────────────────────────────────────────────────────────

let splashLabel = null;   // non-null = show splash, null = show landing
let splashProgress = -1;  // -1 = indeterminate, 0–1 = determinate
let landingError = null;

const demos = [
    { label: 'vLLM + System', file: 'vllm.parquet' },
    { label: 'Cachecannon + System', file: 'cachecannon.parquet' },
    { label: 'System Metrics', file: 'demo.parquet' },
];

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

// ── Load demo parquet (with download progress) ──────────────────────

async function loadDemo(filename = 'demo.parquet') {
    splashLabel = filename;
    splashProgress = -1;
    landingError = null;
    m.redraw();

    try {
        const resp = await fetch('data/' + filename);
        if (!resp.ok) throw new Error(`Failed to fetch ${filename}: ${resp.status}`);

        let data;
        const contentLength = resp.headers.get('Content-Length');

        if (contentLength && resp.body) {
            const total = parseInt(contentLength, 10);
            const reader = resp.body.getReader();
            const chunks = [];
            let received = 0;

            for (;;) {
                const { done, value } = await reader.read();
                if (done) break;
                chunks.push(value);
                received += value.length;
                splashProgress = received / total;
                m.redraw();
            }

            data = new Uint8Array(received);
            let pos = 0;
            for (const chunk of chunks) {
                data.set(chunk, pos);
                pos += chunk.length;
            }
        } else {
            data = new Uint8Array(await resp.arrayBuffer());
        }

        // WASM init phase — indeterminate
        splashLabel = 'Initializing';
        splashProgress = -1;
        m.redraw();

        await loadParquet(data, filename);

        // Ensure ?demo is in the URL so bookmarks/refreshes auto-load the demo
        const url = new URL(window.location);
        const currentDemo = new URLSearchParams(window.location.search).get('demo');
        if (currentDemo !== filename) {
            url.searchParams.set('demo', filename);
            window.history.replaceState(null, '', url);
        }
    } catch (e) {
        splashLabel = null;
        landingError = `Failed to load demo: ${e?.message ?? e ?? 'unknown error'}`;
        m.redraw();
    }
}

// ── Root component — switches between splash and landing ────────────

const Root = {
    view: () => {
        if (splashLabel) {
            return m('div#splash', m('div.card', [
                m('h1', 'Rezolus'),
                m('p.subtitle', `Loading ${splashLabel}…`),
                m('div.progress-bar',
                    m('div.progress-fill', splashProgress < 0
                        ? { class: 'indeterminate' }
                        : { style: { width: `${Math.round(splashProgress * 100)}%` } }
                    ),
                ),
            ]));
        }
        return m('div', m(FileUpload, {
            onDemo: loadDemo,
            demos,
            loading: false,
            error: landingError,
        }));
    },
};

// ── Initial mount ──────────────────────────────────────────────────

const _demoParam = new URLSearchParams(window.location.search).get('demo');
if (_demoParam !== null) {
    splashLabel = _demoParam || 'demo.parquet';
    m.mount(document.body, Root);
    loadDemo(_demoParam || 'demo.parquet');
} else {
    m.mount(document.body, Root);
}
