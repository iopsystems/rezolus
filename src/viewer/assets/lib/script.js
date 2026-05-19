// script.js — Server viewer bootstrap stub.
// Handles mode detection, backend state fetching, live mode, and transport controls.
// Delegates all UI/routing to app.js via initDashboard().

import { ViewerApi } from './viewer_api.js';
import { FileUpload, CompareLanding, splitAlias } from './ui/landing.js';
import { notify, showSaveModal } from './ui/overlays.js';
import { setStorageScope, loadPayloadIntoStore, reportStore, clearStore, seedEventsFromMetadata } from './selection/selection.js';
import { clearMetadataCache, processDashboardData, CAPTURE_EXPERIMENT } from './data.js';
import { initDashboard, cacheSectionResponse, bootstrapSharedSections, bootstrapSectionStatus, clearViewerCaches, chartsState, getHeatmapEnabled, heatmapDataCache, fetchSectionHeatmapData, getActiveCgroupPattern, getRecording, setRecording, preloadSections } from './app.js';

// Splash: mounted on body before any async bootstrap step so the page
// never shows a blank document while we fetch state. Replaced by the
// route mount inside initDashboard() once ready to render the dashboard.

let splashLabel = 'Initializing';

const Splash = {
    view: () => splashLabel === null ? null : m('div#splash', m('div.card', [
        m('h1', 'Rezolus'),
        m('p.subtitle', `${splashLabel}…`),
        m('div.progress-bar', m('div.progress-fill.indeterminate')),
    ])),
};

m.mount(document.body, Splash);

const setSplashLabel = (label) => {
    splashLabel = label;
    m.redraw();
};

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
        // Must run after setStorageScope so a persisted working set wins;
        // only seeds footer events when nothing is persisted.
        seedEventsFromMetadata(fileMetadata);

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

        // Fetch per-section status server-side so the sidebar can
        // gray out empty sections before the user clicks any of
        // them. Don't block on it — the navigation list above is
        // what gets the page rendering; status badges fill in async.
        ViewerApi.getSectionStatus()
            .then((resp) => bootstrapSectionStatus(resp?.data || {}))
            .catch((e) => console.warn('section_status fetch failed:', e))
            .finally(() => m.redraw());

        // A trimmed Save-as-Report parquet has no section data (most
        // columns are projected away) and the backend stamps its
        // section list to []. Don't phantom-load /overview — go
        // straight to the Report view, same as the cold-boot path.
        let isReport = false;
        try {
            const mode = await ViewerApi.getMode();
            isReport = mode?.report === true;
        } catch (_) { /* fall through to overview */ }

        if (isReport) {
            if (m.route.get() !== '/report') {
                m.route.set('/report');
            }
            m.redraw();
            return;
        }

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
        // Keep existing data on error.
    } finally {
        liveRefreshInProgress = false;
    }
};

let landingState = {
    loading: false,
    error: null,
    baselineAttached: false,
    baselineFilename: null,
    experimentAttached: false,
    experimentFilename: null,
    urlLoading: 'disabled',
};

// Best-effort probe of the URL-loading mode the backend exposes via
// /api/v1/mode. Drives the landing's "Load from URL" hint and disabled
// state. A failed probe leaves it 'disabled' (input greyed out).
ViewerApi.getMode()
    .then((res) => {
        const mode = res?.data?.url_loading ?? res?.url_loading;
        if (mode) {
            landingState.urlLoading = mode;
            m.redraw();
        }
    })
    .catch(() => { /* default 'disabled' */ });

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
            onLoadUrl: async (raw) => {
                // Single URL only on the binary-viewer landing — A/B
                // ingestion goes through the experiment-attach flow
                // after the baseline lands, not via the URL field.
                landingState.loading = true;
                landingState.error = null;
                m.redraw();
                try {
                    const [, source] = splitAlias(raw.trim());
                    const res = await ViewerApi.loadFromUrl(source.trim());
                    // Backend wraps both success and recoverable errors
                    // (invalid parquet, upstream 404, allowlist deny) in
                    // an ApiResponse envelope, so check status before
                    // reloading — otherwise an error gets lost in the
                    // page refresh.
                    if (res?.status === 'error') {
                        throw new Error(res.error || 'unknown error');
                    }
                    window.location.reload();
                } catch (e) {
                    landingState.loading = false;
                    landingState.error = `Failed to load URL: ${e?.message ?? e ?? 'unknown error'}`;
                    m.redraw();
                }
            },
            urlLoading: landingState.urlLoading,
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

const bootstrap = async () => {
    let compareMode = false;
    let combinedAB = false;
    let reportMode = false;
    let categoryName = null;
    setSplashLabel('Connecting to viewer');
    try {
        const response = await ViewerApi.getMode();
        if (!response.loaded && !response.live) {
            showLanding();
            return;
        }
        liveMode = response.live === true;
        compareMode = response.compare_mode === true;
        categoryName = response.category || null;
        combinedAB = response.combined_ab === true;
        reportMode = response.report === true;
    } catch (_) { /* assume loaded file mode */ }

    setSplashLabel('Loading capture metadata');
    await fetchBackendState();
    setSplashLabel('Loading section list');
    try {
        const sectionsResponse = await ViewerApi.getSections();
        bootstrapSharedSections(sectionsResponse?.data?.sections || []);
    } catch (_) {
        bootstrapSharedSections([]);
    }

    // Fetch per-section status server-side so the sidebar's
    // empty-section gray-out is correct on first paint. Done under
    // the splash (next to the already-awaited sections call) rather
    // than async-after-mount — the cost is ~200ms of additional
    // splash time, but the sidebar then renders with the right
    // state once, instead of flashing from "all bright" to
    // "gray-out applied".
    setSplashLabel('Loading section status');
    try {
        const statusResponse = await ViewerApi.getSectionStatus();
        bootstrapSectionStatus(statusResponse?.data || {});
    } catch (e) {
        console.warn('section_status fetch failed:', e);
    }

    if (fileChecksum) {
        setStorageScope({ filename: fileChecksum });
    }
    seedEventsFromMetadata(fileMetadata);

    let experimentSystemInfo = null;
    let experimentFileMetadata = null;
    let experimentFilename = null;
    let experimentAlias = null;
    let experimentQueryRange = null;
    if (compareMode) {
        setSplashLabel('Loading experiment capture');
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
        combinedAB,
        reportMode,
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
