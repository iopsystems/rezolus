// script.js — Server viewer bootstrap stub.
// Handles mode detection, backend state fetching, live mode, and transport controls.
// Delegates all UI/routing to app.js via initDashboard().

import { ViewerApi } from './viewer_api.js';
import { FileUpload, CompareLanding } from './landing.js';
import { notify, showSaveModal } from './overlays.js';
import { setStorageScope, loadPayloadIntoStore, reportStore, clearStore } from './selection.js';
import { clearMetadataCache, processDashboardData, CAPTURE_EXPERIMENT } from './data.js';
import { initDashboard, cacheSectionResponse, bootstrapSharedSections, clearViewerCaches, chartsState, getHeatmapEnabled, heatmapDataCache, fetchSectionHeatmapData, getActiveCgroupPattern, getRecording, setRecording, preloadSections } from './app.js';

// ── Backend state fetching ─────────────────────────────────────────

let systemInfo = null;
let fileChecksum = null;
let fileMetadata = null;
let selectionPayload = null;
let liveMode = false;
let baselineAlias = null;

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
        // Display alias carried in /api/v1/metadata response when the
        // CLI was launched with `alias=path`. Absent field = no alias.
        if (r.status === 'success' && r.data?.alias) {
            baselineAlias = r.data.alias;
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
        setRecording(true);
        m.redraw();
    } catch (e) {
        console.error('Failed to start recording:', e);
    }
};

const stopRecording = () => {
    setRecording(false);
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

        // If the uploaded parquet has an embedded selection/report, load
        // it into the reportStore so the "Report" sidebar link appears
        // and the view can render the saved charts.
        clearStore(reportStore);
        if (selectionPayload && Array.isArray(selectionPayload.entries)) {
            loadPayloadIntoStore(reportStore, selectionPayload);
            reportStore.loadedFrom = 'embedded report';
        }

        const sectionsResponse = await ViewerApi.getSections();
        bootstrapSharedSections(sectionsResponse?.data?.sections || []);

        const data = await ViewerApi.getSection('overview');
        const processed = await processDashboardData(data, null, '/overview');
        cacheSectionResponse('overview', processed);
        if (processed.sections) preloadSections(processed.sections);

        if (m.route.get() !== '/overview') {
            m.route.set('/overview');
        }
        m.redraw();
    } catch (e) {
        notify('error', `Failed to upload parquet: ${e?.message ?? e ?? 'unknown error'}`);
    }
};

// ── Live refresh ───────────────────────────────────────────────────

let liveRefreshInProgress = false;

const refreshCurrentSection = async () => {
    if (liveRefreshInProgress) return;
    if (!getRecording() || !chartsState.isDefaultZoom()) return;

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
        const [processed] = await Promise.all(promises);

        cacheSectionResponse(section, processed);
        m.redraw();
    } catch (e) {
        // Keep existing data on error
    } finally {
        liveRefreshInProgress = false;
    }
};

// ── Landing page ───────────────────────────────────────────────────

let landingState = {
    loading: false,
    error: null,
    baselineAttached: false,
    baselineFilename: null,
    experimentAttached: false,
    experimentFilename: null,
};

const isCompareRequested = () =>
    new URLSearchParams(window.location.search).get('compare') === '1';

const showLanding = () => {
    if (isCompareRequested()) {
        showCompareLanding();
        return;
    }
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
                    landingState.error = `Failed to load file: ${e?.message ?? e ?? 'unknown error'}`;
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
                    landingState.error = `Failed to connect: ${e?.message ?? e ?? 'unknown error'}`;
                    m.redraw();
                }
            },
            loading: landingState.loading,
            error: landingState.error,
        }),
    });
};

const showCompareLanding = () => {
    m.mount(document.body, {
        view: () => m(CompareLanding, {
            baselineAttached: landingState.baselineAttached,
            baselineFilename: landingState.baselineFilename,
            experimentAttached: landingState.experimentAttached,
            experimentFilename: landingState.experimentFilename,
            loading: landingState.loading,
            error: landingState.error,
            onBaselineFile: async (file) => {
                landingState.loading = true;
                landingState.error = null;
                m.redraw();
                try {
                    await ViewerApi.uploadParquet(file);
                    landingState.baselineAttached = true;
                    landingState.baselineFilename = file.name || null;
                    landingState.loading = false;
                    m.redraw();
                } catch (e) {
                    landingState.loading = false;
                    landingState.error = `Failed to load baseline: ${e?.message ?? e ?? 'unknown error'}`;
                    m.redraw();
                }
            },
            onExperimentFile: async (file) => {
                landingState.loading = true;
                landingState.error = null;
                m.redraw();
                try {
                    await ViewerApi.attachExperiment(file);
                    landingState.experimentAttached = true;
                    landingState.experimentFilename = file.name || null;
                    landingState.loading = false;
                    m.redraw();
                    // Both captures attached — reload into full compare view.
                    window.location.reload();
                } catch (e) {
                    landingState.loading = false;
                    landingState.error = `Failed to load experiment: ${e?.message ?? e ?? 'unknown error'}`;
                    m.redraw();
                }
            },
        }),
    });
};

// ── Bootstrap ──────────────────────────────────────────────────────

const bootstrap = async () => {
    let compareMode = false;
    let categoryName = null;
    try {
        const response = await ViewerApi.getMode();
        if (!response.loaded && !response.live) {
            showLanding();
            return;
        }
        liveMode = response.live === true;
        compareMode = response.compare_mode === true;
        categoryName = response.category || null;
    } catch (_) { /* assume loaded file mode */ }

    await fetchBackendState();
    if (fileChecksum) {
        setStorageScope({ filename: fileChecksum });
    }

    let experimentSystemInfo = null;
    let experimentFileMetadata = null;
    let experimentFilename = null;
    let experimentAlias = null;
    let experimentQueryRange = null;
    if (compareMode) {
        const [sysinfo, fileMeta, expMeta] = await Promise.all([
            ViewerApi.getSystemInfo(CAPTURE_EXPERIMENT).catch(() => null),
            ViewerApi.getFileMetadata(CAPTURE_EXPERIMENT).catch(() => null),
            ViewerApi.getMetadata(CAPTURE_EXPERIMENT).catch(() => null),
        ]);
        experimentSystemInfo = sysinfo;
        experimentFileMetadata = fileMeta;
        experimentFilename = expMeta?.data?.filename || null;
        experimentAlias = expMeta?.data?.alias || null;
        const data = expMeta?.data ?? expMeta;
        const minT = data?.minTime ?? data?.min_time ?? data?.start_time;
        const maxT = data?.maxTime ?? data?.max_time ?? data?.end_time;
        if (minT != null && maxT != null) {
            const start = Number(minT);
            const end = Number(maxT);
            if (Number.isFinite(start) && Number.isFinite(end) && end > start) {
                experimentQueryRange = {
                    start,
                    end,
                    step: Math.max(1, Math.floor((end - start) / 500)),
                };
            }
        }
    }

    initDashboard({
        systemInfo,
        fileChecksum,
        fileMetadata,
        selectionPayload,
        liveMode,
        compareMode,
        categoryName,
        baselineAlias,
        experimentSystemInfo,
        experimentFileMetadata,
        experimentFilename,
        experimentAlias,
        experimentQueryRange,
        recording: true,
        onStartRecording: startRecording,
        onStopRecording: stopRecording,
        onSaveCapture: saveCapture,
        onUploadParquet: uploadParquet,
        onRefresh: liveMode ? refreshCurrentSection : null,
    });
};

bootstrap();
