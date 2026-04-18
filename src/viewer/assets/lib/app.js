// app.js — Shared dashboard logic for both server and WASM viewers.
// Exports initDashboard(config) which sets up state and mounts the Mithril router.

import { ChartsState, Chart } from './charts/chart.js';
import { QueryExplorer, SingleChartView } from './explorers.js';
import { CgroupSelector } from './cgroup_selector.js';
import globalColorMapper from './charts/util/colormap.js';
import { TopNav, Sidebar, countCharts, formatSize } from './layout.js';
import { CpuTopology } from './topology.js';
import { executePromQLRangeQuery, applyResultToPlot, fetchHeatmapsForGroups, substituteCgroupPattern, processDashboardData, clearMetadataCache, setStepOverride, getStepOverride, setSelectedNode, setSelectedInstance, getSelectedNode, injectLabel } from './data.js';
import { reportStore, setStorageScope, loadPayloadIntoStore, SelectionView, ReportView } from './selection.js';
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
    }));

    SystemInfoView = createSystemInfoView({
        CpuTopology,
        formatBytes: formatSize,
    });

    MetadataView = createMetadataView();
};

// ── Helpers ────────────────────────────────────────────────────────

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

const changeGranularity = async (step) => {
    currentGranularity = step;
    setStepOverride(step);

    const currentRoute = m.route.get();
    const section = currentRoute ? currentRoute.replace(/^\//, '') : '';

    for (const key of Object.keys(sectionResponseCache)) {
        if (key !== section) delete sectionResponseCache[key];
    }
    heatmapDataCache.clear();
    chartsState.zoomLevel = null;
    chartsState.zoomSource = null;
    chartsState.globalZoom = null;

    if (!section) return;

    try {
        delete sectionResponseCache[section];
        const data = await loadSection(section);
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
    extra,
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
            (g.plots || []).some(p => isHistogramPlot(p))
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

    if (config.selectionPayload && Array.isArray(config.selectionPayload.entries)) {
        loadPayloadIntoStore(reportStore, config.selectionPayload);
        reportStore.loadedFrom = 'embedded report';
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
        sectionResponseCache,
        getHasSystemInfo: () => systemInfoData,
        getHasFileMetadata: () => fileMetadata && Object.keys(fileMetadata).length > 0,
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
                    return buildClientOnlySectionView(Main, sectionResponseCache, systemInfoSection);
                }

                if (params.section === 'metadata') {
                    bootstrapCacheIfNeeded();
                    return buildClientOnlySectionView(Main, sectionResponseCache, metadataSection);
                }

                if (params.section === 'selection') {
                    bootstrapCacheIfNeeded();
                    return buildClientOnlySectionView(Main, sectionResponseCache, selectionSection);
                }

                if (params.section === 'report') {
                    bootstrapCacheIfNeeded();
                    return buildClientOnlySectionView(Main, sectionResponseCache, reportSection);
                }

                const cachedView = (sectionKey, path) => ({
                    view() {
                        const data = sectionResponseCache[sectionKey];
                        if (!data) return m('div', 'Loading...');
                        const activeSection = data.sections.find(
                            (section) => section.route === path,
                        );
                        return m(Main, { ...data, activeSection });
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

export { initDashboard, sectionResponseCache, clearViewerCaches, chartsState, loadSection, preloadSections, getHeatmapEnabled, heatmapDataCache, fetchSectionHeatmapData, getActiveCgroupPattern, getRecording, setRecording };
