// app.js — Shared dashboard logic for both server and WASM viewers.
// Exports initDashboard(config) which sets up state and mounts the Mithril router.

import { ChartsState, Chart } from './charts/chart.js';
import { QueryExplorer, SingleChartView } from './explorers.js';
import { CgroupSelector } from './cgroup_selector.js';
import globalColorMapper from './charts/util/colormap.js';
import { TopNav, Sidebar, countCharts, formatSize } from './layout.js';
import { collectGroupPlots } from './group_utils.js';
import { CpuTopology } from './topology.js';
import { executePromQLRangeQuery, applyResultToPlot, fetchHeatmapsForGroups, substituteCgroupPattern, processDashboardData, clearMetadataCache, setStepOverride, getStepOverride, setSelectedNode, setSelectedInstance, getSelectedNode, injectLabel, CAPTURE_EXPERIMENT } from './data.js';
import { reportStore, selectionStore, persistSelection, setStorageScope, loadPayloadIntoStore, SelectionView, ReportView, setChartToggle as setChartToggleInStore, setAnchor } from './selection.js';
import { SaveModal } from './overlays.js';
import { ViewerApi } from './viewer_api.js';
import { createSystemInfoView, createMetadataView, renderCgroupSection } from './section_views.js';
import { buildTopNavAttrs, createMainComponent } from './navigation.js';
import { initTheme } from './theme.js';
import { isHistogramPlot } from './charts/metric_types.js';
import { renderServiceSection, createServiceRoutes } from './service.js';
import { createGroupComponent, getCachedSectionMeta, buildClientOnlySectionView } from './viewer_core.js';
import {
    createSectionCacheState,
    storeSectionResponse,
    storeSharedSections,
    getSections,
    withSharedSections,
    clearSectionResponses,
    resetSectionCacheState,
    setSectionCacheLimit,
    pinSectionKey,
} from './section_cache.js';

// ── State ──────────────────────────────────────────────────────────

let activeSectionRoute = null;
let systemInfoData = null;
let fileChecksum = null;
let fileMetadata = null;
let nodeList = [];
let nodeVersions = {};
let selectedNode = null;
let serviceInstances = {};
let selectedInstances = {};
let activeCgroupPattern = null;
let heatmapEnabled = false;
let heatmapLoading = false;
const heatmapDataCache = new Map();
const chartsState = new ChartsState();
let currentGranularity = null;
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

// Compare-mode state (Stage 4 of A/B compare plan)
let compareMode = false;
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

// Compare-mode per-chart toggles + anchors live in `selectionStore` so
// they persist across page reloads. See selection_migration.js for the
// schema. The accessors below read-through to the store.

// Config-driven state (set by initDashboard)
let liveMode = false;
let recording = false;
let onStartRecording = null;
let onStopRecording = null;
let onSaveCapture = null;
let onUploadParquet = null;
let onRefresh = null;
let liveRefreshInterval = null;

// ── Components (initialized once) ──────────────────────────────────

let Main;
let SystemInfoView;
let MetadataView;
let Group;

