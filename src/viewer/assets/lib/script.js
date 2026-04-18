// script.js — Server viewer bootstrap stub.
// Handles mode detection, backend state fetching, live mode, and transport controls.
// Delegates all UI/routing to app.js via initDashboard().

import { ViewerApi } from './viewer_api.js';
import { FileUpload } from './landing.js';
import { notify, showSaveModal } from './overlays.js';
import { reportStore, setStorageScope, loadPayloadIntoStore } from './selection.js';
import { clearMetadataCache, processDashboardData } from './data.js';
import { initDashboard, sectionResponseCache, clearViewerCaches, chartsState, getHeatmapEnabled, heatmapDataCache, fetchSectionHeatmapData, getActiveCgroupPattern, preloadSections } from './app.js';

// ── Backend state fetching ─────────────────────────────────────────

let systemInfo = null;
let fileChecksum = null;
let fileMetadata = null;
let selectionPayload = null;
let liveMode = false;
let recording = true;

const fetchBackendState = async () => {
    const [metaResult, sysResult, selResult, fmResult] = await Promise.allSettled([
        ViewerApi.getMetadata(),
        ViewerApi.getSystemInfo(),
        ViewerApi.getSelection(),
        ViewerApi.getFileMetadata(),
    ]);
    if (metaResult.status === 'fulfilled') {
        const r = metaResult.value;
        if (r.status === 'success' && r.data?.fileChecksum) {
            fileChecksum = r.data.fileChecksum;
        }
    }
    if (sysResult.status === 'fulfilled') {
        systemInfo = sysResult.value;
    }
    if (selResult.status === 'fulfilled') {
        selectionPayload = selResult.value;
    }
    if (fmResult.status === 'fulfilled') {
        fileMetadata = fmResult.value;
    }
};

// ── Transport controls ─────────────────────────────────────────────

const startRecording = async () => {
    try {
        await ViewerApi.reset();
        clearViewerCaches();
        recording = true;
        m.redraw();
    } catch (e) {
        console.error('Failed to start recording:', e);
    }
};

const stopRecording = () => {
    recording = false;
};

const saveCapture = async () => {
    const result = await showSaveModal('rezolus-capture', '.parquet');
    if (!result) return;
    const filename = result.filename;
    const a = document.createElement('a');
    a.href = ViewerApi.saveUrl();
    a.download = filename;
    document.body.appendChild(a);
    a.click();
    document.body.removeChild(a);
    notify('info', `Saved ${filename}`);
};

const uploadParquet = async (file) => {
    try {
        await ViewerApi.uploadParquet(file);
        clearViewerCaches();
        clearMetadataCache();
        chartsState.resetAll();
        await fetchBackendState();
        if (fileChecksum) {
            setStorageScope({ filename: fileChecksum });
        }
        const data = await ViewerApi.getSection('overview');
        const processed = await processDashboardData(data, null, '/overview');
        sectionResponseCache['overview'] = processed;
        if (processed.sections) preloadSections(processed.sections);

        if (m.route.get() !== '/overview') {
            m.route.set('/overview');
        }
        m.redraw();
    } catch (e) {
        notify('error', `Failed to upload parquet: ${e.message || e}`);
    }
};

// ── Live refresh ───────────────────────────────────────────────────

let liveRefreshInProgress = false;

const refreshCurrentSection = async () => {
    if (liveRefreshInProgress) return;
    if (!recording || !chartsState.isDefaultZoom()) return;

    const currentRoute = m.route.get();
    if (!currentRoute) return;

    const section = currentRoute.replace(/^\//, '');
    if (!section || section === 'query') return;

    liveRefreshInProgress = true;
    try {
        const data = await ViewerApi.getSection(section, true);

        const promises = [processDashboardData(data, getActiveCgroupPattern(), currentRoute)];
        if (getHeatmapEnabled()) {
            promises.push(fetchSectionHeatmapData(currentRoute, data.groups));
        }
        await Promise.all(promises);

        sectionResponseCache[section] = data;
        m.redraw();
    } catch (e) {
        // Keep existing data on error
    } finally {
        liveRefreshInProgress = false;
    }
};

// ── Landing page ───────────────────────────────────────────────────

let landingState = { loading: false, error: null };

const showLanding = () => {
    m.mount(document.body, {
        view: () => m(FileUpload, {
            onFile: async (file) => {
                landingState.loading = true;
                landingState.error = null;
                m.redraw();
                try {
                    await ViewerApi.uploadParquet(file);
                    window.location.reload();
                } catch (e) {
                    landingState.loading = false;
                    landingState.error = `Failed to load file: ${e.message || e}`;
                    m.redraw();
                }
            },
            onConnect: async (url) => {
                landingState.loading = true;
                landingState.error = null;
                m.redraw();
                try {
                    await ViewerApi.connectAgent(url);
                    window.location.reload();
                } catch (e) {
                    landingState.loading = false;
                    landingState.error = `Failed to connect: ${e.message || e}`;
                    m.redraw();
                }
            },
            loading: landingState.loading,
            error: landingState.error,
        }),
    });
};

// ── Bootstrap ──────────────────────────────────────────────────────

const bootstrap = async () => {
    try {
        const response = await ViewerApi.getMode();
        if (!response.loaded && !response.live) {
            showLanding();
            return;
        }
        liveMode = response.live === true;
    } catch (_) { /* assume loaded file mode */ }

    await fetchBackendState();
    if (fileChecksum) {
        setStorageScope({ filename: fileChecksum });
    }

    initDashboard({
        systemInfo,
        fileChecksum,
        fileMetadata,
        selectionPayload,
        liveMode,
        recording,
        onStartRecording: startRecording,
        onStopRecording: stopRecording,
        onSaveCapture: saveCapture,
        onUploadParquet: uploadParquet,
        onRefresh: liveMode ? refreshCurrentSection : null,
        showLanding,
    });
};

bootstrap();
