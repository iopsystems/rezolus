// app.js — Shared dashboard logic for both server and WASM viewers.
// Exports initDashboard(config) which sets up state and mounts the Mithril router.

import { ChartsState, Chart } from './charts/chart.js';
import { QueryExplorer, SingleChartView } from './explorers.js';
import { CgroupSelector } from './cgroup_selector.js';
import globalColorMapper from './charts/util/colormap.js';
import { TopNav, Sidebar, countCharts, formatSize } from './layout.js';
import { collectGroupPlots } from './group_utils.js';
import { CpuTopology, CompareBanner } from './topology.js';
import { executePromQLRangeQuery, applyResultToPlot, fetchHeatmapsForGroups, substituteCgroupPattern, processDashboardData, clearMetadataCache, setStepOverride, getStepOverride, setSelectedNode, setSelectedInstance, getSelectedNode, injectLabel } from './data.js';
import { reportStore, selectionStore, persistSelection, setStorageScope, loadPayloadIntoStore, SelectionView, ReportView, setChartToggle as setChartToggleInStore, setAnchor } from './selection.js';
import { SaveModal } from './overlays.js';
import { ViewerApi } from './viewer_api.js';
import { createSystemInfoView, createMetadataView, renderCgroupSection } from './section_views.js';
import { buildTopNavAttrs, createMainComponent } from './navigation.js';
import { initTheme } from './theme.js';
import { isHistogramPlot } from './charts/metric_types.js';
import { renderServiceSection, createServiceRoutes } from './service.js';
import { createGroupComponent, getCachedSectionMeta, buildClientOnlySectionView } from './viewer_core.js';

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
const sectionResponseCache = {};

// Compare-mode state (Stage 4 of A/B compare plan)
let compareMode = false;
let experimentAttached = false;
let experimentSystemInfo = null;
let experimentDurationMs = null;
let baselineDurationMs = null;

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

// ── Compare-mode actions ───────────────────────────────────────────

const attachExperiment = async (file) => {
    const sysinfo = await ViewerApi.attachExperiment(file);
    let expFileMeta = null;
    try { expFileMeta = await ViewerApi.getFileMetadata('experiment'); }
    catch (_) { /* optional */ }
    experimentSystemInfo = sysinfo || null;
    experimentDurationMs = durationFromFileMetadata(expFileMeta);
    experimentAttached = true;
    compareMode = true;

    // Clamp a stale anchor when the newly-attached experiment is
    // shorter than the previously-saved offset. Avoids a chart starting
    // past the end of its data.
    const anchors = selectionStore.anchors || { baseline: 0, experiment: 0 };
    if (experimentDurationMs != null && anchors.experiment > experimentDurationMs) {
        setAnchor('experiment', experimentDurationMs);
        console.info(
            `[compare] experiment anchor clamped to ${experimentDurationMs}ms to fit capture duration`,
        );
    }

    m.redraw();
};

const detachExperiment = async () => {
    try { await ViewerApi.detachExperiment(); } catch (_) { /* best effort */ }
    experimentSystemInfo = null;
    experimentDurationMs = null;
    experimentAttached = false;
    compareMode = false;
    m.redraw();
};

const getCompareState = () => ({
    compareMode,
    experimentAttached,
    experimentSystemInfo,
    experimentDurationMs,
    baselineDurationMs,
});

// Chart-toggle accessors for compare mode (e.g. heatmap `diff` flag).
// State lives in `selectionStore.chartToggles` so it persists across
// reloads and rides along with exported/annotated selections. These
// accessors are thin passthroughs to selection.js.
const getChartToggles = () => selectionStore.chartToggles || {};
const setChartToggle = (chartId, key, value) => {
    setChartToggleInStore(chartId, key, value);
};

const clearViewerCaches = () => {
    Object.keys(sectionResponseCache).forEach((k) => delete sectionResponseCache[k]);
    heatmapDataCache.clear();
    chartsState.clear();
};

