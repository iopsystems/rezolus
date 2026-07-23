// app.js — Shared dashboard logic for both server and WASM viewers.
// Exports initDashboard(config) which sets up state and mounts the Mithril router.

import { ChartsState, Chart } from './charts/chart.js';
import { QueryExplorer, SingleChartView } from './features/explorers.js';
import { CgroupSelector } from './features/cgroup_selector.js';
import { GpuSelector } from './features/gpu_selector.js';
import globalColorMapper from './charts/util/colormap.js';
import { TopNav, Sidebar, countCharts, formatSize } from './ui/layout.js';
import { collectGroupPlots } from './features/group_utils.js';
import { CpuTopology } from './features/topology.js';
import { executePromQLRangeQuery, applyResultToPlot, fetchHeatmapsForGroups, substituteCgroupPattern, processDashboardData, clearMetadataCache, clearDisplayTiles, setStepOverride, getStepOverride, setRateMode, getRateMode, setSelectedNode, setSelectedInstance, getSelectedNode, setSelectedGpus, getSelectedGpus, injectLabel, setDisplayMode, getDisplayMode, setRangeOverride, getRangeOverride, CAPTURE_EXPERIMENT } from './data.js';

// Opt line-ish charts into display (boxplot decimation) mode: they fetch the
// decimated boxplot binary instead of the full native-resolution JSON matrix.
setDisplayMode(true);
import { reportStore, notebookStore, loadedSelectionStore, persistNotebook, setStorageScope, loadPayloadIntoStore, NotebookView, ReportView, LoadedSelectionView, setChartToggle as setChartToggleInStore, setAnchor } from './selection/selection.js';
import { SaveModal } from './ui/overlays.js';
import { ViewerApi } from './viewer_api.js';
import { createSystemInfoView, createMetadataView, renderCgroupSection } from './sections/section_views.js';
import { buildTopNavAttrs, createMainComponent } from './ui/navigation.js';
import { initTheme } from './ui/theme.js';
import { isHistogramPlot } from './charts/metric_types.js';
import { renderServiceSection, createServiceRoutes } from './features/service.js';
import { MetricBrowserView } from './features/metric_browser.js';
import { createSourceRoutes } from './features/source_routes.js';
import { createGroupComponent, getCachedSectionMeta, buildClientOnlySectionView } from './viewer_core.js';
import { renderSectionNotes } from './sections/section_notes.js';
import {
    createSectionCacheState,
    storeSectionResponse,
    storeSharedSections,
    getSections,
    withSharedSections,
    clearSectionResponses,
    clearNonServiceResponses,
    resetSectionCacheState,
    setSectionCacheLimit,
    pinSectionKey,
} from './sections/section_cache.js';

let activeSectionRoute = null;

// Captured at mount-time from the HTML <title> so we can fall back to
// it when neither section nor filename is known yet (e.g. initial load).
const DEFAULT_DOCUMENT_TITLE = (typeof document !== 'undefined' && document.title) || 'Rezolus';

// Update browser tab title to reflect the active section + loaded file
// so pasted URLs (e.g. .../viewer/?demo=vllm.parquet#/gpu) preview
// usefully in link-unfurling and tab strips.
const updateDocumentTitle = (sectionKey) => {
    if (typeof document === 'undefined') return;
    const section = getCachedSections().find(s => s.route === `/${sectionKey}`);
    const sectionName = section?.name;
    const filename = fileMetadata?.filename;
    if (sectionName && filename) {
        document.title = `${sectionName} (${filename}) — Rezolus`;
    } else if (sectionName) {
        document.title = `${sectionName} — Rezolus`;
    } else if (filename) {
        document.title = `${filename} — Rezolus`;
    } else {
        document.title = DEFAULT_DOCUMENT_TITLE;
    }
};
let systemInfoData = null;
let fileChecksum = null;
let fileMetadata = null;
let nodeList = [];
let nodeVersions = {};
let selectedNode = null;
let gpuList = [];          // GPU ids present in the recording (e.g. [0, 1])
let selectedGpus = [];     // selected GPU ids filtering the GPU section, [] = all
let serviceInstances = {};
let selectedInstances = {};
let activeCgroupPattern = null;
let heatmapEnabled = false;
let heatmapLoading = false;
const heatmapDataCache = new Map();
const chartsState = new ChartsState();
let currentGranularity = null;
let currentTimeMode = 'grid';
const sectionCacheState = createSectionCacheState();
// Cache limit: overview (pinned) + active route + one look-ahead. Keeps
// memory bounded on the static-site WASM viewer where each section body
// can be MB-scale.
setSectionCacheLimit(sectionCacheState, 3);
pinSectionKey(sectionCacheState, 'overview');
const sectionResponseCache = sectionCacheState.responses;
const cacheSectionResponse = (section, data) =>
    storeSectionResponse(sectionCacheState, section, data);
const bootstrapSharedSections = (sections) =>
    storeSharedSections(sectionCacheState, sections);
const withCachedSections = (data) => withSharedSections(sectionCacheState, data);
const getCachedSections = () => getSections(sectionCacheState);

let compareMode = false;
let combinedAB = false;
let reportMode = false;
let experimentAttached = false;
let experimentSystemInfo = null;
let experimentDurationMs = null;
let experimentFilename = null;
// {start, end, step} in PromQL-native seconds, cached at compare-mode
// entry so every CompareChartWrapper skips its per-chart
// ViewerApi.getMetadata round-trip. Cleared on detach.
let experimentQueryRange = null;
export const getExperimentQueryRange = () => experimentQueryRange;

// Optional display aliases for the two captures, threaded in via
// initDashboard when the CLI was launched with `alias=path` or the
// static-site WASM Viewer had set_alias() called on it. null means
// "no alias set, UI falls back to the capture id (baseline / experiment)".
let baselineAlias = null;
let experimentAlias = null;
export const getBaselineAlias = () => baselineAlias;
export const getExperimentAlias = () => experimentAlias;

// Compare-mode per-chart toggles + anchors live in `notebookStore` so
// they persist across page reloads. See selection_migration.js for the
// schema. The accessors below read-through to the store.

