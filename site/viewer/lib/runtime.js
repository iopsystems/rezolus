import { ChartsState, Chart } from './charts/chart.js';
import { QueryExplorer, SingleChartView } from './explorers.js';
import { CgroupSelector } from './cgroup_selector.js';
import globalColorMapper from './charts/util/colormap.js';
import { TopNav, Sidebar, countCharts } from './layout.js';
import { CpuTopology } from './topology.js';
import { executePromQLRangeQuery, applyResultToPlot, fetchHeatmapsForGroups, substituteCgroupPattern, processDashboardData } from './data.js';
import { selectionStore, reportStore, toggleSelection, isSelected, loadPayloadIntoStore, SelectionView, ReportView } from './selection.js';
import { SaveModal } from './overlays.js';
import { ViewerApi } from './viewer_api.js';
import { createSystemInfoView, renderCgroupSection } from './section_views.js';
import { buildTopNavAttrs, createMainComponent } from './navigation.js';

// Viewer info — set after WASM parquet load
let viewerInfo = null;

// System info data — parsed from parquet metadata
let systemInfoData = null;

// File checksum — not available in WASM mode (data never leaves the browser)
let fileChecksum = null;

// Build TopNav attrs from section data.
const topNavAttrs = (data, sectionRoute, extra) => buildTopNavAttrs({
    data,
    sectionRoute,
    chartsState,
    fileChecksum,
    liveMode: false,
    recording: false,
    extra,
});

const Main = createMainComponent({
    TopNav,
    Sidebar,
    SaveModal,
    SectionContent,
    sectionResponseCache,
    getHasSystemInfo: () => systemInfoData,
    buildAttrs: topNavAttrs,
});

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
            m('div#groups',
                attrs.groups.map((group) => m(Group, { ...group, sectionRoute, sectionName, interval })),
            ),
        ]);
    },
};

// System Info display component
const formatBytes = (bytes) => {
    if (!bytes) return '';
    if (bytes < 1024) return bytes + ' B';
    if (bytes < 1024 * 1024) return (bytes / 1024).toFixed(1) + ' KB';
    if (bytes < 1024 * 1024 * 1024) return (bytes / (1024 * 1024)).toFixed(1) + ' MB';
    return (bytes / (1024 * 1024 * 1024)).toFixed(1) + ' GB';
};

const SystemInfoView = createSystemInfoView({
    CpuTopology,
    formatBytes,
});

