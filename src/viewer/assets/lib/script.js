import { ChartsState, Chart } from './charts/chart.js';
import { QueryExplorer, SingleChartView } from './explorers.js';
import { CgroupSelector } from './cgroup_selector.js';
import globalColorMapper from './charts/util/colormap.js';
import { TopNav, Sidebar, countCharts, formatSize } from './layout.js';
import { CpuTopology } from './topology.js';
import { executePromQLRangeQuery, applyResultToPlot, fetchHeatmapsForGroups, substituteCgroupPattern, processDashboardData, clearMetadataCache, setStepOverride, getStepOverride, setSelectedNode, setSelectedInstance } from './data.js';
import { reportStore, setStorageScope, loadPayloadIntoStore, SelectionView, ReportView } from './selection.js';
import { expandLink, selectButton } from './chart_controls.js';
import { notify, showSaveModal, SaveModal } from './overlays.js';
import { ViewerApi } from './viewer_api.js';
import { FileUpload } from './landing.js';
import { createSystemInfoView, createMetadataView, renderCgroupSection } from './section_views.js';
import { buildTopNavAttrs, createMainComponent } from './navigation.js';
import { initTheme } from './theme.js';
import { isHistogramPlot, buildHistogramHeatmapSpec } from './charts/metric_types.js';
import { renderServiceSection, createServiceRoutes } from './service.js';

initTheme();

// Tracks the active section route to detect section switches
let activeSectionRoute = null;

// Live mode state - detected at startup
let liveMode = false;
let liveRefreshInterval = null;

// System info data — fetched once at startup
let systemInfoData = null;

// File checksum (SHA-256) — fetched once at startup for parquet identity
let fileChecksum = null;

// File-level metadata — fetched once at startup
let fileMetadata = null;

// Parsed node list and per-source metadata from combined files
let nodeList = [];           // e.g. ["web01", "web02"]
let perSourceMeta = {};      // parsed per_source_metadata object
let selectedNode = null;     // currently selected node name (null = no multi-node)

// Per-service instance lists: { "vllm": [{id: "0", node: "gpu01"}, ...], ... }
let serviceInstances = {};

// Selected instance per service: { "vllm": null, "llm-perf": "0" }
let selectedInstances = {};

// Transport state for live mode (Wireshark-style)
// Starts recording — data flows from agent into TSDB and UI refreshes
let recording = true;

// Fetch metadata, system info, and selection in parallel.
// Used by both bootstrap() and uploadParquet().
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
        systemInfoData = sysResult.value;
    }
    if (selResult.status === 'fulfilled') {
        const data = selResult.value;
        if (data && Array.isArray(data.entries)) {
            loadPayloadIntoStore(reportStore, data);
            reportStore.loadedFrom = 'embedded report';
        }
    }
    if (fmResult.status === 'fulfilled') {
        fileMetadata = fmResult.value;
        parseNodeList();
    }
};

const parseNodeList = () => {
    nodeList = [];
    perSourceMeta = {};
    selectedNode = null;

    if (!fileMetadata || !fileMetadata.per_source_metadata) return;

    perSourceMeta = fileMetadata.per_source_metadata;
    const nodes = [];
    const rezGroup = perSourceMeta.rezolus;
    if (rezGroup && typeof rezGroup === 'object') {
        for (const [subKey, value] of Object.entries(rezGroup)) {
            const nodeName = value.node || subKey;
            if (!nodes.includes(nodeName)) {
                nodes.push(nodeName);
            }
        }
    }
    nodeList = nodes;
    if (nodeList.length > 0) {
        selectedNode = nodeList[0];
        setSelectedNode(nodeList[0]);
    }

    parseServiceInstances();
};

const parseServiceInstances = () => {
    serviceInstances = {};
    selectedInstances = {};

    if (!perSourceMeta) return;

    for (const [source, group] of Object.entries(perSourceMeta)) {
        if (source === 'rezolus') continue;
        if (!group || typeof group !== 'object') continue;

        for (const [subKey, value] of Object.entries(group)) {
            const instanceId = value.instance || subKey;
            if (!serviceInstances[source]) {
                serviceInstances[source] = [];
            }
            serviceInstances[source].push({
                id: instanceId,
                node: value.node || null,
            });
        }
    }

    for (const source of Object.keys(serviceInstances)) {
        selectedInstances[source] = null;
    }
};

