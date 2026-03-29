import { ChartsState, Chart } from './charts/chart.js';
import { QueryExplorer, SingleChartView } from './explorers.js';
import { CgroupSelector } from './cgroup_selector.js';
import { TopNav, Sidebar, countCharts } from './layout.js';
import { executePromQLRangeQuery, applyResultToPlot, substituteCgroupPattern, processDashboardData } from './data.js';

// Live mode state - detected at startup
let liveMode = false;
let liveRefreshInterval = null;

// Transport state for live mode (Wireshark-style)
// Starts recording — data flows from agent into TSDB and UI refreshes
let recording = true;

// Detect live mode on startup
m.request({ method: 'GET', url: '/api/v1/mode', withCredentials: true })
    .then((response) => {
        liveMode = response.live === true;
        if (liveMode) {
            startLiveRefresh();
        }
    })
    .catch(() => { /* ignore - assume file mode */ });

// Transport control actions
const startRecording = async () => {
    try {
        // Clear TSDB so the new recording has no gaps
        await m.request({ method: 'POST', url: '/api/v1/reset', withCredentials: true, background: true });
        // Clear frontend caches
        Object.keys(sectionResponseCache).forEach(k => delete sectionResponseCache[k]);
        heatmapDataCache.clear();
        chartsState.clear();
        recording = true;
        m.redraw();
    } catch (e) {
        console.error('Failed to start recording:', e);
    }
};

const stopRecording = () => {
    recording = false;
};

const saveCapture = () => {
    const a = document.createElement('a');
    a.href = '/api/v1/save';
    a.download = 'rezolus-capture.parquet';
    document.body.appendChild(a);
    a.click();
    document.body.removeChild(a);
};



// Main component
const Main = {
    view({
        attrs: { activeSection, groups, sections, source, version, filename, interval, filesize, start_time, end_time, num_series },
    }) {
        return m(
            'div',
            m(TopNav, {
                sectionRoute: activeSection?.route,
                groups,
                filename,
                source,
                version,
                interval,
                filesize,
                start_time,
                end_time,
                num_series,
                liveMode,
                recording,
                onStartRecording: startRecording,
                onStopRecording: stopRecording,
                onSaveCapture: saveCapture,
                chartsState,
            }),
            m('main', [
                m(Sidebar, {
                    activeSection,
                    sections,
                    sectionResponseCache,
                }),
                m(SectionContent, {
                    section: activeSection,
                    groups,
                    interval,
                }),
            ]),
        );
    },
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

        const { withData } = countCharts(attrs.groups);
        const titleText = `${sectionName} (${withData})`;

        // Special handling for cgroups with selector and two-column layout
        if (attrs.section.route === '/cgroups') {
            const leftGroups = attrs.groups.filter(
                (g) => g.metadata?.side === 'left',
            );
            const rightGroups = attrs.groups.filter(
                (g) => g.metadata?.side === 'right',
            );

            return m('div#section-content.cgroups-section', [
                m('h1.section-title', titleText),
                m(CgroupSelector, {
                    groups: attrs.groups,
                    executeQuery: executePromQLRangeQuery,
                    substitutePattern: substituteCgroupPattern,
                    setActiveCgroupPattern: (p) => { activeCgroupPattern = p; },
                }),
                m('div.cgroup-columns', [
                    m(
                        'div.cgroup-column.cgroup-column-left',
                        leftGroups.map((group) =>
                            m(Group, { ...group, sectionRoute, sectionName, interval, noCollapse: true }),
                        ),
                    ),
                    m(
                        'div.cgroup-column.cgroup-column-right',
                        rightGroups.map((group) =>
                            m(Group, { ...group, sectionRoute, sectionName, interval, noCollapse: true }),
                        ),
                    ),
                ]),
            ]);
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

// Active cgroup selection pattern — used by processDashboardData during live refresh
// to substitute __SELECTED_CGROUPS__ placeholders in cgroup queries.
let activeCgroupPattern = null;

// Global heatmap mode — applies to all sections
let heatmapEnabled = false;
let heatmapLoading = false;
// Cache of fetched heatmap data per section: sectionRoute -> Map<chartId, data>
const heatmapDataCache = new Map();

// Fetch heatmap data for all histogram charts in a section — queries run in parallel.
const fetchSectionHeatmapData = async (sectionRoute, groups) => {
    heatmapLoading = true;
    m.redraw();

    // Collect all histogram plots that need heatmap queries
    const heatmapPlots = [];
    for (const group of groups || []) {
        for (const plot of group.plots || []) {
            if (plot.promql_query && plot.promql_query.includes('histogram_percentiles')) {
                const match = plot.promql_query.match(/histogram_percentiles\s*\(\s*\[[^\]]*\]\s*,\s*(.+)\)$/);
                if (!match) continue;

                const metricSelector = match[1].trim();
                heatmapPlots.push({
                    id: plot.opts.id,
                    query: `histogram_heatmap(${metricSelector})`,
                });
            }
        }
    }

    // Fire all heatmap queries concurrently
    const results = await Promise.allSettled(
        heatmapPlots.map((hp) => executePromQLRangeQuery(hp.query)),
    );

    const heatmapData = new Map();
    for (let i = 0; i < heatmapPlots.length; i++) {
        const outcome = results[i];
        if (outcome.status === 'fulfilled') {
            const result = outcome.value;
            if (result.status === 'success' && result.data && result.data.resultType === 'histogram_heatmap') {
                const heatmapResult = result.data.result;
                heatmapData.set(heatmapPlots[i].id, {
                    time_data: heatmapResult.timestamps,
                    bucket_bounds: heatmapResult.bucket_bounds,
                    data: heatmapResult.data,
                    min_value: heatmapResult.min_value,
                    max_value: heatmapResult.max_value,
                });
            }
        } else {
            console.error('Failed to fetch histogram heatmap:', outcome.reason);
        }
    }

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
                            ]);
                        }

                        const prefixedSpec = { ...spec, opts: prefixTitle(spec.opts), noCollapse: attrs.noCollapse };
                        return m('div.chart-wrapper', [
                            chartHeader(prefixedSpec.opts),
                            m(Chart, { spec: prefixedSpec, chartsState, interval }),
                            expandLink(spec),
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

const sectionResponseCache = {};

// Fetch data for a section and cache it.
const preloadSection = async (section) => {
    // Skip preloading in live mode - data changes constantly
    if (liveMode || sectionResponseCache[section]) {
        return Promise.resolve();
    }

    const url = `/data/${section}.json`;
    const data = await m.request({
        method: 'GET',
        url,
        withCredentials: true,
    });

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
        const url = `/data/${section}.json`;
        const data = await m.request({ method: 'GET', url, withCredentials: true, background: true });

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

// Main application entry point
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
                        m(TopNav, {
                            sectionRoute: activeSection?.route,
                            groups: data.groups,
                            filename: data.filename,
                            source: data.source,
                            version: data.version,
                            interval: data.interval,
                            filesize: data.filesize,
                            start_time: data.start_time,
                            end_time: data.end_time,
                            num_series: data.num_series,
                            liveMode,
                            recording,
                            onStartRecording: startRecording,
                            onStopRecording: stopRecording,
                            onSaveCapture: saveCapture,
                            chartsState,
                        }),
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

            const url = `/data/${sectionKey}.json`;
            return m.request({ method: 'GET', url, withCredentials: true })
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

                // Reset scroll position.
                window.scrollTo(0, 0);

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

            const url = `/data/${params.section}.json`;
            return m
                .request({
                    method: 'GET',
                    url,
                    withCredentials: true,
                })
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
