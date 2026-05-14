// script.js — WASM viewer bootstrap stub.
// Handles demo parquet loading, WASM init, and template loading.
// Delegates all UI/routing to app.js via initDashboard().

import { ViewerApi } from './viewer_api.js';
import { FileUpload, splitAlias } from './landing.js';
import { setStorageScope } from './selection.js';
import { initDashboard, bootstrapSharedSections } from './app.js';

// ── UI state ────────────────────────────────────────────────────────

let splashLabel = null;   // non-null = show splash, null = show landing
let splashProgress = -1;  // -1 = indeterminate, 0–1 = determinate
let landingError = null;

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
    // Source of truth is `templates/manifest.json`, regenerated from
    // `config/templates/*.json` by `crates/viewer/build.sh` and the
    // pages-deploy workflow. Adding/removing a template doesn't require
    // editing this file.
    let templateNames = [];
    try {
        const manifest = await fetch('templates/manifest.json').then(r => r.ok ? r.json() : null);
        if (Array.isArray(manifest)) templateNames = manifest;
    } catch (_) { /* fall through to empty registry */ }

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
    // Boot the duckdb-wasm-backed registry. Mirrors the legacy
    // `WasmCaptureRegistry` surface; data layer is unchanged elsewhere.
    if (!ViewerApi.registry()) {
        const { CaptureRegistry } = await import('../../viewer-sql/lib/duckdb-registry.js');
        ViewerApi.setRegistry(new CaptureRegistry({ workersPerCapture: 4 }));
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
    try {
        const sections = await ViewerApi.getSections();
        bootstrapSharedSections(Array.isArray(sections) ? sections : (sections?.data?.sections || []));
    } catch (_) {
        bootstrapSharedSections([]);
    }
    // Match the server viewer's mode.report flag — the WASM crate stamps
    // the section list to [] when KEY_REPORT="trimmed", and app.js
    // reroutes to /report when reportMode is true.
    const reportMode = state.fileMetadata?.report === 'trimmed';
    initDashboard({
        systemInfo: state.systemInfo,
        fileMetadata: state.fileMetadata,
        selectionPayload: state.selectionPayload,
        reportMode,
        // Wire the topnav "Load Parquet" button to the same handler the
        // landing-page dropzone uses. The server viewer uploads via
        // /api/v1/upload; here we take the File bytes and reuse
        // loadParquet directly.
        onUploadParquet: loadFile,
    });
}

// Shared file-upload entry point: reads the File's bytes and runs the
// usual loadParquet flow. Used by both the landing-page Choose-File /
// drop zone and the topnav "Load Parquet" button after a capture is
// already loaded (which destroys the splash and re-enters loadParquet
// with the new bytes — same lifecycle as `loadCapture`).
async function loadFile(file) {
    if (!file) return;
    const display = file.name || 'capture.parquet';
    splashLabel = display;
    splashProgress = -1;
    landingError = null;
    m.redraw();
    try {
        const data = new Uint8Array(await file.arrayBuffer());
        splashLabel = 'Initializing';
        splashProgress = -1;
        m.redraw();
        await loadParquet(data, display);
        // Drop any URL params that pinned the previous capture so a
        // refresh doesn't fight the just-uploaded file.
        const url = new URL(window.location);
        url.searchParams.delete('demo');
        url.searchParams.delete('demoA');
        url.searchParams.delete('demoB');
        url.searchParams.delete('capture');
        window.history.replaceState(null, '', url);
    } catch (e) {
        splashLabel = null;
        landingError = `Failed to load ${display}: ${e?.message ?? e ?? 'unknown error'}`;
        m.redraw();
    }
}

// ── Load demo parquet (with download progress) ──────────────────────

