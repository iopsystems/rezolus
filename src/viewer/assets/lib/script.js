import { ChartsState, Chart } from './charts/chart.js';
import { QueryExplorer, SingleChartView } from './explorers.js';
import { CgroupSelector } from './cgroup_selector.js';
import globalColorMapper from './charts/util/colormap.js';
import { TopNav, Sidebar, countCharts, formatSize } from './layout.js';
import { CpuTopology } from './topology.js';
import { executePromQLRangeQuery, applyResultToPlot, fetchHeatmapsForGroups, substituteCgroupPattern, processDashboardData } from './data.js';
import { reportStore, toggleSelection, isSelected, loadPayloadIntoStore, SelectionView, ReportView } from './selection.js';
import { notify, showSaveModal, SaveModal } from './overlays.js';
import { ViewerApi } from './viewer_api.js';
import { FileUpload } from './landing.js';
import { createSystemInfoView, renderCgroupSection } from './section_views.js';
import { buildTopNavAttrs, createMainComponent } from './navigation.js';

// Live mode state - detected at startup
let liveMode = false;
let liveRefreshInterval = null;

// System info data — fetched once at startup
let systemInfoData = null;

// File checksum (SHA-256) — fetched once at startup for parquet identity
let fileChecksum = null;

// Transport state for live mode (Wireshark-style)
// Starts recording — data flows from agent into TSDB and UI refreshes
let recording = true;

// Fetch metadata, system info, and selection in parallel.
// Used by both bootstrap() and uploadParquet().
const fetchBackendState = async () => {
    const [metaResult, sysResult, selResult] = await Promise.allSettled([
        ViewerApi.getMetadata(),
        ViewerApi.getSystemInfo(),
        ViewerApi.getSelection(),
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
    const filename = await showSaveModal('rezolus-capture', '.parquet');
    if (!filename) return;
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

const uploadParquet = async (file) => {
    try {
        await ViewerApi.uploadParquet(file);
        clearViewerCaches();
        chartsState.resetAll();
        await fetchBackendState();
        // Re-fetch overview so the view has data to render immediately.
        // m.route.set('/overview') is a no-op when already on /overview
        // (the route guard returns a never-resolving promise), so we must
        // populate the cache before triggering a redraw.
        const data = await ViewerApi.getSection('overview');
        const processed = await processDashboardData(data, null);
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

        const hasLocalZoom = chartsState.zoomSource === 'local' && !chartsState.isDefaultZoom();
        const hasSelection = hasLocalZoom ||
            Array.from(chartsState.charts.values()).some(c => c._tooltipFrozen || (c.pinnedSet && c.pinnedSet.size > 0));

        const hasHistogramCharts = (attrs.groups || []).some(g =>
            (g.plots || []).some(p => p.promql_query && p.promql_query.includes('histogram_percentiles'))
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
    buildAttrs: topNavAttrs,
});
const SystemInfoView = createSystemInfoView({
    CpuTopology,
    formatBytes: formatSize,
});

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

        const expandLink = (spec) => {
            if (!spec.promql_query) return null;
            const href = `${sectionRoute}/chart/${encodeURIComponent(spec.opts.id)}`;
            return m('a.chart-expand', {
                href, target: '_blank', title: 'Open in new tab',
                onclick: (e) => e.stopPropagation(),
            }, [
                'Expand ',
                m('svg', { width: 12, height: 12, viewBox: '0 0 16 16', fill: 'currentColor' },
                    m('path', { d: 'M10 1h5v5h-1.5V3.56L9.78 7.28 8.72 6.22l3.72-3.72H10V1zM1 6V1h5v1.5H3.56l3.72 3.72-1.06 1.06L2.5 3.56V6H1zm5 4H1v5h5v-1.5H3.56l3.72-3.72-1.06-1.06L2.5 12.44V10zm4 0v1.5h2.44l-3.72 3.72 1.06 1.06 3.72-3.72V15H15v-5h-5z' }),
                ),
            ]);
        };

        const selectButton = (spec) => {
            if (!spec.promql_query) return null;
            const sectionKey = sectionRoute.replace(/^\//, '');
            const selected = isSelected(spec.opts.id);
            return m('button.chart-select', {
                class: selected ? 'chart-selected' : '',
                onclick: (e) => {
                    e.stopPropagation();
                    toggleSelection(spec, sectionKey, sectionName);
                    m.redraw();
                },
                title: selected ? 'Remove from selection' : 'Add to selection',
            }, selected ? 'Selected' : 'Select');
        };

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
                        const isHistogramChart = spec.promql_query && spec.promql_query.includes('histogram_percentiles');

                        if (isHistogramChart && isHeatmapMode && sectionHeatmapData?.has(spec.opts.id)) {
                            // Create heatmap spec from the fetched data
                            const heatmapData = sectionHeatmapData.get(spec.opts.id);
                            const heatmapSpec = {
                                ...spec,
                                opts: {
                                    ...prefixTitle(spec.opts),
                                    style: 'histogram_heatmap',
                                },
                                time_data: heatmapData.time_data,
                                bucket_bounds: heatmapData.bucket_bounds,
                                data: heatmapData.data,
                                min_value: heatmapData.min_value,
                                max_value: heatmapData.max_value,
                            };
                            return m('div.chart-wrapper', [
                                chartHeader(heatmapSpec.opts),
                                m(Chart, { spec: heatmapSpec, chartsState, interval }),
                                expandLink(spec),
                                selectButton(spec),
                            ]);
                        }

                        const prefixedSpec = { ...spec, opts: prefixTitle(spec.opts), noCollapse: attrs.noCollapse };
                        return m('div.chart-wrapper', [
                            chartHeader(prefixedSpec.opts),
                            m(Chart, { spec: prefixedSpec, chartsState, interval }),
                            expandLink(spec),
                            selectButton(spec),
                        ]);
                    }),
                ),
            ],
        );
    },
};

// Application state management
const chartsState = new ChartsState();

// Double-click anywhere on the page resets zoom and clears all pin selections
document.addEventListener('dblclick', () => {
    if (!chartsState.isDefaultZoom() || chartsState.charts.size > 0) {
        chartsState.resetAll();
        m.redraw();
    }
});


// Fetch data for a section and cache it.
const preloadSection = async (section) => {
    // Skip preloading in live mode - data changes constantly
    if (liveMode || sectionResponseCache[section]) {
        return Promise.resolve();
    }

    const data = await ViewerApi.getSection(section);

    const processedData = await processDashboardData(data, activeCgroupPattern);
    sectionResponseCache[section] = processedData;
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
        const promises = [processDashboardData(data, activeCgroupPattern)];
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
                        const processedData = await processDashboardData(data, activeCgroupPattern);
                        sectionResponseCache[sectionKey] = processedData;
                        return makeSingleChartView();
                    });
            },
        },
        '/:section': {
            onmatch(params, requestedPath) {
                // Prevent a route change if we're already on this route
                if (m.route.get() === requestedPath) {
                    return new Promise(function () {});
                }

                if (requestedPath !== m.route.get()) {
                    // Clear chart instances (they'll be recreated), but preserve zoom.
                    chartsState.charts.clear();

                    // Reset cgroup pattern so it doesn't leak between sections.
                    activeCgroupPattern = null;

                    // Reset scroll position.
                    window.scrollTo(0, 0);
                }

                // System Info is not a backend section — render directly
                if (params.section === 'systeminfo') {
                    bootstrapCacheIfNeeded();
                    return buildClientOnlySectionView(systemInfoSection);
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
                        const processedData = await processDashboardData(data, activeCgroupPattern);
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