let liveMode = false;
let recording = false;
let onStartRecording = null;
let onStopRecording = null;
let onSaveCapture = null;
let onUploadParquet = null;
let onRefresh = null;
let liveRefreshInterval = null;

let Main;
let SystemInfoView;
let MetadataView;
let Group;

const initComponents = () => {
    Group = createGroupComponent(() => ({
        chartsState, heatmapEnabled, heatmapLoading, heatmapDataCache,
        compareMode,
        toggles: notebookStore.chartToggles || {},
        setChartToggle,
        anchors: notebookStore.anchors || { baseline: 0, experiment: 0 },
        experimentQueryRange,
        baselineAlias,
        experimentAlias,
    }));

    SystemInfoView = createSystemInfoView({
        CpuTopology,
        formatBytes: formatSize,
    });

    MetadataView = createMetadataView();
};

// Tries the structured duration_ms field first; falls back to
// max_time - min_time. Returns null when neither is available.
const durationFromFileMetadata = (fileMeta) => {
    if (!fileMeta) return null;
    if (typeof fileMeta.duration_ms === 'number') return fileMeta.duration_ms;
    if (typeof fileMeta.maxTime === 'number' && typeof fileMeta.minTime === 'number') {
        return fileMeta.maxTime - fileMeta.minTime;
    }
    if (typeof fileMeta.max_time === 'number' && typeof fileMeta.min_time === 'number') {
        return fileMeta.max_time - fileMeta.min_time;
    }
    return null;
};

// /api/v1/metadata returns minTime/maxTime in PromQL-native seconds
// (same scale as plot.data[0] timestamps). Returns null when the
// response shape doesn't include a recognisable time range.
const queryRangeFromMeta = (meta) => {
    const data = meta?.data ?? meta;
    const minT = data?.minTime ?? data?.min_time ?? data?.start_time;
    const maxT = data?.maxTime ?? data?.max_time ?? data?.end_time;
    if (minT == null || maxT == null) return null;
    const start = Number(minT);
    const end = Number(maxT);
    if (!Number.isFinite(start) || !Number.isFinite(end) || end <= start) return null;
    return {
        start, end,
        step: Math.max(1, Math.floor((end - start) / 500)),
        // Native sampling step, so the compare-mode boxplot fetch can decimate
        // the experiment onto the SAME grid as the baseline (see
        // fetchExperimentResult). Distinct from `step`, which targets ~500
        // points for the plain matrix fetch.
        interval: Math.max(1, Number(data.interval) || 1),
    };
};

const attachExperiment = async (file) => {
    const sysinfo = await ViewerApi.attachExperiment(file);
    const [expFileMeta, expMeta] = await Promise.all([
        ViewerApi.getFileMetadata(CAPTURE_EXPERIMENT).catch(() => null),
        ViewerApi.getMetadata(CAPTURE_EXPERIMENT).catch(() => null),
    ]);
    experimentSystemInfo = sysinfo || null;
    experimentDurationMs = durationFromFileMetadata(expFileMeta);
    experimentQueryRange = queryRangeFromMeta(expMeta);
    // Prefer server-stamped filename (works in both file-drop and CLI paths);
    // fall back to the dropped File's name.
    experimentFilename = (expMeta?.data?.filename)
        || (file && file.name)
        || (expFileMeta && (expFileMeta.filename || expFileMeta.file_name))
        || null;
    experimentAlias = expMeta?.data?.alias || null;
    experimentAttached = true;
    compareMode = true;

    applyMultiNodeInfo(expFileMeta);
    clearViewerCaches();

    // Clamp a stale anchor when the newly-attached experiment is
    // shorter than the previously-saved offset. Avoids a chart starting
    // past the end of its data.
    const anchors = notebookStore.anchors || { baseline: 0, experiment: 0 };
    if (experimentDurationMs != null && anchors.experiment > experimentDurationMs) {
        setAnchor(CAPTURE_EXPERIMENT, experimentDurationMs);
        console.info(
            `[compare] experiment anchor clamped to ${experimentDurationMs}ms to fit capture duration`,
        );
    }

    m.redraw();
};

const detachExperiment = async () => {
    try { await ViewerApi.detachExperiment(); }
    catch (err) { console.warn('[compare] detachExperiment failed; server may leak a temp file', err); }
    experimentSystemInfo = null;
    experimentDurationMs = null;
    experimentFilename = null;
    experimentQueryRange = null;
    experimentAlias = null;
    experimentAttached = false;
    compareMode = false;

    applyMultiNodeInfo(null);
    clearViewerCaches();
    m.redraw();
};

// Replace the currently attached experiment with a new parquet. Unlike
// detachExperiment + attachExperiment, this preserves compareMode across
// the transition so the UI doesn't bounce back to single-capture mode.
const loadExperiment = async (file) => {
    if (experimentAttached) {
        try { await ViewerApi.detachExperiment(); } catch (_) { /* best effort */ }
    }
    await attachExperiment(file);
};

const setChartToggle = (chartId, key, value) => {
    setChartToggleInStore(chartId, key, value);
};

const clearViewerCaches = () => {
    // Drop responses only — keep the bootstrapped nav list. Section
    // payloads no longer embed `sections`, so it can't be recovered
    // by reloading.
    clearSectionResponses(sectionCacheState);
    heatmapDataCache.clear();
    chartsState.clear();
};

const applyMultiNodeInfo = (experimentFileMetadata = null) => {
    const previousSelection = selectedNode;

    nodeList = [];
    nodeVersions = {};
    selectedNode = null;
    serviceInstances = {};
    selectedInstances = {};

    if (!fileMetadata) {
        setSelectedNode(null);
        return;
    }

    // In compare mode, the dropdown shows the union of nodes from
    // both captures. If the user picks a node that only exists on one
    // side, that side renders empty rather than silently fanning out
    // across all nodes — see buildEffectiveQuery's node injection on
    // the cross-capture path.
    const baselineNodes = fileMetadata.nodes || [];
    const experimentNodes = experimentFileMetadata?.nodes || [];
    const seen = new Set();
    nodeList = [...baselineNodes, ...experimentNodes].filter((n) => {
        if (seen.has(n)) return false;
        seen.add(n);
        return true;
    });
    nodeVersions = fileMetadata.node_versions || {};
    serviceInstances = fileMetadata.service_instances || {};

    if (nodeList.length > 0) {
        const pinned = fileMetadata.pinned_node;
        const defaultNode = (pinned && nodeList.includes(pinned)) ? pinned : nodeList[0];
        // Preserve the user's prior selection if it survives the union
        // (e.g. attach/detach on the experiment side shouldn't reset).
        const next = (previousSelection && nodeList.includes(previousSelection))
            ? previousSelection
            : defaultNode;
        selectedNode = next;
        setSelectedNode(next);
    } else {
        setSelectedNode(null);
    }

    for (const source of Object.keys(serviceInstances)) {
        selectedInstances[source] = null;
    }
};