// Transport control actions
const startRecording = async () => {
    try {
        // Clear TSDB so the new recording has no gaps
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

const clearViewerCaches = () => {
    Object.keys(sectionResponseCache).forEach((k) => delete sectionResponseCache[k]);
    heatmapDataCache.clear();
    chartsState.clear();
};

const changeNode = (nodeName) => {
    selectedNode = nodeName;
    setSelectedNode(nodeName);
    clearViewerCaches();
    // Re-set the current route to force onmatch to re-run and fetch
    // fresh data for the newly selected node.
    m.route.set(m.route.get());
};

const changeInstance = (serviceName, instanceId) => {
    selectedInstances[serviceName] = instanceId;
    setSelectedInstance(serviceName, instanceId);
    const svcKey = `service/${serviceName}`;
    delete sectionResponseCache[svcKey];
    m.route.set(m.route.get());
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
        // Re-fetch overview so the view has data to render immediately.
        // m.route.set('/overview') is a no-op when already on /overview
        // (the route guard returns a never-resolving promise), so we must
        // populate the cache before triggering a redraw.
        const data = await ViewerApi.getSection('overview');
        const processed = await processDashboardData(data, null, '/overview');
        sectionResponseCache['overview'] = processed;
        if (processed.sections) preloadSections(processed.sections);
        // Navigate to overview if on a different route; otherwise just redraw.
        if (m.route.get() !== '/overview') {
            m.route.set('/overview');
        }
        m.redraw();
    } catch (e) {
        notify('error', `Failed to upload parquet: ${e.message || e}`);
    }
};

// Build common TopNav attrs from section data. Pass extra to override/add fields.
const topNavAttrs = (data, sectionRoute, extra) => buildTopNavAttrs({
    data,
    sectionRoute,
    chartsState,
    fileChecksum,
    liveMode,
    recording,
    onStartRecording: startRecording,
    onStopRecording: stopRecording,
    onSaveCapture: saveCapture,
    onUploadParquet: uploadParquet,
    granularity: currentGranularity,
    onGranularityChange: changeGranularity,
    nodeList,
    selectedNode,
    perSourceMeta,
    onNodeChange: changeNode,
    extra,
});

let Main;

const toggleGlobalHeatmap = async () => {
    heatmapEnabled = !heatmapEnabled;
    m.redraw();
};

const getCachedSectionMeta = (interval) => {
    const anyCached = Object.values(sectionResponseCache)[0];
    return {
        interval: anyCached?.interval || interval,
        version: anyCached?.version,
        source: anyCached?.source,
        filename: anyCached?.filename,
        start_time: anyCached?.start_time,
        end_time: anyCached?.end_time,
    };
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

        // Special handling for Query Explorer
        if (sectionName === 'Query Explorer') {
            return m('div#section-content', [
                m(QueryExplorer, { liveMode, isRecording: () => recording }),
            ]);
        }

        // Special handling for System Info
        if (sectionName === 'System Info') {
            return m('div#section-content', [
                m(SystemInfoView, { data: systemInfoData }),
            ]);
        }

        // Special handling for Metadata
        if (sectionName === 'Metadata') {
            return m('div#section-content', [
                m(MetadataView, { data: fileMetadata }),
            ]);
        }

        // Special handling for Selection
        if (sectionName === 'Selection') {
            const sectionMeta = getCachedSectionMeta(interval);
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

        // Special handling for Report
        if (sectionName === 'Report') {
            const sectionMeta = getCachedSectionMeta(interval);
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

        // Special handling for Service extension
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
                executePromQLRangeQuery,
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
            m(
                'div#groups',
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

// Active cgroup selection pattern — used by processDashboardData during live refresh
// to substitute __SELECTED_CGROUPS__ placeholders in cgroup queries.
let activeCgroupPattern = null;

// Global heatmap mode — applies to all sections
let heatmapEnabled = false;
let heatmapLoading = false;
// Cache of fetched heatmap data per section: sectionRoute -> Map<chartId, data>
const heatmapDataCache = new Map();

// Fetch heatmap data for all histogram charts in a section.
const fetchSectionHeatmapData = async (sectionRoute, groups) => {
    heatmapLoading = true;
    m.redraw();

    const heatmapData = await fetchHeatmapsForGroups(groups);
    heatmapDataCache.set(sectionRoute, heatmapData);

    heatmapLoading = false;
    m.redraw();
};

// Group component
const Group = {
    view({ attrs }) {
        const sectionRoute = attrs.sectionRoute;
        const sectionName = attrs.sectionName;
        const interval = attrs.interval;
        const sectionHeatmapData = heatmapDataCache.get(sectionRoute);
        const isHeatmapMode = heatmapEnabled && !heatmapLoading;

        // Prefix plot titles for self-contained chart labels.
        // Overview page uses group name (CPU, Network, etc.) since it aggregates multiple sections.
        // Other pages use section name (Memory, CPU, etc.).
        const isOverview = sectionRoute === '/overview';
        const titlePrefix = isOverview ? attrs.name : sectionName;
        const prefixTitle = (opts) => titlePrefix
            ? { ...opts, title: `${titlePrefix}: ${opts.title}` }
            : opts;

        const chartHeader = (opts) => m('div.chart-header', [
            m('span.chart-title', opts.title),
            opts.description && m('span.chart-subtitle', opts.description),
        ]);

        return m(
            'div.group',
            {
                id: attrs.id,
            },
            [
                m('h2', `${attrs.name}`),
                m(
                    'div.charts',
                    attrs.plots.map((spec) => {
                        // Check if this is a histogram chart and we're in heatmap mode
                        const isHistogramChart = isHistogramPlot(spec);

                        if (isHistogramChart && isHeatmapMode && sectionHeatmapData?.has(spec.opts.id)) {
                            // Create heatmap spec from the fetched data
                            const heatmapData = sectionHeatmapData.get(spec.opts.id);
                            const heatmapSpec = buildHistogramHeatmapSpec(spec, heatmapData, prefixTitle(spec.opts));
                            return m('div.chart-wrapper', [
                                chartHeader(heatmapSpec.opts),
                                m(Chart, { spec: heatmapSpec, chartsState, interval }),
                                expandLink(spec, sectionRoute),
                                selectButton(spec, sectionRoute, sectionName),
                            ]);
                        }

                        const prefixedSpec = { ...spec, opts: prefixTitle(spec.opts), noCollapse: attrs.noCollapse };
                        return m('div.chart-wrapper', [
                            chartHeader(prefixedSpec.opts),
                            m(Chart, { spec: prefixedSpec, chartsState, interval }),
                            expandLink(spec, sectionRoute),
                            selectButton(spec, sectionRoute, sectionName),
                        ]);
                    }),
                ),
            ],
        );
    },
};

// Application state management
const chartsState = new ChartsState();
let currentGranularity = null;

const changeGranularity = async (step) => {
    currentGranularity = step;
    setStepOverride(step);
    clearViewerCaches();
    m.redraw();

    const currentRoute = m.route.get();
    if (!currentRoute) return;
    const section = currentRoute.replace(/^\//, '');
    if (!section) return;

    try {
        const data = await ViewerApi.getSection(section);
        const processed = await processDashboardData(data, activeCgroupPattern, `/${section}`);
        sectionResponseCache[section] = processed;
        if (processed.sections) preloadSections(processed.sections);
        m.redraw();
    } catch (_) { /* keep existing view on error */ }
};

// Double-click anywhere on the page resets zoom and clears all pin selections
document.addEventListener('dblclick', () => {
    if (!chartsState.isDefaultZoom() || chartsState.charts.size > 0) {
        chartsState.resetAll();
        m.redraw();
    }
});


// Fetch, process, and cache a section. Returns the processed data.
const loadSection = async (section) => {
    if (sectionResponseCache[section]) return sectionResponseCache[section];
    const data = await ViewerApi.getSection(section);
    const processedData = await processDashboardData(data, activeCgroupPattern, `/${section}`);
    sectionResponseCache[section] = processedData;
    return processedData;
};

// Fetch data for a section and cache it (preload variant — skips in live mode).
const preloadSection = async (section) => {
    if (liveMode || sectionResponseCache[section]) {
        return Promise.resolve();
    }
    return loadSection(section);
};

// Preload all sections in parallel so sidebar chart counts appear eagerly.
const preloadSections = (allSections) => {
    const sectionsToPreload = allSections
        .filter((section) => !sectionResponseCache[section.route])
        .map((section) => section.route.substring(1));

    for (const section of sectionsToPreload) {
        preloadSection(section).then(() => m.redraw()).catch(() => {});
    }
};

// Live mode: re-fetch section JSON and re-process PromQL queries.
// This creates new data objects so chart components detect the change
// via reference comparison in onupdate.
let liveRefreshInProgress = false;

const refreshCurrentSection = async () => {
    if (liveRefreshInProgress) return;

    // Skip UI refresh when paused or zoomed in — TSDB still ingests in the background
    if (!recording || !chartsState.isDefaultZoom()) return;

    const currentRoute = m.route.get();
    if (!currentRoute) return;

    const section = currentRoute.replace(/^\//, '');
    if (!section || section === 'query') return;

    liveRefreshInProgress = true;
    try {
        const data = await ViewerApi.getSection(section, true);

        // Run regular queries and histogram heatmap queries concurrently
        const promises = [processDashboardData(data, activeCgroupPattern, currentRoute)];
        if (heatmapEnabled) {
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

const startLiveRefresh = () => {
    if (liveRefreshInterval) return;
    liveRefreshInterval = setInterval(refreshCurrentSection, 5000);
};

// Synthetic section object for System Info (not a backend dashboard section)
const systemInfoSection = { name: 'System Info', route: '/systeminfo' };
const metadataSection = { name: 'Metadata', route: '/metadata' };
const selectionSection = { name: 'Selection', route: '/selection' };
const reportSection = { name: 'Report', route: '/report' };

const bootstrapCacheIfNeeded = () => {
    if (Object.keys(sectionResponseCache).length > 0) {
        return;
    }

    preloadSection('overview').then(() => {
        const data = sectionResponseCache.overview;
        if (data?.sections) preloadSections(data.sections);
        m.redraw();
    }).catch(() => {});
};

const buildClientOnlySectionView = (activeSection) => ({
    view() {
        const anyCached = Object.values(sectionResponseCache)[0];
        const sections = anyCached?.sections || [];
        return m(Main, {
            activeSection,
            groups: [],
            sections,
            source: anyCached?.source,
            version: anyCached?.version,
            filename: anyCached?.filename,
            interval: anyCached?.interval,
            filesize: anyCached?.filesize,
            start_time: anyCached?.start_time,
            end_time: anyCached?.end_time,
            num_series: anyCached?.num_series,
        });
    },
});

// Landing page state
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

// Main application entry point
const bootstrap = async () => {
    // Check backend mode first
    try {
        const response = await ViewerApi.getMode();
        if (!response.loaded && !response.live) {
            showLanding();
            return;
        }
        liveMode = response.live === true;
        if (liveMode) startLiveRefresh();
    } catch (_) { /* assume loaded file mode */ }

    // Fetch metadata, system info, and selection in parallel
    await fetchBackendState();
    if (fileChecksum) {
        setStorageScope({ filename: fileChecksum });
    }

    // Set up the router now that data is loaded
    m.route.prefix = ''; // use regular paths for navigation, eg. /overview
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

                return ViewerApi.getSection(sectionKey)
                    .then(async (data) => {
                        const processedData = await processDashboardData(data, activeCgroupPattern, `/${sectionKey}`);
                        sectionResponseCache[sectionKey] = processedData;
                        return makeSingleChartView();
                    });
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
                // Prevent a route change if we're already on this route
                if (m.route.get() === requestedPath) {
                    return new Promise(function () {});
                }

                if (requestedPath !== m.route.get()) {
                    // Clear chart instances (they'll be recreated), but preserve zoom.
                    chartsState.charts.clear();

                    // Reset cgroup pattern so it doesn't leak between sections,
                    // but preserve it when navigating back to cgroups.
                    if (params.section !== 'cgroups') {
                        activeCgroupPattern = null;
                    }

                    // Reset scroll position.
                    window.scrollTo(0, 0);
                }

                // System Info is not a backend section — render directly
                if (params.section === 'systeminfo') {
                    bootstrapCacheIfNeeded();
                    return buildClientOnlySectionView(systemInfoSection);
                }

                if (params.section === 'metadata') {
                    bootstrapCacheIfNeeded();
                    return buildClientOnlySectionView(metadataSection);
                }

                // Selection is a client-only section — no backend data
                if (params.section === 'selection') {
                    bootstrapCacheIfNeeded();
                    return buildClientOnlySectionView(selectionSection);
                }

                // Report is a client-only section — loaded from JSON import or parquet metadata
                if (params.section === 'report') {
                    bootstrapCacheIfNeeded();
                    return buildClientOnlySectionView(reportSection);
                }

                // In live mode, always read from cache dynamically so
                // refreshes flow through to the rendered view.
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
                    // Fetch heatmap data if globally enabled and not cached for this section
                    if (heatmapEnabled && !heatmapDataCache.has(requestedPath)) {
                        fetchSectionHeatmapData(requestedPath, sectionResponseCache[params.section].groups);
                    }
                    return cachedView(params.section, requestedPath);
                }

                return ViewerApi.getSection(params.section)
                    .then(async (data) => {

                        // Process PromQL queries for this section
                        const processedData = await processDashboardData(data, activeCgroupPattern, requestedPath);
                        sectionResponseCache[params.section] = processedData;

                        // Fetch heatmap data if globally enabled and not cached for this section
                        if (heatmapEnabled && !heatmapDataCache.has(requestedPath)) {
                            fetchSectionHeatmapData(requestedPath, processedData.groups);
                        }

                        // Preload other sections after initial load
                        preloadSections(processedData.sections);

                        return cachedView(params.section, requestedPath);
                    });
            },
        },
    });
};

bootstrap();
