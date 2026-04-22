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
        ViewerApi.initTemplates(JSON.stringify(templates));
    }
};

const initWasmViewer = async (data, filename) => {
    const wasmModule = await import('../pkg/wasm_viewer.js');
    await wasmModule.default();
    if (!ViewerApi.registry()) {
        ViewerApi.setRegistry(new wasmModule.WasmCaptureRegistry());
    }
    await ViewerApi.attachBaseline(data, filename);
    await loadTemplates();

    const info = await ViewerApi.getInfo('baseline');
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

// Fetch a demo parquet from `data/<filename>` with optional progress
// reporting. Returns the raw bytes as a Uint8Array.
async function fetchDemoBytes(filename, onProgress) {
    const resp = await fetch('data/' + filename);
    if (!resp.ok) throw new Error(`Failed to fetch ${filename}: ${resp.status}`);

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
            if (typeof onProgress === 'function') onProgress(received / total);
        }
        const data = new Uint8Array(received);
        let pos = 0;
        for (const chunk of chunks) {
            data.set(chunk, pos);
            pos += chunk.length;
        }
        return data;
    }
    return new Uint8Array(await resp.arrayBuffer());
}

async function loadDemo(filename = 'demo.parquet') {
    splashLabel = filename;
    splashProgress = -1;
    landingError = null;
    m.redraw();

    try {
        const data = await fetchDemoBytes(filename, (p) => {
            splashProgress = p;
            m.redraw();
        });

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

// Load a pair of demo parquets as baseline + experiment and launch the
// viewer in compare mode. Triggered by ?demoA=<file>&demoB=<file>.
async function loadCompareDemo(fileA, fileB) {
    splashLabel = `${fileA} vs ${fileB}`;
    splashProgress = -1;
    landingError = null;
    m.redraw();

    try {
        // Fetch both in parallel; progress bar tracks the combined
        // fraction so the user sees one moving indicator.
        let aDone = 0;
        let bDone = 0;
        const [dataA, dataB] = await Promise.all([
            fetchDemoBytes(fileA, (p) => {
                aDone = p;
                splashProgress = (aDone + bDone) / 2;
                m.redraw();
            }),
            fetchDemoBytes(fileB, (p) => {
                bDone = p;
                splashProgress = (aDone + bDone) / 2;
                m.redraw();
            }),
        ]);

        splashLabel = 'Initializing';
        splashProgress = -1;
        m.redraw();

        // Attach baseline first (standard loadParquet path sets up the
        // registry + templates + storage scope), then layer the
        // experiment in via the registry.
        await initWasmViewer(dataA, fileA);
        await ViewerApi.attachExperimentBytes(dataB, fileB);

        // Fetch baseline + experiment state for initDashboard.
        const base = await fetchInitialState();
        let experimentSystemInfo = null;
        let experimentFileMetadata = null;
        let experimentFilename = fileB;
        try { experimentSystemInfo = await ViewerApi.getSystemInfo('experiment'); }
        catch (_) { /* optional */ }
        try { experimentFileMetadata = await ViewerApi.getFileMetadata('experiment'); }
        catch (_) { /* optional */ }
        try {
            const expMeta = await ViewerApi.getMetadata('experiment');
            experimentFilename = expMeta?.data?.filename || fileB;
        } catch (_) { /* optional */ }

        initDashboard({
            systemInfo: base.systemInfo,
            fileMetadata: base.fileMetadata,
            selectionPayload: base.selectionPayload,
            compareMode: true,
            experimentSystemInfo,
            experimentFileMetadata,
            experimentFilename,
        });

        // Keep the URL shape stable so refreshes reload the same pair.
        const url = new URL(window.location);
        url.searchParams.set('demoA', fileA);
        url.searchParams.set('demoB', fileB);
        url.searchParams.delete('demo');
        window.history.replaceState(null, '', url);
    } catch (e) {
        splashLabel = null;
        landingError = `Failed to load compare demos: ${e?.message ?? e ?? 'unknown error'}`;
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

const _params = new URLSearchParams(window.location.search);
const _demoA = _params.get('demoA');
const _demoB = _params.get('demoB');
const _demoParam = _params.get('demo');

if (_demoA && _demoB) {
    // A/B compare demo.
    splashLabel = `${_demoA} vs ${_demoB}`;
    m.mount(document.body, Root);
    loadCompareDemo(_demoA, _demoB);
} else if (_demoParam !== null) {
    splashLabel = _demoParam || 'demo.parquet';
    m.mount(document.body, Root);
    loadDemo(_demoParam || 'demo.parquet');
} else {
    m.mount(document.body, Root);
}