const loadSection = async (section) => {
    if (sectionResponseCache[section]) return sectionResponseCache[section];

    const data = await ViewerApi.getSection(section);
    if (!data) return null;

    // Pick up the GPU id list from the GPU view's metadata so the GPU selector
    // dropdown can be populated.
    const gpuSel = data.metadata?.gpu_selector;
    if (gpuSel?.enabled && Array.isArray(gpuSel.ids)) {
        gpuList = gpuSel.ids.slice();
    }

    const processedData = await processDashboardData(data, activeCgroupPattern, `/${section}`);
    return cacheSectionResponse(section, processedData);
};

const preloadSections = (allSections) => {
    // Intentional no-op: section bodies load on demand. Kept so existing
    // callers don't eagerly materialize every section payload.
    void allSections;
};

const reloadCurrentSection = async () => {
    const currentRoute = m.route.get();
    if (!currentRoute) return;
    const section = currentRoute.replace(/^\//, '');
    if (!section) return;
    // Client-only routes (the simple-capture metric browser at /source/<name>)
    // have no server-generated section payload — loadSection would fetch
    // /data/source/<name>.json and 404 on every selection change, logging an
    // error. The metric browser fetches its own catalog, so there's nothing to
    // reload here.
    if (section.startsWith('source/')) return;

    try {
        delete sectionResponseCache[section];
        const data = await loadSection(section);
        if (data?.sections) preloadSections(data.sections);
        if (heatmapEnabled && !heatmapDataCache.has(currentRoute)) {
            fetchSectionHeatmapData(currentRoute, data.groups);
        }
        m.redraw();
    } catch (e) {
        console.error('Failed to reload section after selection change:', e);
    }
};

// ── Refetch-on-zoom (display-mode drill-down) ─────────────────────────────
// Server-side decimation means the browser only holds the decimated points,
// so a zoom can't reveal per-second detail on its own — we refetch the
// narrower window at the same point budget (higher resolution), down to
// native 1s once the window fits the budget. Translate a zoom into an
// absolute [start,end] seconds window, set the range override, reset the zoom
// visual (so the refetched window shows fully), and reload the section.
let _baselineRange = null;
const baselineRange = async () => {
    if (_baselineRange) return _baselineRange;
    const meta = await ViewerApi.getMetadata().catch(() => null);
    _baselineRange = queryRangeFromMeta(meta);
    return _baselineRange;
};

let _programmaticZoom = false;
let _zoomRefetchTimer = null;
// Drill-down cancellation: the AbortController for the in-flight refetch and a
// monotonic generation. A new drill-down aborts the previous and bumps the
// generation so a superseded window's late response is discarded.
let _drillController = null;
let _drillGen = 0;

// Re-run the current section's queries for the new window WITHOUT deleting the
// section cache. reloadCurrentSection() deletes the cache, which drops the
// view's data and flashes the loading page while the whole section (spec + all
// queries) re-fetches. Here the charts stay on screen and each one updates in
// place as its own query lands (progressive render), so a zoom feels live even
// when the slower histogram queries take a moment.
const refetchCurrentSectionInPlace = async ({ signal, isStale } = {}) => {
    const currentRoute = m.route.get();
    if (!currentRoute) return;
    const section = currentRoute.replace(/^\//, '');
    const data = section && sectionResponseCache[section];
    // No cached section yet (shouldn't happen from a zoom) → fall back to the
    // full reload path rather than silently doing nothing.
    if (!data) { await reloadCurrentSection(); return; }
    try {
        // Refetch the line/scatter display data and — in heatmap mode — the
        // bucket heatmaps for the same window, concurrently. Both fetch paths
        // honor the range override, so each returns finer detail for the window.
        // The heatmap refetch writes the cache WITHOUT flipping heatmapLoading,
        // so the current (coarse) heatmap stays visible until the crisp window
        // data lands rather than flashing the section's LOADING state.
        const jobs = [processDashboardData(data, activeCgroupPattern, currentRoute, { isStale, signal })];
        if (heatmapEnabled && Array.isArray(data.groups)) {
            jobs.push(
                fetchHeatmapsForGroups(data.groups)
                    // Drop a superseded window's heatmaps: a newer zoom owns the cache.
                    .then((hd) => { if (!(isStale && isStale())) heatmapDataCache.set(currentRoute, hd); })
                    .catch((e) => console.error('Heatmap window refetch failed:', e)),
            );
        }
        await Promise.all(jobs);
        if (isStale && isStale()) return;
        // Synchronous redraw so the chart reconfigures run NOW, while
        // chartsState._zoomRefine is still true (applyDisplayWindow clears it
        // right after this returns). An async m.redraw() could fire after the
        // flag is reset, and the line/multi charts would rebuild (notMerge)
        // instead of sharpening in place.
        m.redraw.sync();
    } catch (e) {
        console.error('Failed to refetch section for zoom window:', e);
    }
};

const applyDisplayWindow = async (win) => {
    setRangeOverride(win); // { start, end } seconds, or null for full recording
    _programmaticZoom = true;
    // Clear the zoom state silently — do NOT snap every chart back out to full
    // range before the refetch lands, or the old full-range data flashes for
    // the refetch's duration (the bouncy "intermediate state"). The refetch's
    // reconfigure resets each chart's dataZoom to the new window on its own.
    chartsState.setZoom(null, { source: null, silent: true });
    _programmaticZoom = false;

    // Cancel any drill-down still in flight: abort its requests (true network
    // cancellation on the display fetch path) and bump the generation so any
    // response that still lands is discarded rather than clobbering this window.
    if (_drillController) _drillController.abort();
    _drillController = new AbortController();
    const signal = _drillController.signal;
    const gen = ++_drillGen;
    const isStale = () => gen !== _drillGen;

    // Sharpen line/multi charts in place (merge update) rather than a notMerge
    // rebuild while the narrower window refetches — see ChartsState._zoomRefine.
    chartsState._zoomRefine = true;
    try {
        await refetchCurrentSectionInPlace({ signal, isStale });
    } finally {
        // Only clear the refine flag if we're still the current drill-down; a
        // newer one owns it otherwise.
        if (!isStale()) chartsState._zoomRefine = false;
    }
};

const onZoomRefetch = async (zoom) => {
    const full = await baselineRange();
    if (!full) return;
    const cur = getRangeOverride() || full;

    // Reset (double-click / default zoom) → back to the full recording.
    if (!zoom || chartsState.isDefaultZoom()) {
        if (getRangeOverride()) await applyDisplayWindow(null);
        return;
    }

    const span = cur.end - cur.start;
    let ns;
    let ne;
    if (zoom.startValue != null && zoom.endValue != null) {
        // Absolute axis coords (ms) from a chart drag-zoom.
        ns = zoom.startValue / 1000;
        ne = zoom.endValue / 1000;
    } else if (zoom.start != null && zoom.end != null) {
        // Percentages relative to the currently-loaded window.
        ns = cur.start + (zoom.start / 100) * span;
        ne = cur.start + (zoom.end / 100) * span;
    } else {
        return;
    }
    // Ignore degenerate / non-zoom-in selections.
    if (!(ne - ns > 1) || ne - ns >= span * 0.99) return;
    await applyDisplayWindow({ start: ns, end: ne });
};

if (typeof chartsState.subscribeZoom === 'function') {
    chartsState.subscribeZoom((zoom) => {
        // Ignore our own programmatic zoom resets, and skip when display mode
        // is off (the JSON path has native data — client-side zoom is enough).
        if (_programmaticZoom || !getDisplayMode()) return;
        clearTimeout(_zoomRefetchTimer);
        _zoomRefetchTimer = setTimeout(() => { onZoomRefetch(zoom).catch(() => {}); }, 300);
    });
}

const changeNode = async (nodeName) => {
    selectedNode = nodeName;
    setSelectedNode(nodeName);
    // Service routes don't depend on the node selector — keep their
    // caches and skip reload when one is the active route.
    clearNonServiceResponses(sectionCacheState);
    for (const route of Array.from(heatmapDataCache.keys())) {
        if (!route.startsWith('/service/')) {
            heatmapDataCache.delete(route);
        }
    }
    const onServiceRoute = m.route.get()?.startsWith('/service/');
    if (!onServiceRoute) {
        chartsState.clear();
    }
    m.redraw();
    if (!onServiceRoute) {
        await reloadCurrentSection();
    }
};

const changeGpu = async (gpuIds) => {
    selectedGpus = Array.isArray(gpuIds) ? gpuIds.slice() : [];
    setSelectedGpus(selectedGpus);
    // Only the GPU section's charts depend on this; drop its cached data and
    // reload so the queries re-run with the id filter applied.
    clearNonServiceResponses(sectionCacheState);
    for (const route of Array.from(heatmapDataCache.keys())) {
        if (route === '/gpu') heatmapDataCache.delete(route);
    }
    chartsState.clear();
    m.redraw();
    await reloadCurrentSection();
};

const changeInstance = async (serviceName, instanceId) => {
    selectedInstances[serviceName] = instanceId;
    setSelectedInstance(serviceName, instanceId);
    const svcKey = `service/${serviceName}`;
    delete sectionResponseCache[svcKey];
    m.redraw();
    await reloadCurrentSection();
};

const CLIENT_ONLY_SECTIONS = new Set([
    'notebook',
    'selection',
    'report',
    'systeminfo',
    'metadata',
    'query_explorer',
]);

const changeGranularity = async (step) => {
    currentGranularity = step;
    setStepOverride(step);
    notebookStore.stepOverride = step ?? null;
    persistNotebook();

    const currentRoute = m.route.get();
    const section = currentRoute ? currentRoute.replace(/^\//, '') : '';

    // All cached section data is stale against the new step. Counter/gauge query
    // text no longer encodes the step (rewriteCounterQuery was removed once the
    // engine took over per-step rate), so the query-keyed tile cache would keep
    // serving the old step's decimated tiles — clear it explicitly.
    clearDisplayTiles();
    clearSectionResponses(sectionCacheState);
    heatmapDataCache.clear();
    // Route zoom clear through the observable setter so any charts
    // that are still alive at this point (explorers etc.) see the
    // reset via their subscription — setZoom accepts null as "no
    // zoom" and notifies subscribers with it.
    chartsState.setZoom(null, { source: null });
    chartsState.globalZoom = null;

    // Data sections refetch themselves; client-only sections (Selection,
    // Report, System Info, Metadata, Query Explorer) need overview
    // primed so the sidebar + topnav meta stays populated after the
    // cache wipe — otherwise the whole nav collapses.
    const target = !section || CLIENT_ONLY_SECTIONS.has(section) ? 'overview' : section;

    try {
        const data = await loadSection(target);
        if (data?.sections) preloadSections(data.sections);
        m.redraw();
    } catch (_) { /* keep existing view on error */ }
};

const changeTimeMode = async (mode) => {
    currentTimeMode = mode === 'raw' ? 'raw' : 'grid';
    setRateMode(currentTimeMode);

    const currentRoute = m.route.get();
    const section = currentRoute ? currentRoute.replace(/^\//, '') : '';

    // Every cached rate/irate series is stale against the new time mode. The
    // display tile cache is keyed by query text (not mode), so it MUST be
    // cleared here or already-loaded charts keep serving the old mode's tiles.
    clearDisplayTiles();
    clearSectionResponses(sectionCacheState);
    heatmapDataCache.clear();
    chartsState.setZoom(null, { source: null });
    chartsState.globalZoom = null;

    const target = !section || CLIENT_ONLY_SECTIONS.has(section) ? 'overview' : section;
    try {
        const data = await loadSection(target);
        if (data?.sections) preloadSections(data.sections);
        m.redraw();
    } catch (_) { /* keep existing view on error */ }
};

const toggleGlobalHeatmap = async (sectionRoute, groups) => {
    heatmapEnabled = !heatmapEnabled;
    const cached = heatmapDataCache.get(sectionRoute);
    if (heatmapEnabled && (!cached || cached.size === 0)) {
        await fetchSectionHeatmapData(sectionRoute, groups);
    } else {
        m.redraw();
    }
};

const fetchSectionHeatmapData = async (sectionRoute, groups) => {
    heatmapLoading = true;
    m.redraw();
    const heatmapData = await fetchHeatmapsForGroups(groups);
    heatmapDataCache.set(sectionRoute, heatmapData);
    heatmapLoading = false;
    m.redraw();
};

const topNavAttrs = (data, sectionRoute, extra) => buildTopNavAttrs({
    data,
    sectionRoute,
    chartsState,
    fileChecksum,
    liveMode,
    recording,
    onStartRecording,
    onStopRecording,
    onSaveCapture,
    onUploadParquet,
    granularity: currentGranularity,
    onGranularityChange: changeGranularity,
    timeMode: currentTimeMode,
    onTimeModeChange: changeTimeMode,
    nodeList,
    selectedNode,
    nodeVersions,
    onNodeChange: changeNode,
    gpuList,
    selectedGpus,
    gpuSelectorActive: (m.route.get() || '').startsWith('/gpu'),
    onGpuChange: changeGpu,
    extra: {
        // Default compare state so TopNav renders the badge in every
        // code path (Main.view, single-chart route, service route).
        // Callers may override via their own `extra`.
        compareMode,
        onDetachExperiment: compareMode ? () => { detachExperiment(); } : null,
        experimentFilename,
        baselineAlias,
        experimentAlias,
        onLoadBaseline: onUploadParquet ? (file) => onUploadParquet(file) : null,
        onLoadExperiment: onUploadParquet ? (file) => { loadExperiment(file); } : null,
        ...(extra || {}),
    },
});

const SectionContent = {
    view({ attrs }) {
        // No resolved section (route doesn't match a loaded section yet,
        // e.g. mid-load or after a failed fetch): render nothing rather
        // than throwing mid-render, which desyncs mithril's DOM and
        // cascades into removeChild errors.
        if (!attrs.section) return null;
        const sectionRoute = attrs.section.route;
        const sectionName = attrs.section.name;
        const interval = attrs.interval;

        if (sectionRoute !== activeSectionRoute) {
            activeSectionRoute = sectionRoute;
            // When leaving a section with a local chart zoom, snap back
            // to the most recent global zoom. Route through setZoom so
            // the new section's charts (freshly mounted, freshly
            // subscribed) also see the snap via their subscription.
            if (chartsState.zoomSource === 'local') {
                const gz = chartsState.globalZoom || { start: 0, end: 100 };
                const isDefault = gz.start === 0 && gz.end === 100;
                chartsState.setZoom(gz, { source: isDefault ? null : 'global' });
            }
            // Re-apply the current zoom to whichever charts are
            // registered after the new section's mount + echart init
            // settles. Chart.initEchart already tries to apply on its
            // own when it runs, but its IntersectionObserver is async,
            // compare-mode experiment queries are async, and the order
            // isn't guaranteed relative to Mithril's redraw cadence —
            // deferring a replay here ensures the new charts show the
            // zoom the user had in the old section. Idempotent, so
            // wasted calls against already-synced charts are harmless.
            requestAnimationFrame(() => {
                requestAnimationFrame(() => chartsState.replayZoom());
            });
        }

        if (sectionName === 'Query Explorer') {
            return m('div#section-content', [
                m(QueryExplorer, { liveMode, isRecording: () => recording }),
            ]);
        }

        if (sectionName === 'System Info') {
            return m('div#section-content', [
                m(SystemInfoView, { data: systemInfoData }),
            ]);
        }

        if (sectionName === 'Metadata') {
            return m('div#section-content', [
                m(MetadataView, { data: fileMetadata }),
            ]);
        }

        if (sectionName === 'Notebook') {
            const sectionMeta = getCachedSectionMeta(sectionResponseCache, interval);
            return m(NotebookView, {
                title: 'Notebook',
                ...sectionMeta,
                chartsState,
                fileChecksum,
                heatmapEnabled,
                heatmapLoading,
                stepOverride: currentGranularity,
                onToggleHeatmap: toggleGlobalHeatmap,
                // Compare-mode plumbing — NotebookView mirrors the
                // current viewer state, so pinning in compare mode and
                // visiting /notebook renders the compare view.
                compareMode,
                anchors: notebookStore.anchors || { baseline: 0, experiment: 0 },
                toggles: notebookStore.chartToggles || {},
                setChartToggle,
                experimentQueryRange,
                baselineAlias,
                experimentAlias,
                experimentFilename,
                combinedAB,
            });
        }

        if (sectionName === 'Selection') {
            const sectionMeta = getCachedSectionMeta(sectionResponseCache, interval);
            return m(LoadedSelectionView, {
                title: 'Selection',
                ...sectionMeta,
                chartsState,
                stepOverride: currentGranularity,
                compareMode,
                experimentQueryRange,
                baselineAlias,
                experimentAlias,
            });
        }

        if (sectionName === 'Report') {
            const sectionMeta = getCachedSectionMeta(sectionResponseCache, interval);
            return m(ReportView, {
                title: 'Report',
                ...sectionMeta,
                chartsState,
                fileChecksum,
                heatmapEnabled,
                heatmapLoading,
                stepOverride: currentGranularity,
                onToggleHeatmap: toggleGlobalHeatmap,
                compareMode,
                experimentQueryRange,
                baselineAlias,
                experimentAlias,
            });
        }

        if (sectionRoute.startsWith('/source/')) {
            const srcName = sectionRoute.slice('/source/'.length);
            // Run one plot's query through the same pipeline the dashboard
            // uses so the plot spec is populated identically (data,
            // _resolvedStyle) and Group gives it a title + working
            // heatmap/spectrum controls. processDashboardData mutates the
            // plot in place; we wrap it in a throwaway one-group payload.
            const runQuery = (plot) => processDashboardData(
                { groups: [{ name: '', subgroups: [{ name: null, plots: [plot] }] }] },
                null,
                sectionRoute,
            );
            return m('div#section-content', [
                m(MetricBrowserView(srcName), {
                    sourceName: srcName,
                    interval,
                    Group,
                    sectionRoute,
                    runQuery,
                    Chart,
                    chartsState,
                }),
            ]);
        }

        if (sectionRoute.startsWith('/service/')) {
            const svcName = sectionRoute.replace('/service/', '');
            return renderServiceSection(attrs, Group, sectionRoute, sectionName, interval, {
                instances: serviceInstances[svcName] || [],
                selectedInstance: selectedInstances[svcName] || null,
                onInstanceChange: (id) => changeInstance(svcName, id),
            });
        }

        const { withData } = countCharts(attrs.groups);
        const titleText = `${sectionName} (${withData})`;

        if (attrs.section.route === '/cgroups') {
            return renderCgroupSection({
                attrs,
                titleText,
                interval,
                chartsState,
                Chart,
                CgroupSelector,
                executePromQLRangeQuery: (query, ...args) => {
                    const node = getSelectedNode();
                    if (node) query = injectLabel(query, 'node', node);
                    return executePromQLRangeQuery(query, ...args);
                },
                applyResultToPlot,
                substituteCgroupPattern,
                setActiveCgroupPattern: (p) => { activeCgroupPattern = p; },
                globalColorMapper,
            });
        }

        const hasSelection = chartsState.hasActiveSelection();

        const hasHistogramCharts = (attrs.groups || []).some(g =>
            collectGroupPlots(g).some(p => isHistogramPlot(p))
        );

        const unavailableCharts = attrs.metadata?.unavailable_charts || [];

        return m('div#section-content', [
            m('div.section-header-row', [
                m('h1.section-title', titleText),
                m('div.section-actions', [
                    hasSelection && m('button.section-action-btn', {
                        onclick: () => {
                            chartsState.resetAll();
                            m.redraw();
                        },
                    }, 'RESET SELECTION'),
                    // Hide the percentile↔heatmap toggle in compare
                    // mode — the compare adapter's histogram-heatmap
                    // side-by-side path doesn't yet populate experiment
                    // bucket data through fetchHeatmapsForGroups, so
                    // toggling renders the baseline heatmap next to an
                    // empty experiment pane. Re-enable when that gap
                    // is closed.
                    !compareMode && m('button.section-action-btn', {
                        onclick: () => toggleGlobalHeatmap(sectionRoute, attrs.groups),
                        disabled: heatmapLoading || !hasHistogramCharts,
                    }, heatmapLoading ? 'LOADING...' : (heatmapEnabled ? 'SHOW PERCENTILES' : 'SHOW HEATMAPS')),
                ]),
            ]),
            // GPU section: a two-panel selector (matching the cgroups UI) that
            // filters the non-per-GPU charts to a subset of GPU ids. Shown only
            // when the recording has more than one GPU. Per-GPU charts (queries
            // grouped `by (id)`) always show all GPUs.
            sectionRoute === '/gpu' && gpuList.length > 1 && m(GpuSelector, {
                ids: gpuList,
                selected: selectedGpus,
                onChange: changeGpu,
                // GPU details (name/model, memory) from System Info, keyed by id,
                // so each entry shows the device model alongside its id.
                gpus: systemInfoData?.gpus || [],
            }),
            m('div#groups',
                attrs.groups.map((group) => m(Group, { ...group, sectionRoute, sectionName, interval })),
            ),
            renderSectionNotes({
                title: 'Charts with no data',
                lead: 'The following charts have no matching data in this recording:',
                items: unavailableCharts,
                formatItem: (c) => m('li', [
                    m('strong', c.title),
                    c.subgroup ? ` — ${c.subgroup}` : '',
                    c.group ? ` (${c.group})` : '',
                ]),
            }),
        ]);
    },
};

const systemInfoSection = { name: 'System Info', route: '/systeminfo' };
const metadataSection = { name: 'Metadata', route: '/metadata' };
const notebookSection = { name: 'Notebook', route: '/notebook' };
const selectionSection = { name: 'Selection', route: '/selection' };
const reportSection = { name: 'Report', route: '/report' };

const bootstrapCacheIfNeeded = () => {
    if (Object.keys(sectionResponseCache).length > 0) return;

    loadSection('overview').then((data) => {
        if (data?.sections) preloadSections(data.sections);
        m.redraw();
    }).catch(() => {});
};

const initDashboard = (config = {}) => {
    initTheme();
    initComponents();

    systemInfoData = config.systemInfo || null;
    fileChecksum = config.fileChecksum || null;
    fileMetadata = config.fileMetadata || null;

    // Compare-mode initial state (supplied by bootstrap when /api/v1/mode
    // reported compare_mode=true).
    compareMode = config.compareMode === true;
    experimentAttached = compareMode;
    combinedAB = config.combinedAB === true;
    reportMode = config.reportMode === true;
    experimentSystemInfo = config.experimentSystemInfo || null;
    experimentDurationMs = durationFromFileMetadata(config.experimentFileMetadata);
    experimentQueryRange = config.experimentQueryRange || null;
    experimentFilename = config.experimentFilename
        || (config.experimentFileMetadata
            && (config.experimentFileMetadata.filename || config.experimentFileMetadata.file_name))
        || null;
    baselineAlias = config.baselineAlias || null;
    experimentAlias = config.experimentAlias || null;

    if (config.selectionPayload && Array.isArray(config.selectionPayload.entries)) {
        loadPayloadIntoStore(reportStore, config.selectionPayload);
        reportStore.loadedFrom = 'embedded report';
    }

    // Restore step override from whichever store carries one. Report
    // (loaded-from-parquet) wins when both exist so a shared annotated
    // parquet shows the granularity its author intended.
    const restoredStep =
        reportStore.stepOverride ?? notebookStore.stepOverride ?? null;
    if (restoredStep != null) {
        currentGranularity = restoredStep;
        setStepOverride(restoredStep);
    }

    applyMultiNodeInfo(config.experimentFileMetadata);

    liveMode = config.liveMode || false;
    recording = config.recording !== undefined ? config.recording : false;
    onStartRecording = config.onStartRecording || null;
    onStopRecording = config.onStopRecording || null;
    onSaveCapture = config.onSaveCapture || null;
    onUploadParquet = config.onUploadParquet || null;
    onRefresh = config.onRefresh || null;

    Main = createMainComponent({
        TopNav,
        Sidebar,
        SaveModal,
        SectionContent,
        sectionResponseCache,
        getHasSystemInfo: () => systemInfoData,
        getHasFileMetadata: () => fileMetadata && Object.keys(fileMetadata).length > 0,
        getCompareBadgeAttrs: () => ({
            compareMode,
            // Baseline filename comes from TopNav's existing attrs.filename.
            experimentFilename,
            baselineAlias,
            experimentAlias,
            // The WASM viewer has no onUploadParquet handler — that path
            // is how the site viewer loads its initial parquet on its own.
            // Use its absence as the "WASM mode" signal and hide both
            // per-capture Load buttons there. Server viewer always has it.
            onLoadBaseline: onUploadParquet ? (file) => onUploadParquet(file) : null,
            onLoadExperiment: onUploadParquet ? (file) => { loadExperiment(file); } : null,
        }),
        buildAttrs: topNavAttrs,
    });

    if (liveMode && onRefresh) {
        liveRefreshInterval = setInterval(onRefresh, 5000);
    }

    // When the capture carries a service extension, default to that
    // service's section instead of the generic overview — the service
    // KPIs are usually what the user came to look at. In category mode
    // the canonical section is
    // the category itself (e.g. `/service/inference-library`); the
    // per-member sections from `serviceInstances` don't exist in the
    // rendered map at all.
    const categoryName = config.categoryName || null;
    const serviceNames = Object.keys(serviceInstances || {});
    // Simple-capture fallback: a foreign source has no Overview/built-in
    // sections, only `/source/<name>` entries. Land on the first one so
    // the user doesn't hit an empty `/overview` that this capture never
    // emitted. Rezolus/service files keep their existing landing.
    const bootstrapSections = getCachedSections();
    const hasOverview = bootstrapSections.some((s) => s.route === '/overview');
    const firstSourceRoute = bootstrapSections.find(
        (s) => s.route && s.route.startsWith('/source/'),
    )?.route;
    const defaultRoute = reportMode
        ? '/report'
        : (categoryName
            ? `/service/${categoryName}`
            : (serviceNames.length > 0
                ? `/service/${serviceNames[0]}`
                : (hasOverview
                    ? '/overview'
                    : (firstSourceRoute || '/overview'))));

    // A stale hash (e.g. `#/service/llm-perf` from a previous session
    // or external link to a different capture) would otherwise drive
    // mithril to a route whose data fetch throws "Unknown section" and
    // surfaces as a confusing error. Build the set of service names
    // that actually have a section in this load; if the hash points at
    // anything else, drop it so `defaultRoute` kicks in.
    const validServiceNames = new Set();
    if (categoryName) {
        validServiceNames.add(categoryName);
    } else {
        for (const name of serviceNames) validServiceNames.add(name);
        const expServiceInstances = config.experimentFileMetadata?.service_instances;
        if (expServiceInstances) {
            for (const name of Object.keys(expServiceInstances)) {
                validServiceNames.add(name);
            }
        }
    }
    {
        const hash = window.location.hash || '';
        if (hash.startsWith('#/service/')) {
            const tail = hash.slice('#/service/'.length);
            const requestedSvc = decodeURIComponent(tail.split('/')[0] || '');
            if (!validServiceNames.has(requestedSvc)) {
                window.location.hash = '';
            }
        }
    }

    m.route.prefix = '#';
    m.route(document.body, defaultRoute, {
        '/:section/chart/:chartId': {
            onmatch(params) {
                const sectionKey = params.section;
                const makeSingleChartView = () => ({
                    view() {
                        const data = sectionResponseCache[sectionKey];
                        const activeSection = getCachedSections()
                            .find(s => s.route === `/${sectionKey}`);
                        if (!data) {
                            return m('div#splash', m('div.card', [
                                m('h1', activeSection?.name || 'Loading'),
                                m('p.subtitle', 'Loading…'),
                                m('div.progress-bar',
                                    m('div.progress-fill.indeterminate'),
                                ),
                            ]));
                        }
                        return m('div', [
                            m(TopNav, topNavAttrs(data, activeSection?.route)),
                            m('main.single-chart-main', [
                                m(SingleChartView, {
                                    data,
                                    chartId: decodeURIComponent(params.chartId),
                                    applyResultToPlot,
                                }),
                            ]),
                        ]);
                    },
                });

                // Resolve synchronously regardless of cache state — see
                // the matching comment in '/:section' below for rationale.
                if (!sectionResponseCache[sectionKey]) {
                    loadSection(sectionKey)
                        .then(() => m.redraw())
                        .catch(() => {});
                }
                return makeSingleChartView();
            },
        },
        ...createServiceRoutes({
            sectionResponseCache,
            loadSection,
            preloadSections,
            chartsState,
            Main,
            TopNav,
            topNavAttrs,
            SingleChartView,
            applyResultToPlot,
            getCompareMode: () => compareMode,
            getSections: getCachedSections,
            withSharedSections: withCachedSections,
            getDefaultRoute: () => defaultRoute,
        }),
        // Foreign-source ("simple capture") sections + their expanded single
        // charts. Unlike built-in/service sections these carry no
        // server-rendered groups — the MetricBrowser fetches its own catalog
        // and runs per-metric queries client-side — so the chart route
        // reconstructs one chart from the catalog. See features/source_routes.js.
        ...createSourceRoutes({
            sectionResponseCache,
            ViewerApi,
            processDashboardData,
            applyResultToPlot,
            SingleChartView,
            TopNav,
            topNavAttrs,
            Main,
            getSections: getCachedSections,
            getCompareMode: () => compareMode,
            chartsState,
        }),
        '/about': {
            render() {
                return m('div', { style: 'display:flex;align-items:center;justify-content:center;min-height:100vh;padding:2rem' },
                    m('div.card', [
                        m('h1', 'Rezolus'),
                        m('div.version', liveMode ? 'Live Mode' : 'Viewer'),
                        m('p.subtitle', 'High-resolution systems performance telemetry agent.'),
                        m('div.link-row', [
                            m('a', { href: 'https://rezolus.com' }, 'Website'),
                            m('a', { href: 'https://github.com/iopsystems/rezolus' }, 'GitHub'),
                            m('a', { href: '#!/overview' }, 'Dashboard'),
                        ]),
                    ]),
                );
            },
        },
        '/:section': {
            onmatch(params, requestedPath) {
                if (m.route.get() === requestedPath) {
                    return new Promise(function () {});
                }

                if (requestedPath !== m.route.get()) {
                    chartsState.charts.clear();
                    if (params.section !== 'cgroups') {
                        activeCgroupPattern = null;
                    }
                    window.scrollTo(0, 0);
                }

                updateDocumentTitle(params.section);

                if (params.section === 'systeminfo') {
                    bootstrapCacheIfNeeded();
                    return buildClientOnlySectionView(
                        Main,
                        sectionResponseCache,
                        getCachedSections,
                        systemInfoSection,
                        () => compareMode,
                    );
                }

                if (params.section === 'metadata') {
                    bootstrapCacheIfNeeded();
                    return buildClientOnlySectionView(
                        Main,
                        sectionResponseCache,
                        getCachedSections,
                        metadataSection,
                        () => compareMode,
                    );
                }

                if (params.section === 'notebook') {
                    bootstrapCacheIfNeeded();
                    return buildClientOnlySectionView(
                        Main,
                        sectionResponseCache,
                        getCachedSections,
                        notebookSection,
                        () => compareMode,
                    );
                }

                if (params.section === 'selection') {
                    bootstrapCacheIfNeeded();
                    return buildClientOnlySectionView(
                        Main,
                        sectionResponseCache,
                        getCachedSections,
                        selectionSection,
                        () => compareMode,
                    );
                }

                if (params.section === 'report') {
                    bootstrapCacheIfNeeded();
                    return buildClientOnlySectionView(
                        Main,
                        sectionResponseCache,
                        getCachedSections,
                        reportSection,
                        () => compareMode,
                    );
                }

                const cachedView = (sectionKey, path) => ({
                    view() {
                        const data = sectionResponseCache[sectionKey];
                        const activeSection = getCachedSections().find(
                            (section) => section.route === path,
                        );
                        if (!data) {
                            // Cache miss: the synchronous route resolution
                            // above already unmounted the previous section's
                            // chart canvases. Show a splash/progress bar
                            // styled the same as the initial-load splash so
                            // the user gets clear in-flight feedback until
                            // loadSection settles and triggers a redraw.
                            return m('div#splash', m('div.card', [
                                m('h1', activeSection?.name || 'Loading'),
                                m('p.subtitle', 'Loading…'),
                                m('div.progress-bar',
                                    m('div.progress-fill.indeterminate'),
                                ),
                            ]));
                        }
                        return m(Main, {
                            ...withCachedSections(data),
                            activeSection,
                            compareMode,
                        });
                    },
                });

                // Resolve the route synchronously even on a cache miss so
                // the old section's DOM unmounts immediately (firing each
                // chart's onremove → echart.dispose()). cachedView falls
                // back to a "Loading…" placeholder while data is in flight,
                // which gets replaced by the populated view via m.redraw()
                // once loadSection settles.
                if (!sectionResponseCache[params.section]) {
                    loadSection(params.section)
                        .then((data) => {
                            if (data?.sections) preloadSections(data.sections);
                            if (heatmapEnabled && !heatmapDataCache.has(requestedPath)) {
                                fetchSectionHeatmapData(requestedPath, data.groups);
                            }
                            m.redraw();
                        })
                        .catch((err) => {
                            // Stale URL pointing at a missing section. Drop
                            // back to the dashboard's default route instead
                            // of letting the "Unknown section" error bubble.
                            // If defaultRoute itself points at the failing
                            // section (can happen when serviceInstances and
                            // dashboard_sections disagree on naming), fall
                            // through to /overview to avoid bouncing into
                            // the same broken route.
                            console.warn(`[viewer] section ${params.section} not available; redirecting`, err);
                            const failingRoute = `/${params.section}`;
                            const target = defaultRoute === failingRoute ? '/overview' : defaultRoute;
                            if (target && target !== m.route.get()) {
                                m.route.set(target);
                            }
                        });
                } else if (heatmapEnabled && !heatmapDataCache.has(requestedPath)) {
                    fetchSectionHeatmapData(requestedPath, sectionResponseCache[params.section].groups);
                }
                return cachedView(params.section, requestedPath);
            },
        },
    });

    // Initial title refresh — m.route's onmatch may resolve before
    // fileMetadata is set on first load, and same-path reload (Load
    // Parquet) suppresses onmatch entirely. Force-refresh once
    // fileMetadata is in hand.
    const currentRoute = (m.route.get() || '').split('/')[1] || '';
    updateDocumentTitle(currentRoute);
};

// Double-click anywhere resets zoom and clears all pin selections
document.addEventListener('dblclick', () => {
    if (!chartsState.isDefaultZoom() || chartsState.charts.size > 0) {
        chartsState.resetAll();
        m.redraw();
    }
});

const getHeatmapEnabled = () => heatmapEnabled;
const getActiveCgroupPattern = () => activeCgroupPattern;
const getRecording = () => recording;
const setRecording = (value) => { recording = value; };

export { initDashboard, sectionResponseCache, cacheSectionResponse, bootstrapSharedSections, clearViewerCaches, chartsState, loadSection, preloadSections, getHeatmapEnabled, heatmapDataCache, fetchSectionHeatmapData, getActiveCgroupPattern, getRecording, setRecording, attachExperiment, detachExperiment, durationFromFileMetadata, setChartToggle };