const applyMultiNodeInfo = () => {
    nodeList = [];
    nodeVersions = {};
    selectedNode = null;
    serviceInstances = {};
    selectedInstances = {};

    if (!fileMetadata) return;

    nodeList = fileMetadata.nodes || [];
    nodeVersions = fileMetadata.node_versions || {};
    serviceInstances = fileMetadata.service_instances || {};

    if (nodeList.length > 0) {
        const pinned = fileMetadata.pinned_node;
        const defaultNode = (pinned && nodeList.includes(pinned)) ? pinned : nodeList[0];
        selectedNode = defaultNode;
        setSelectedNode(defaultNode);
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
    sectionResponseCache[section] = processedData;
    return processedData;
};

const preloadSections = (allSections) => {
    for (const section of allSections) {
        const key = section.route.substring(1);
        if (!sectionResponseCache[key]) {
            // Skip preloading in live mode — data flows dynamically
            if (liveMode) continue;
            loadSection(key).then(() => m.redraw()).catch(() => {});
        }
    }
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
    for (const key of Object.keys(sectionResponseCache)) {
        delete sectionResponseCache[key];
    }
    heatmapDataCache.clear();
    chartsState.zoomLevel = null;
    chartsState.zoomSource = null;
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
        // code path (Main.view, single-chart route). Callers may
        // override via their own `extra`.
        compareMode,
        onDetachExperiment: compareMode ? () => { detachExperiment(); } : null,
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
            if (chartsState.zoomSource === 'local') {
                const gz = chartsState.globalZoom || { start: 0, end: 100 };
                chartsState.zoomLevel = gz;
                chartsState.zoomSource = gz.start === 0 && gz.end === 100 ? null : 'global';
            }
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
                    m('button.section-action-btn', {
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
    baselineDurationMs = durationFromFileMetadata(fileMetadata);

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

    applyMultiNodeInfo();

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
        CompareBanner,
        sectionResponseCache,
        getHasSystemInfo: () => systemInfoData,
        getHasFileMetadata: () => fileMetadata && Object.keys(fileMetadata).length > 0,
        getBaselineSysinfo: () => systemInfoData,
        getExperimentSysinfo: () => experimentSystemInfo,
        getCompareBadgeAttrs: () => ({
            compareMode,
            onDetachExperiment: () => { detachExperiment(); },
        }),
        buildAttrs: topNavAttrs,
    });

    // Start live refresh if applicable
    if (liveMode && onRefresh) {
        liveRefreshInterval = setInterval(onRefresh, 5000);
    }

    // Mount router with hash-based routing
    m.route.prefix = '#';
    m.route(document.body, '/overview', {
        '/:section/chart/:chartId': {
            onmatch(params) {
                const sectionKey = params.section;
                const makeSingleChartView = () => ({
                    view() {
                        const data = sectionResponseCache[sectionKey];
                        if (!data) return m('div', 'Loading...');
                        const activeSection = data.sections.find(s => s.route === `/${sectionKey}`);
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

                if (sectionResponseCache[sectionKey]) {
                    return makeSingleChartView();
                }

                return loadSection(sectionKey).then(() => makeSingleChartView());
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
                    return buildClientOnlySectionView(Main, sectionResponseCache, systemInfoSection, () => compareMode);
                }

                if (params.section === 'metadata') {
                    bootstrapCacheIfNeeded();
                    return buildClientOnlySectionView(Main, sectionResponseCache, metadataSection, () => compareMode);
                }

                if (params.section === 'selection') {
                    bootstrapCacheIfNeeded();
                    return buildClientOnlySectionView(Main, sectionResponseCache, selectionSection, () => compareMode);
                }

                if (params.section === 'report') {
                    bootstrapCacheIfNeeded();
                    return buildClientOnlySectionView(Main, sectionResponseCache, reportSection, () => compareMode);
                }

                const cachedView = (sectionKey, path) => ({
                    view() {
                        const data = sectionResponseCache[sectionKey];
                        if (!data) return m('div', 'Loading...');
                        const activeSection = data.sections.find(
                            (section) => section.route === path,
                        );
                        return m(Main, { ...data, activeSection, compareMode });
                    },
                });

                if (sectionResponseCache[params.section]) {
                    if (heatmapEnabled && !heatmapDataCache.has(requestedPath)) {
                        fetchSectionHeatmapData(requestedPath, sectionResponseCache[params.section].groups);
                    }
                    return cachedView(params.section, requestedPath);
                }

                return loadSection(params.section).then((data) => {
                    if (data?.sections) preloadSections(data.sections);
                    if (heatmapEnabled && !heatmapDataCache.has(requestedPath)) {
                        fetchSectionHeatmapData(requestedPath, data.groups);
                    }
                    return cachedView(params.section, requestedPath);
                });
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

export { initDashboard, sectionResponseCache, clearViewerCaches, chartsState, loadSection, preloadSections, getHeatmapEnabled, heatmapDataCache, fetchSectionHeatmapData, getActiveCgroupPattern, getRecording, setRecording, attachExperiment, detachExperiment, getCompareState, durationFromFileMetadata, getChartToggles, setChartToggle };