let activeCgroupPattern = null;
let heatmapEnabled = false;
let heatmapLoading = false;
const heatmapDataCache = new Map();

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

        const isOverview = sectionRoute === '/overview';
        const titlePrefix = isOverview ? attrs.name : sectionName;
        const prefixTitle = (opts) => titlePrefix
            ? { ...opts, title: `${titlePrefix} / ${opts.title}` }
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

        return m('div.group', { id: attrs.id }, [
            m('h2', `${attrs.name}`),
            m('div.charts',
                attrs.plots.map((spec) => {
                    const isHistogramChart = spec.promql_query && spec.promql_query.includes('histogram_percentiles');

                    if (isHistogramChart && isHeatmapMode && sectionHeatmapData?.has(spec.opts.id)) {
                        const heatmapData = sectionHeatmapData.get(spec.opts.id);
                        const heatmapSpec = {
                            ...spec,
                            opts: { ...prefixTitle(spec.opts), style: 'histogram_heatmap' },
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
        ]);
    },
};

// Application state
const chartsState = new ChartsState();

document.addEventListener('dblclick', () => {
    if (!chartsState.isDefaultZoom() || chartsState.charts.size > 0) {
        chartsState.resetAll();
        m.redraw();
    }
});

const sectionResponseCache = {};

// Load a section: generate dashboard data from JS definitions, then run PromQL via WASM.
const loadSection = async (sectionKey) => {
    if (sectionResponseCache[sectionKey]) return sectionResponseCache[sectionKey];
    if (!viewerInfo) return null;

    const data = await ViewerApi.getSection(sectionKey);
    if (!data) return null;

    const processedData = await processDashboardData(data, activeCgroupPattern);
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
const selectionSection = { name: 'Selection', route: '/selection' };
const reportSection = { name: 'Report', route: '/report' };

const bootstrapCacheIfNeeded = () => {
    if (Object.keys(sectionResponseCache).length > 0) return;

    loadSection('overview').then((data) => {
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

// ---- File Upload Landing Page ----

const FileUpload = {
    view() {
        return m('div.upload-container', [
            m('div.upload-card', [
                m('h1.upload-title', 'Rezolus Viewer'),
                m('p.upload-subtitle', 'Drop a parquet file to explore system performance metrics.'),
                m('p.upload-privacy', 'Your data never leaves the browser.'),
                m('div.upload-dropzone', {
                    id: 'dropzone',
                    ondragover: (e) => {
                        e.preventDefault();
                        e.currentTarget.classList.add('dragover');
                    },
                    ondragleave: (e) => {
                        e.currentTarget.classList.remove('dragover');
                    },
                    ondrop: (e) => {
                        e.preventDefault();
                        e.currentTarget.classList.remove('dragover');
                        const file = e.dataTransfer.files[0];
                        if (file) loadFile(file);
                    },
                }, [
                    m('svg.upload-icon', { width: 48, height: 48, viewBox: '0 0 24 24', fill: 'none', stroke: 'currentColor', 'stroke-width': 1.5 }, [
                        m('path', { d: 'M12 16V4m0 0L8 8m4-4l4 4', 'stroke-linecap': 'round', 'stroke-linejoin': 'round' }),
                        m('path', { d: 'M2 17l.621 2.485A2 2 0 004.561 21h14.878a2 2 0 001.94-1.515L22 17', 'stroke-linecap': 'round', 'stroke-linejoin': 'round' }),
                    ]),
                    m('p', 'Drag & drop a .parquet file here'),
                    m('p.upload-or', 'or'),
                    m('label.upload-btn', [
                        'Choose File',
                        m('input', {
                            type: 'file',
                            accept: '.parquet',
                            style: 'display:none',
                            onchange: (e) => {
                                const file = e.target.files[0];
                                if (file) loadFile(file);
                            },
                        }),
                    ]),
                ]),
                m('div', { style: 'margin-top: 1.5rem' }, [
                    m('p.upload-or', 'or'),
                    m('button.upload-btn', {
                        style: 'margin-top: 0.75rem; background: #6c757d',
                        onclick: loadDemo,
                        disabled: window._loading,
                    }, 'Try Demo'),
                ]),
                window._loadError && m('p.upload-error', window._loadError),
                window._loading && m('p.upload-loading', 'Loading parquet file...'),
            ]),
        ]);
    },
};

async function loadDemo() {
    window._loading = true;
    window._loadError = null;
    m.redraw();

    try {
        const resp = await fetch('demo.parquet');
        if (!resp.ok) throw new Error(`Failed to fetch demo: ${resp.status}`);
        const arrayBuffer = await resp.arrayBuffer();
        const data = new Uint8Array(arrayBuffer);

        const wasmModule = await import('../pkg/wasm_viewer.js');
        await wasmModule.default();
        window.viewer = new wasmModule.Viewer(data, 'demo.parquet');
        ViewerApi.setViewer(window.viewer);

        viewerInfo = JSON.parse(window.viewer.info());
        ViewerApi.setViewerInfo(viewerInfo);

        try { systemInfoData = await ViewerApi.getSystemInfo(); } catch { /* ignore */ }

        try {
            const parsed = await ViewerApi.getSelection();
            if (parsed && Array.isArray(parsed.entries)) {
                loadPayloadIntoStore(reportStore, parsed);
                reportStore.loadedFrom = 'embedded report';
            }
        } catch { /* ignore */ }

        window._loading = false;
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

        try { systemInfoData = await ViewerApi.getSystemInfo(); } catch { /* ignore */ }

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
        '/:section': {
            onmatch(params, requestedPath) {
                if (m.route.get() === requestedPath) {
                    return new Promise(function () {});
                }

                if (requestedPath !== m.route.get()) {
                    chartsState.charts.clear();
                    activeCgroupPattern = null;
                    window.scrollTo(0, 0);
                }

                if (params.section === 'systeminfo') {
                    bootstrapCacheIfNeeded();
                    return buildClientOnlySectionView(systemInfoSection);
                }

                if (params.section === 'selection') {
                    bootstrapCacheIfNeeded();
                    return buildClientOnlySectionView(selectionSection);
                }

                if (params.section === 'report') {
                    bootstrapCacheIfNeeded();
                    return buildClientOnlySectionView(reportSection);
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
if (new URLSearchParams(window.location.search).has('demo')) {
    loadDemo();
} else {
    m.mount(document.body, FileUpload);
}
