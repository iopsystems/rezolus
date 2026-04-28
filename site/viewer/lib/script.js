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

// Split a "alias=path" string into [alias, path]. Mirrors the Rust
// split_alias in src/viewer/mod.rs: alias must be non-empty, free of
// path separators, ':' (URL-scheme guard), and whitespace. Anything
// else parses as a bare path with alias=null.
const splitAlias = (raw) => {
    const eq = raw.indexOf('=');
    if (eq <= 0) return [null, raw];
    const lhs = raw.slice(0, eq);
    const rhs = raw.slice(eq + 1);
    if (/[\/\\:\s]/.test(lhs)) return [null, raw];
    return [lhs, rhs];
};

const demoSections = [
    {
        label: 'Host (System) Metrics',
        demos: [
            { label: 'Host-1', file: 'demo.parquet' },
        ],
    },
    {
        label: 'Host and Client Metrics',
        demos: [
            { label: 'Cache (Valkey)', file: 'cachecannon.parquet' },
            { label: 'Inference (vLLM)', file: 'vllm.parquet' },
        ],
    },
    {
        label: 'A/B Testing',
        demos: [
            { label: 'Cache (Default vs Pinned Interrupts)', files: ['AB_base.parquet', 'AB_base_pin.parquet'] },
            {
                label: 'Inference (vLLM vs SGLang)',
                files: ['vllm_gemma3.parquet', 'sglang_gemma3.parquet'],
                legends: { baseline: 'vLLM', experiment: 'SGLang' },
                category: 'inference-library',
            },
        ],
    },
];

// ── WASM + template initialization ─────────────────────────────────

// Stash the raw templates JSON on first load so the compare-mode flow
// can re-init for the experiment slot and trigger combined regen
// without re-fetching from disk.
let loadedTemplatesJson = null;

const loadTemplates = async () => {
    const templateNames = ['cachecannon', 'inference-library', 'llm-perf', 'sglang', 'valkey', 'vllm'];
    const results = await Promise.allSettled(
        templateNames.map(name => fetch(`templates/${name}.json`).then(r => r.ok ? r.json() : null))
    );
    const templates = results
        .filter(r => r.status === 'fulfilled' && r.value)
        .map(r => r.value);
    if (templates.length > 0) {
        loadedTemplatesJson = JSON.stringify(templates);
        ViewerApi.initTemplates(loadedTemplatesJson);
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
async function loadCompareDemo(fileA, fileB, legends = null, category = null) {
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

        // Templates were initialized for baseline inside initWasmViewer
        // via loadTemplates (which stashes the JSON in loadedTemplatesJson).
        // Now also init for the experiment slot, then trigger the
        // combined-regen pass that picks up a matching category and
        // rewrites baseline's section list.
        if (typeof loadedTemplatesJson === 'string' && loadedTemplatesJson.length > 0) {
            ViewerApi.initTemplates(loadedTemplatesJson, 'experiment');
            ViewerApi.regenerateCombined(loadedTemplatesJson, category);
        }

        // Fetch baseline + experiment state for initDashboard.
        const base = await fetchInitialState();
        const [experimentSystemInfo, experimentFileMetadata, expMeta] = await Promise.all([
            ViewerApi.getSystemInfo('experiment').catch(() => null),
            ViewerApi.getFileMetadata('experiment').catch(() => null),
            ViewerApi.getMetadata('experiment').catch(() => null),
        ]);
        const experimentFilename = expMeta?.data?.filename || fileB;
        let experimentQueryRange = null;
        const data = expMeta?.data ?? expMeta;
        const minT = data?.minTime ?? data?.min_time ?? data?.start_time;
        const maxT = data?.maxTime ?? data?.max_time ?? data?.end_time;
        if (minT != null && maxT != null) {
            const start = Number(minT);
            const end = Number(maxT);
            if (Number.isFinite(start) && Number.isFinite(end) && end > start) {
                experimentQueryRange = { start, end, step: Math.max(1, Math.floor((end - start) / 500)) };
            }
        }

        initDashboard({
            systemInfo: base.systemInfo,
            fileMetadata: base.fileMetadata,
            selectionPayload: base.selectionPayload,
            compareMode: true,
            categoryName: category || null,
            baselineAlias: legends?.baseline || null,
            experimentSystemInfo,
            experimentAlias: legends?.experiment || null,
            experimentFileMetadata,
            experimentFilename,
            experimentQueryRange,
        });

        // Canonicalize the URL: repeated `capture=label=path` (or bare
        // `capture=path` when no label). Order encodes role — first is
        // baseline, second is experiment. Mirrors the CLI positional
        // shape and scales naturally to N captures in the future. The
        // label is purely the legend used for display; template
        // selection and category membership are derived from each
        // parquet's source metadata.
        const url = new URL(window.location);
        const encode = (file, label) => label ? `${label}=${file}` : file;
        url.searchParams.delete('demo');
        url.searchParams.delete('demoA');
        url.searchParams.delete('demoB');
        url.searchParams.delete('capture');
        url.searchParams.delete('category');
        url.searchParams.append('capture', encode(fileA, legends?.baseline));
        url.searchParams.append('capture', encode(fileB, legends?.experiment));
        if (category) url.searchParams.set('category', category);
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
            onDemo: (demo) => {
                if (demo && Array.isArray(demo.files) && demo.files.length === 2) {
                    loadCompareDemo(
                        demo.files[0],
                        demo.files[1],
                        demo.legends || null,
                        demo.category || null,
                    );
                } else if (demo && demo.file) {
                    loadDemo(demo.file);
                }
            },
            demoSections,
            loading: false,
            error: landingError,
        }));
    },
};