// Fetch a parquet by URL or relative path. Relative paths resolve under
// `data/` (the static-site convention for bundled demos); anything
// matching `^https?://` is fetched as-is. Returns the raw bytes as a
// Uint8Array.
async function fetchParquetBytes(source, onProgress) {
    const url = /^https?:\/\//i.test(source) ? source : 'data/' + source;
    const resp = await fetch(url);
    if (!resp.ok) throw new Error(`Failed to fetch ${source}: ${resp.status}`);

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
        const data = await fetchParquetBytes(filename, (p) => {
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

// Filename surfaced in the splash + URL bar. URL paths get their basename
// (or the explicit alias); relative-path entries stay as-is.
const captureLabel = (alias, source) => {
    if (alias) return alias;
    try {
        const u = new URL(source);
        return u.pathname.split('/').filter(Boolean).pop() || source;
    } catch (_) {
        return source;
    }
};

// Load a single capture from either a relative path (resolved under
// `data/`) or an absolute http(s) URL. Mirrors loadDemo's lifecycle but
// canonicalizes the address bar to ?capture=… instead of ?demo=… so
// URL pins survive a refresh.
async function loadCapture(source, alias = null) {
    const display = captureLabel(alias, source);
    splashLabel = display;
    splashProgress = -1;
    landingError = null;
    m.redraw();

    try {
        const data = await fetchParquetBytes(source, (p) => {
            splashProgress = p;
            m.redraw();
        });

        splashLabel = 'Initializing';
        splashProgress = -1;
        m.redraw();

        await loadParquet(data, display);

        // Canonicalize address bar to ?capture=[alias=]source so the
        // URL is shareable and bookmarkable.
        const url = new URL(window.location);
        url.searchParams.delete('demo');
        url.searchParams.delete('demoA');
        url.searchParams.delete('demoB');
        url.searchParams.delete('capture');
        url.searchParams.append('capture', alias ? `${alias}=${source}` : source);
        window.history.replaceState(null, '', url);
    } catch (e) {
        splashLabel = null;
        const msg = e?.message ?? e ?? 'unknown error';
        // Cross-origin fetch failures usually surface as opaque "Failed
        // to fetch" — point users at the proxy workaround.
        const isUrl = /^https?:\/\//i.test(source);
        landingError = isUrl
            ? `Failed to load ${display}: ${msg}. If this is a cross-origin URL, the source may be missing CORS headers — host the viewer with \`rezolus view --proxy-allow=<host>\` to bypass.`
            : `Failed to load ${display}: ${msg}`;
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
            fetchParquetBytes(fileA, (p) => {
                aDone = p;
                splashProgress = (aDone + bDone) / 2;
                m.redraw();
            }),
            fetchParquetBytes(fileB, (p) => {
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

        try {
            const sections = await ViewerApi.getSections();
            bootstrapSharedSections(Array.isArray(sections) ? sections : (sections?.data?.sections || []));
        } catch (_) {
            bootstrapSharedSections([]);
        }
        const reportMode = base.fileMetadata?.report === 'trimmed'
            || experimentFileMetadata?.report === 'trimmed';
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
            reportMode,
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
            onFile: loadFile,
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
            onLoadUrl: (raw) => {
                // Comma separates A/B for compare mode. Each entry uses
                // the same alias=URL grammar as the URL params, so a
                // paste like `vllm=https://…/a.parquet, sglang=https://…/b.parquet`
                // seeds legends and routes to compare mode automatically.
                const entries = raw.split(',').map((s) => s.trim()).filter(Boolean);
                if (entries.length >= 2) {
                    const [labelA, fileA] = splitAlias(entries[0]);
                    const [labelB, fileB] = splitAlias(entries[1]);
                    const legends = (labelA || labelB)
                        ? { baseline: labelA, experiment: labelB }
                        : null;
                    loadCompareDemo(fileA, fileB, legends, null);
                } else if (entries.length === 1) {
                    const [alias, source] = splitAlias(entries[0]);
                    loadCapture(source, alias);
                }
            },
            // Static-site bundle has no local proxy; URL loading goes
            // direct from the browser, so CORS on the source matters.
            urlLoading: 'direct',
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
} else if (_captureRaw.length === 1) {
    const [alias, source] = splitAlias(_captureRaw[0]);
    splashLabel = alias || source;
    m.mount(document.body, Root);
    loadCapture(source, alias);
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
