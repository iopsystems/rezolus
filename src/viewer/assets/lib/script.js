// script.js — Server viewer bootstrap stub.
// Handles mode detection, backend state fetching, live mode, and transport controls.
// Delegates all UI/routing to app.js via initDashboard().

import { ViewerApi } from './viewer_api.js';
import { FileUpload, CompareLanding, splitAlias } from './ui/landing.js';
import { notify, showSaveModal } from './ui/overlays.js';
import { setStorageScope, loadPayloadIntoStore, reportStore, clearStore, seedEventsFromMetadata } from './selection/selection.js';
import { clearMetadataCache, processDashboardData, nativeInterval, stepAtLeast, sameExtent, CAPTURE_EXPERIMENT } from './data.js';
import { initDashboard, cacheSectionResponse, bootstrapSharedSections, clearViewerCaches, resetLinkedViewState, reapplyFileMetadata, chartsState, getHeatmapEnabled, heatmapDataCache, fetchSectionHeatmapData, getActiveCgroupPattern, getRecording, setRecording, preloadSections, stopRefreshing, noteRecordingExtent } from './app.js';

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
// The opened file is an archive still being written; see `following` in
// app.js.
let followMode = false;
let baselineAlias = null;
// The baseline's [minTime, maxTime] in seconds, so a link's from/to can be
// clamped before the first section loads.
let baselineQueryRange = null;

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
        baselineQueryRange = null;
        if (r.status === 'success') {
            const start = Number(r.data?.minTime);
            const end = Number(r.data?.maxTime);
            if (Number.isFinite(start) && Number.isFinite(end) && end > start) {
                baselineQueryRange = { start, end };
            }
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
        const res = await ViewerApi.reset();
        if (res?.status !== 'success') {
            notify('error', `Could not reconnect: ${res?.error ?? 'unknown error'}`);
            return;
        }
        // The server now holds a new recording. A refresh still in flight
        // reads the old one, so it is aborted and its results dropped, and
        // the cached metadata (the old recording's time range) goes too.
        liveGen++;
        if (liveRefreshController) liveRefreshController.abort();
        // A superseded refresh may still be waiting on a request that takes
        // no signal; the next refresh does not wait for it.
        liveRefreshInProgress = false;
        clearViewerCaches();
        clearMetadataCache();
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
    const result = await showSaveModal('rezolus-capture', '.dendro');
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
        // The previous file's window, time mode and selections, and the URL
        // keys that carried them, mean nothing for this file.
        resetLinkedViewState();
        chartsState.resetAll();
        await fetchBackendState();
        // Node and instance selections come from the new file, as they do
        // on the initial load. The server keeps an attached experiment
        // across a baseline upload, so in compare mode the node list is
        // the union of both captures, as on the initial load.
        let uploadedExperimentFm = null;
        try {
            const mode = await ViewerApi.getMode();
            if (mode?.compare_mode === true) {
                uploadedExperimentFm = await ViewerApi.getFileMetadata(CAPTURE_EXPERIMENT).catch(() => null);
            }
        } catch (_) { /* single-file upload */ }
        reapplyFileMetadata(fileMetadata, uploadedExperimentFm);
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
// Bumped when a reset starts a new live recording; a refresh begun under an
// older value is superseded. The controller aborts that refresh's requests.
let liveGen = 0;
let liveRefreshController = null;
// The baseline extent the last poll of a followed file saw.
let lastFollowExtent = null;

const refreshCurrentSection = async () => {
    if (liveRefreshInProgress || !getRecording()) return;

    liveRefreshInProgress = true;
    // Superseded when a reset starts a new recording before this lands.
    const gen = liveGen;
    const isStale = () => gen !== liveGen;
    const controller = new AbortController();
    liveRefreshController = controller;
    const signal = controller.signal;
    try {
        // A live recording that stopped (the agent refused a reconnect, or a
        // write failed) no longer advances. Say so until dismissed, and stop
        // presenting the view as recording. Checked before the zoom and route
        // checks, so it is seen from a zoomed chart or the query page too.
        const meta = await ViewerApi.getMetadata();
        if (isStale()) return;
        const liveError = meta?.data?.liveError;
        if (liveError) {
            stopRecording();
            notify('error', `Live recording stopped: ${liveError}. Record again to reconnect.`, 2147483647);
            m.redraw();
            return;
        }

        // The follow of an archive file ended (it was finalized or removed,
        // or the baseline was replaced), so this refresh is the last one.
        if (followMode && meta?.data?.following !== true) {
            followMode = false;
            stopRefreshing();
        }
        await noteRecordingExtent(meta);

        // A followed file whose extent is unchanged since the last poll has
        // no new rows to show, so the section is not queried again. The
        // redraw lets compare charts fetch the experiment if its range
        // changed. The poll after a follow ends always queries.
        const extent = { start: meta?.data?.minTime, end: meta?.data?.maxTime };
        const unchanged = followMode && sameExtent(extent, lastFollowExtent);
        lastFollowExtent = extent;
        if (unchanged) {
            m.redraw();
            return;
        }

        if (!chartsState.isDefaultZoom()) return;
        const currentRoute = m.route.get();
        if (!currentRoute) return;
        const section = currentRoute.replace(/^\//, '');
        if (!section || section === 'query') return;

        const data = await ViewerApi.getSection(section, true);
        if (isStale()) return;

        // freshMetadata: TSDB grows continuously in live mode; without
        // this the query window stays frozen at first-fetch maxTime and
        // charts visibly stop updating even though /api/v1/query_range
        // would serve fresh data if asked.
        const promises = [processDashboardData(data, getActiveCgroupPattern(), currentRoute, { freshMetadata: true, isStale, signal })];
        if (getHeatmapEnabled()) {
            promises.push(fetchSectionHeatmapData(currentRoute, data.groups, isStale));
        }
        const [processed] = await Promise.all(promises);
        if (isStale()) return;

        cacheSectionResponse(section, processed);
        m.redraw();
    } catch (e) {
        // Keep existing data on error.
    } finally {
        // A superseded refresh leaves the flag to the refreshes after the reset.
        if (!isStale()) liveRefreshInProgress = false;
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
        followMode = response.following === true;
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
                    step: stepAtLeast(nativeInterval(data), (end - start) / 500),
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
        following: followMode,
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
        queryRange: baselineQueryRange,
        recording: true,
        onStartRecording: startRecording,
        onStopRecording: stopRecording,
        onSaveCapture: saveCapture,
        onUploadParquet: uploadParquet,
        onRefresh: (liveMode || followMode) ? refreshCurrentSection : null,
    });
};

bootstrap();