// ── Initial mount ──────────────────────────────────────────────────

// Canonical compare URL: `?capture=label=path&capture=label=path&category=name`
// (each `label=` prefix optional, category optional). The label is
// purely the legend used for display; template selection and category
// membership come from each parquet's source metadata. Order encodes
// role: 1st = baseline, 2nd = experiment. Legacy: `?demoA=…&demoB=…`
// is parsed as a fallback for one release; on first load we rewrite to
// canonical shape.
const _params = new URLSearchParams(window.location.search);
const _captureRaw = _params.getAll('capture');
const _demoA = _params.get('demoA');
const _demoB = _params.get('demoB');
const _demoParam = _params.get('demo');
const _category = _params.get('category');

const parsePair = (rawA, rawB) => {
    const [labelA, fileA] = splitAlias(rawA);
    const [labelB, fileB] = splitAlias(rawB);
    const legends = (labelA || labelB)
        ? { baseline: labelA, experiment: labelB }
        : null;
    return { fileA, fileB, legends };
};

if (_captureRaw.length >= 2) {
    const { fileA, fileB, legends } = parsePair(_captureRaw[0], _captureRaw[1]);
    splashLabel = `${legends?.baseline || fileA} vs ${legends?.experiment || fileB}`;
    m.mount(document.body, Root);
    loadCompareDemo(fileA, fileB, legends, _category);
} else if (_demoA && _demoB) {
    // Legacy A/B compare URL — parsed for one release, rewritten to
    // canonical `?capture=…&capture=…` on load by loadCompareDemo.
    const { fileA, fileB, legends } = parsePair(_demoA, _demoB);
    splashLabel = `${legends?.baseline || fileA} vs ${legends?.experiment || fileB}`;
    m.mount(document.body, Root);
    loadCompareDemo(fileA, fileB, legends, _category);
} else if (_demoParam !== null) {
    splashLabel = _demoParam || 'demo.parquet';
    m.mount(document.body, Root);
    loadDemo(_demoParam || 'demo.parquet');
} else {
    m.mount(document.body, Root);
}
