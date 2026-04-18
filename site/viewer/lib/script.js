import { ChartsState, Chart } from './charts/chart.js';
import { QueryExplorer, SingleChartView } from './explorers.js';
import { CgroupSelector } from './cgroup_selector.js';
import globalColorMapper from './charts/util/colormap.js';
import { TopNav, Sidebar, countCharts, formatSize } from './layout.js';
import { CpuTopology } from './topology.js';
import { executePromQLRangeQuery, applyResultToPlot, fetchHeatmapsForGroups, substituteCgroupPattern, processDashboardData, setStepOverride, getStepOverride, setSelectedNode, setSelectedInstance, getSelectedNode, injectLabel } from './data.js';
import { reportStore, setStorageScope, loadPayloadIntoStore, SelectionView, ReportView } from './selection.js';
import { SaveModal } from './overlays.js';
import { ViewerApi } from './viewer_api.js';
import { createSystemInfoView, createMetadataView, renderCgroupSection } from './section_views.js';
import { buildTopNavAttrs, createMainComponent } from './navigation.js';
import { FileUpload } from './landing.js';
import { initTheme } from './theme.js';
import { isHistogramPlot } from './charts/metric_types.js';
import { renderServiceSection, createServiceRoutes } from './service.js';
import { createGroupComponent, getCachedSectionMeta, buildClientOnlySectionView } from './viewer_core.js';

initTheme();

// Tracks the active section route to detect section switches
let activeSectionRoute = null;

// Viewer info — set after WASM parquet load
let viewerInfo = null;

// System info data — parsed from parquet metadata
let systemInfoData = null;

// File checksum — not available in WASM mode (data never leaves the browser)
let fileChecksum = null;

// File-level metadata — fetched once after parquet load
let fileMetadata = null;

// Multi-node state — pre-computed by WASM from per_source_metadata
let nodeList = [];
let nodeVersions = {};
let selectedNode = null;

// Per-service instance lists: { "vllm": [{id: "0", node: "gpu01"}, ...], ... }
let serviceInstances = {};

// Selected instance per service: { "vllm": null, "llm-perf": "0" }
let selectedInstances = {};

const clearViewerCaches = () => {
    Object.keys(sectionResponseCache).forEach((k) => delete sectionResponseCache[k]);
    heatmapDataCache.clear();
    chartsState.clear();
};

// Apply pre-computed multi-node info from the WASM response.
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