const initComponents = () => {
    Group = createGroupComponent(() => ({
        chartsState, heatmapEnabled, heatmapLoading, heatmapDataCache,
        compareMode,
        toggles: selectionStore.chartToggles || {},
        setChartToggle,
        anchors: selectionStore.anchors || { baseline: 0, experiment: 0 },
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

// ── Helpers ────────────────────────────────────────────────────────

// Extract a capture's duration (milliseconds) from its file metadata.
// Tries the structured field first; falls back to max_time - min_time when
// present. Returns null when neither is available.
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

// Parse /api/v1/metadata response shape into a compare-mode query range.
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
    return { start, end, step: Math.max(1, Math.floor((end - start) / 500)) };
};

// ── Compare-mode actions ───────────────────────────────────────────

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

    // Refresh the multi-node dropdown with the union of baseline +
    // experiment nodes. Preserves the user's prior node selection when
    // it still exists in the union.
    applyMultiNodeInfo(expFileMeta);
    clearViewerCaches();

    // Clamp a stale anchor when the newly-attached experiment is
    // shorter than the previously-saved offset. Avoids a chart starting
    // past the end of its data.
    const anchors = selectionStore.anchors || { baseline: 0, experiment: 0 };
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

    // Drop experiment-only nodes from the dropdown.
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

// Thin passthrough to selection.js; kept because it reads cleanly at
// the call sites in createGroupComponent.
const setChartToggle = (chartId, key, value) => {
    setChartToggleInStore(chartId, key, value);
};

const clearViewerCaches = () => {
    resetSectionCacheState(sectionCacheState);
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

// ── Section loading ────────────────────────────────────────────────

const loadSection = async (section) => {
    if (sectionResponseCache[section]) return sectionResponseCache[section];

    const data = await ViewerApi.getSection(section);
    if (!data) return null;

    const processedData = await processDashboardData(data, activeCgroupPattern, `/${section}`);
    return cacheSectionResponse(section, processedData);
};

const preloadSections = (allSections) => {
    // Section bodies now load on demand. Keep this hook as a no-op so
    // existing callers don't eagerly materialize every section payload.
    void allSections;
};

const reloadCurrentSection = async () => {
    const currentRoute = m.route.get();
    if (!currentRoute) return;
    const section = currentRoute.replace(/^\//, '');
    if (!section) return;

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

const changeNode = async (nodeName) => {
    selectedNode = nodeName;
    setSelectedNode(nodeName);
    clearViewerCaches();
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
    'selection',
    'report',
    'systeminfo',
    'metadata',
    'query_explorer',
]);

const changeGranularity = async (step) => {
    currentGranularity = step;
    setStepOverride(step);
    selectionStore.stepOverride = step ?? null;
    persistSelection();

    const currentRoute = m.route.get();
    const section = currentRoute ? currentRoute.replace(/^\//, '') : '';

    // All cached section data is stale against the new step.
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

// ── Heatmap ────────────────────────────────────────────────────────

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

// ── TopNav builder ─────────────────────────────────────────────────

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
    nodeList,
    selectedNode,
    nodeVersions,
    onNodeChange: changeNode,
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

// ── SectionContent component ───────────────────────────────────────

const SectionContent = {
    view({ attrs }) {
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

        if (sectionName === 'Selection') {
            const sectionMeta = getCachedSectionMeta(sectionResponseCache, interval);
            return m(SelectionView, {
                title: 'Selection',
                ...sectionMeta,
                chartsState,
                fileChecksum,
                heatmapEnabled,
                heatmapLoading,
                stepOverride: currentGranularity,
                onToggleHeatmap: toggleGlobalHeatmap,
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
            });
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
            m('div#groups',
                attrs.groups.map((group) => m(Group, { ...group, sectionRoute, sectionName, interval })),
            ),
        ]);
    },
};

// ── Synthetic sections ─────────────────────────────────────────────

const systemInfoSection = { name: 'System Info', route: '/systeminfo' };
const metadataSection = { name: 'Metadata', route: '/metadata' };
const selectionSection = { name: 'Selection', route: '/selection' };
const reportSection = { name: 'Report', route: '/report' };

const bootstrapCacheIfNeeded = () => {
    if (Object.keys(sectionResponseCache).length > 0) return;

    loadSection('overview').then((data) => {
        if (data?.sections) preloadSections(data.sections);
        m.redraw();
    }).catch(() => {});
};

// ── initDashboard ──────────────────────────────────────────────────

const initDashboard = (config = {}) => {
    initTheme();
    initComponents();

    // Apply pre-fetched state
    systemInfoData = config.systemInfo || null;
    fileChecksum = config.fileChecksum || null;
    fileMetadata = config.fileMetadata || null;

    // Compare-mode initial state (supplied by bootstrap when /api/v1/mode
    // reported compare_mode=true).
    compareMode = config.compareMode === true;
    experimentAttached = compareMode;
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
        reportStore.stepOverride ?? selectionStore.stepOverride ?? null;
    if (restoredStep != null) {
        currentGranularity = restoredStep;
        setStepOverride(restoredStep);
    }

    applyMultiNodeInfo(config.experimentFileMetadata);

    // Apply capabilities
    liveMode = config.liveMode || false;
    recording = config.recording !== undefined ? config.recording : false;
    onStartRecording = config.onStartRecording || null;
    onStopRecording = config.onStopRecording || null;
    onSaveCapture = config.onSaveCapture || null;
    onUploadParquet = config.onUploadParquet || null;
    onRefresh = config.onRefresh || null;

    // Build Main component
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

    // Start live refresh if applicable
    if (liveMode && onRefresh) {
        liveRefreshInterval = setInterval(onRefresh, 5000);
    }

    // Mount router with hash-based routing. When the capture carries a
    // service extension, default to that service's section instead of
    // the generic overview — the service KPIs are usually what the
    // user came to look at. In category mode the canonical section is
    // the category itself (e.g. `/service/inference-library`); the
    // per-member sections from `serviceInstances` don't exist in the
    // rendered map at all.
    const categoryName = config.categoryName || null;
    const serviceNames = Object.keys(serviceInstances || {});
    const defaultRoute = categoryName
        ? `/service/${categoryName}`
        : (serviceNames.length > 0 ? `/service/${serviceNames[0]}` : '/overview');

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
};

// Double-click anywhere resets zoom and clears all pin selections
document.addEventListener('dblclick', () => {
    if (!chartsState.isDefaultZoom() || chartsState.charts.size > 0) {
        chartsState.resetAll();
        m.redraw();
    }
});

// Getter/setter functions for mutable state shared with stubs
const getHeatmapEnabled = () => heatmapEnabled;
const getActiveCgroupPattern = () => activeCgroupPattern;
const getRecording = () => recording;
const setRecording = (value) => { recording = value; };

export { initDashboard, sectionResponseCache, cacheSectionResponse, bootstrapSharedSections, clearViewerCaches, chartsState, loadSection, preloadSections, getHeatmapEnabled, heatmapDataCache, fetchSectionHeatmapData, getActiveCgroupPattern, getRecording, setRecording, attachExperiment, detachExperiment, durationFromFileMetadata, setChartToggle };