/// Re-fetch and re-process the current section's data, then redraw.
const reloadCurrentSection = async () => {
    const currentRoute = m.route.get();
    if (!currentRoute) return;
    const section = currentRoute.replace(/^\//, '').replace(/#.*/, '');
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

let currentGranularity = null;

const changeGranularity = async (step) => {
    currentGranularity = step;
    setStepOverride(step);

    const currentRoute = m.route.get();
    const section = currentRoute
        ? currentRoute.replace(/^\//, '').replace(/#.*/, '')
        : '';

    // Invalidate all section caches EXCEPT the current one so the component
    // tree stays mounted (avoids unmounting CgroupSelector which would lose
    // its selected-cgroup state and leave charts empty).
    for (const key of Object.keys(sectionResponseCache)) {
        if (key !== section) delete sectionResponseCache[key];
    }
    heatmapDataCache.clear();
    chartsState.zoomLevel = null;
    chartsState.zoomSource = null;
    chartsState.globalZoom = null;

    if (!section) return;

    try {
        // Force re-fetch by clearing just this section's cache before loadSection
        delete sectionResponseCache[section];
        const data = await loadSection(section);
        if (data?.sections) preloadSections(data.sections);
        m.redraw();
    } catch (_) { /* keep existing view on error */ }
};

// Build TopNav attrs from section data.
const topNavAttrs = (data, sectionRoute, extra) => buildTopNavAttrs({
    data,
    sectionRoute,
    chartsState,
    fileChecksum,
    liveMode: false,
    recording: false,
    granularity: currentGranularity,
    onGranularityChange: changeGranularity,
    nodeList,
    selectedNode,
    nodeVersions,
    onNodeChange: changeNode,
    extra,
});

let Main;

const toggleGlobalHeatmap = async () => {
    heatmapEnabled = !heatmapEnabled;
    m.redraw();
};

const SectionContent = {
    view({ attrs }) {
        const sectionRoute = attrs.section.route;
        const sectionName = attrs.section.name;
        const interval = attrs.interval;

        // When switching sections, reset local zoom to global so new charts start
        // from the globally selected time range rather than the previous local zoom.
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
                m(QueryExplorer, { liveMode: false, isRecording: () => false }),
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
                        onclick: async () => {
                            heatmapEnabled = !heatmapEnabled;
                            const sectionHeatmapData = heatmapDataCache.get(sectionRoute);
                            if (heatmapEnabled && (!sectionHeatmapData || sectionHeatmapData.size === 0)) {
                                await fetchSectionHeatmapData(sectionRoute, attrs.groups);
                            } else {
                                m.redraw();
                            }
                        },
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


const sectionResponseCache = {};

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
const SystemInfoView = createSystemInfoView({
    CpuTopology,
    formatBytes: formatSize,
});
const MetadataView = createMetadataView();

let activeCgroupPattern = null;
let heatmapEnabled = false;
let heatmapLoading = false;
const heatmapDataCache = new Map();

// Group component — shared via viewer_core.js
const Group = createGroupComponent(() => ({
    chartsState, heatmapEnabled, heatmapLoading, heatmapDataCache,
}));

const fetchSectionHeatmapData = async (sectionRoute, groups) => {
    heatmapLoading = true;
    m.redraw();
    const heatmapData = await fetchHeatmapsForGroups(groups);
    heatmapDataCache.set(sectionRoute, heatmapData);
    heatmapLoading = false;
    m.redraw();
};

// Application state
const chartsState = new ChartsState();

// Double-click anywhere on the page resets zoom and clears all pin selections
document.addEventListener('dblclick', () => {
    if (!chartsState.isDefaultZoom() || chartsState.charts.size > 0) {
        chartsState.resetAll();
        m.redraw();
    }
});


// Load a section: generate dashboard data from JS definitions, then run PromQL via WASM.
const loadSection = async (sectionKey) => {
    if (sectionResponseCache[sectionKey]) return sectionResponseCache[sectionKey];
    if (!viewerInfo) return null;

    const data = await ViewerApi.getSection(sectionKey);
    if (!data) return null;

    const processedData = await processDashboardData(data, activeCgroupPattern, `/${sectionKey}`);
    sectionResponseCache[sectionKey] = processedData;
    return processedData;
};

// Preload all sections in parallel.
const preloadSections = (allSections) => {
    for (const section of allSections) {
        const key = section.route.substring(1);
        if (!sectionResponseCache[key]) {
            loadSection(key).then(() => m.redraw()).catch(() => {});
        }
    }
};

// Synthetic sections
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


async function loadDemo(filename = 'demo.parquet') {
    window._loading = true;
    window._loadError = null;
    m.redraw();

    try {
        const resp = await fetch(filename);
        if (!resp.ok) throw new Error(`Failed to fetch ${filename}: ${resp.status}`);
        const arrayBuffer = await resp.arrayBuffer();
        const data = new Uint8Array(arrayBuffer);

        const wasmModule = await import('../pkg/wasm_viewer.js');
        await wasmModule.default();
        window.viewer = new wasmModule.Viewer(data, filename);
        ViewerApi.setViewer(window.viewer);

        viewerInfo = JSON.parse(window.viewer.info());
        ViewerApi.setViewerInfo(viewerInfo);
        setStorageScope({
            filename: viewerInfo.filename,
            minTime: viewerInfo.minTime,
            maxTime: viewerInfo.maxTime,
            numSeries: (viewerInfo.counter_names?.length || 0) +
                       (viewerInfo.gauge_names?.length || 0) +
                       (viewerInfo.histogram_names?.length || 0),
        });

        try { systemInfoData = await ViewerApi.getSystemInfo(); } catch { /* ignore */ }

        try {
            fileMetadata = await ViewerApi.getFileMetadata();
            applyMultiNodeInfo();
        } catch { /* ignore */ }

        try {
            const parsed = await ViewerApi.getSelection();
            if (parsed && Array.isArray(parsed.entries)) {
                loadPayloadIntoStore(reportStore, parsed);
                reportStore.loadedFrom = 'embedded report';
            }
        } catch { /* ignore */ }

        window._loading = false;

        // Ensure ?demo is in the URL so bookmarks/refreshes auto-load the demo
        const url = new URL(window.location);
        const currentDemo = new URLSearchParams(window.location.search).get('demo');
        if (currentDemo !== filename) {
            url.searchParams.set('demo', filename);
            window.history.replaceState(null, '', url);
        }

        initDashboardRouter();
    } catch (e) {
        window._loading = false;
        window._loadError = `Failed to load demo: ${e.message || e}`;
        m.redraw();
    }
}

async function loadFile(file) {
    window._loading = true;
    window._loadError = null;
    m.redraw();

    try {
        const arrayBuffer = await file.arrayBuffer();
        const data = new Uint8Array(arrayBuffer);

        const wasmModule = await import('../pkg/wasm_viewer.js');
        await wasmModule.default(); // load the WASM binary
        window.viewer = new wasmModule.Viewer(data, file.name);
        ViewerApi.setViewer(window.viewer);

        viewerInfo = JSON.parse(window.viewer.info());
        ViewerApi.setViewerInfo(viewerInfo);
        setStorageScope({
            filename: viewerInfo.filename,
            minTime: viewerInfo.minTime,
            maxTime: viewerInfo.maxTime,
            numSeries: (viewerInfo.counter_names?.length || 0) +
                       (viewerInfo.gauge_names?.length || 0) +
                       (viewerInfo.histogram_names?.length || 0),
        });

        try { systemInfoData = await ViewerApi.getSystemInfo(); } catch { /* ignore */ }

        try {
            fileMetadata = await ViewerApi.getFileMetadata();
            applyMultiNodeInfo();
        } catch { /* ignore */ }

        try {
            const parsed = await ViewerApi.getSelection();
            if (parsed && Array.isArray(parsed.entries)) {
                loadPayloadIntoStore(reportStore, parsed);
                reportStore.loadedFrom = 'embedded report';
            }
        } catch { /* ignore */ }

        window._loading = false;

        // Switch to the dashboard router
        initDashboardRouter();
    } catch (e) {
        window._loading = false;
        window._loadError = `Failed to load file: ${e.message || e}`;
        m.redraw();
    }
}

function initDashboardRouter() {
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
                        const activeSection = data.sections.find((section) => section.route === path);
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
}

// ---- Initial mount: show file upload page, or auto-load demo ----
const _demoParam = new URLSearchParams(window.location.search).get('demo');
if (_demoParam !== null) {
    loadDemo(_demoParam || 'demo.parquet');
} else {
    m.mount(document.body, {
        view: () => m(FileUpload, {
            onFile: loadFile,
            onDemo: loadDemo,
            demos: [
                { label: 'vLLM + System', file: 'vllm.parquet' },
                { label: 'Cachecannon + System', file: 'cachecannon.parquet' },
                { label: 'System Metrics', file: 'demo.parquet' },
            ],
            loading: window._loading,
            error: window._loadError,
        }),
    });
}
