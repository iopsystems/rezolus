import { ChartsState, Chart } from './charts/chart.js';

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

// Format utilities
const formatSize = (bytes) => {
    if (!bytes) return '';
    if (bytes < 1024) return bytes + ' B';
    if (bytes < 1024 * 1024) return (bytes / 1024).toFixed(1) + ' KB';
    return (bytes / (1024 * 1024)).toFixed(1) + ' MB';
};

const formatInterval = (secs) => {
    if (!secs) return '';
    if (secs < 0.001) return (secs * 1000000).toFixed(0) + 'us';
    if (secs < 1) return (secs * 1000).toFixed(0) + 'ms';
    return secs.toFixed(0) + 's';
};

const formatDuration = (secs) => {
    if (!secs && secs !== 0) return '';
    if (secs < 60) return secs.toFixed(0) + 's';
    if (secs < 3600) return (secs / 60).toFixed(1) + 'm';
    if (secs < 86400) return (secs / 3600).toFixed(1) + 'h';
    return (secs / 86400).toFixed(1) + 'd';
};

// Collapsible metadata state
let metadataExpanded = false;

// Close metadata dropdown on click outside
document.addEventListener('click', (e) => {
    if (metadataExpanded && !e.target.closest('.topnav-source')) {
        metadataExpanded = false;
        m.redraw();
    }
});

// Top navigation bar component
const TopNav = {
    view({ attrs }) {
        const sectionRoute = attrs.sectionRoute;
        const groups = attrs.groups;
        const sectionHeatmapData = heatmapDataCache.get(sectionRoute);

        // Build metadata key/value pairs
        const metaEntries = [];
        if (attrs.source) metaEntries.push(['Source', attrs.source]);
        if (attrs.version) metaEntries.push(['Version', attrs.version]);
        if (attrs.interval) metaEntries.push(['Interval', formatInterval(attrs.interval)]);
        if (!liveMode && attrs.filesize) metaEntries.push(['Size', formatSize(attrs.filesize)]);
        if (attrs.start_time != null && attrs.end_time != null) {
            metaEntries.push(['Duration', formatDuration((attrs.end_time - attrs.start_time) / 1000)]);
        }
        if (attrs.num_series != null) metaEntries.push(['Series', attrs.num_series.toLocaleString()]);

        return m('div#topnav', [
            m('div.logo', [
                'REZOLUS',
                liveMode && m('span.live-indicator', {
                    class: recording ? 'recording' : 'stopped',
                }, recording ? 'REC' : 'STOPPED'),
            ]),
            attrs.filename && m('div.topnav-source', {
                onclick: () => { metadataExpanded = !metadataExpanded; },
            }, [
                m('span.topnav-source-name', attrs.filename),
                m('span.topnav-source-chevron', { class: metadataExpanded ? 'expanded' : '' }, '\u25BE'),
                metadataExpanded && metaEntries.length > 0 && m('div.topnav-meta-table', [
                    m('div.topnav-meta-row.topnav-meta-header',
                        metaEntries.map(([key]) => m('span', key)),
                    ),
                    m('div.topnav-meta-row.topnav-meta-values',
                        metaEntries.map(([, val]) => m('span', val)),
                    ),
                ]),
            ]),
            m('div.topnav-actions', [
                // Transport controls (live mode only)
                liveMode && m('div.transport-controls', [
                    m('button.transport-btn.record-btn', {
                        onclick: startRecording,
                        title: 'Start new recording (clears current data)',
                        disabled: recording,
                    }, m.trust('<svg width="16" height="16" viewBox="0 0 16 16" fill="currentColor"><circle cx="8" cy="8" r="6"/></svg>')),
                    m('button.transport-btn.stop-btn', {
                        onclick: stopRecording,
                        title: 'Stop recording',
                        disabled: !recording,
                    }, m.trust('<svg width="16" height="16" viewBox="0 0 16 16" fill="currentColor"><rect x="2" y="2" width="12" height="12" rx="1"/></svg>')),
                    m('button.transport-btn.save-btn', {
                        onclick: saveCapture,
                        title: 'Save capture as parquet',
                    }, m.trust('<svg width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M8 2v8m0 0l-3-3m3 3l3-3"/><path d="M2 11v2a1 1 0 001 1h10a1 1 0 001-1v-2"/></svg>')),
                ]),

                // Separator between transport and view controls
                liveMode && m('div.topnav-separator'),

                // Heatmap/Percentile action toggle (global, visible when section has histogram charts)
                (() => {
                    const hasHistogramCharts = (groups || []).some(g =>
                        (g.plots || []).some(p => p.promql_query && p.promql_query.includes('histogram_percentiles'))
                    );
                    return m('button', {
                        onclick: async () => {
                            heatmapEnabled = !heatmapEnabled;
                            if (heatmapEnabled && (!sectionHeatmapData || sectionHeatmapData.size === 0)) {
                                await fetchSectionHeatmapData(sectionRoute, groups);
                            } else {
                                m.redraw();
                            }
                        },
                        disabled: heatmapLoading || !hasHistogramCharts,
                    }, heatmapLoading ? 'LOADING...' : (heatmapEnabled ? 'SHOW PERCENTILES' : 'SHOW HEATMAPS'));
                })(),

                m('button', {
                    onclick: () => chartsState.resetZoom(),
                    disabled: chartsState.isDefaultZoom(),
                }, 'RESET ZOOM'),
            ]),
        ]);
    },
};

// Count plots with non-empty data across groups.
const countCharts = (groups) => {
    let total = 0;
    let withData = 0;
    for (const group of groups || []) {
        for (const plot of group.plots || []) {
            total++;
            if (plot.data && plot.data.length >= 1 && plot.data[0] && plot.data[0].length > 0) {
                withData++;
            }
        }
    }
    return { total, withData };
};

// Sidebar component
const Sidebar = {
    view({ attrs }) {
        // Separate Query Explorer from other sections
        const regularSections = attrs.sections.filter(
            (s) => s.name !== 'Query Explorer',
        );
        const queryExplorer = attrs.sections.find(
            (s) => s.name === 'Query Explorer',
        );

        // Find the first non-overview section to use as samplers header
        const overviewSection = regularSections.find((s) => s.name === 'Overview');
        const samplerSections = regularSections.filter((s) => s.name !== 'Overview');

        return m('div#sidebar', [
            // Overview section first (if exists)
            overviewSection && m(
                m.route.Link,
                {
                    class: attrs.activeSection === overviewSection ? 'selected' : '',
                    href: overviewSection.route,
                },
                overviewSection.name,
            ),

            // Samplers label
            samplerSections.length > 0 && m('div.sidebar-label', 'Samplers'),

            // Sampler sections
            samplerSections.map((section) => {
                const sectionKey = section.route.replace(/^\//, '');
                const cached = sectionResponseCache[sectionKey];
                const count = cached ? countCharts(cached.groups) : null;
                const label = count ? `${section.name} (${count.withData})` : section.name;
                return m(
                    m.route.Link,
                    {
                        class:
                            attrs.activeSection === section ? 'selected' : '',
                        href: section.route,
                    },
                    label,
                );
            }),

            // Separator and Query Explorer if it exists
            queryExplorer && [
                m('div.sidebar-separator'),
                m(
                    m.route.Link,
                    {
                        class:
                            attrs.activeSection === queryExplorer
                                ? 'selected query-explorer-link'
                                : 'query-explorer-link',
                        href: queryExplorer.route,
                    },
                    [m('span.arrow', '→'), ' ', queryExplorer.name],
                ),
            ],
        ]);
    },
};

// Status bar component
const StatusBar = {
    view({ attrs }) {
        const { source, version, interval, filesize } = attrs;
        return m('div#status-bar', [
            source && m('span.status-item', [
                m('span.status-label', 'Source'),
                source,
            ]),
            version && m('span.status-item', [
                m('span.status-label', 'Version'),
                version,
            ]),
            interval && m('span.status-item', [
                m('span.status-label', 'Interval'),
                formatInterval(interval),
            ]),
            !liveMode && filesize && m('span.status-item', [
                m('span.status-label', 'Size'),
                formatSize(filesize),
            ]),
        ]);
    },
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
            }),
            m('main', [
                m(Sidebar, {
                    activeSection,
                    sections,
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

// Query Explorer component for running custom PromQL queries
const QueryExplorer = {
    oninit(vnode) {
        vnode.state.query = '';
        vnode.state.result = null;
        vnode.state.error = null;
        vnode.state.loading = false;
        vnode.state.queryHistory = JSON.parse(
            localStorage.getItem('promql_history') || '[]',
        );
        // Create a separate ChartsState for query explorer to prevent flickering
        // when other sections load in the background
        vnode.state.queryChartsState = new ChartsState();

        // Format a PromQL value (handle both instant and range values)
        vnode.state.formatValue = (value) => {
            if (Array.isArray(value) && value.length === 2) {
                // Instant query result: [timestamp, value]
                const v = parseFloat(value[1]);
                if (Math.abs(v) < 0.01 && v !== 0) {
                    return v.toExponential(3);
                } else if (Math.abs(v) >= 1000000) {
                    return (v / 1000000).toFixed(2) + 'M';
                } else if (Math.abs(v) >= 1000) {
                    return (v / 1000).toFixed(2) + 'K';
                } else {
                    return v.toFixed(3);
                }
            }
            return String(value);
        };

        // Bind the executeQuery method to the state
        vnode.state.executeQuery = async () => {
            if (!vnode.state.query.trim()) return;

            vnode.state.loading = true;
            vnode.state.error = null;

            try {
                // First, get the data time range
                const metadataResponse = await m.request({
                    method: 'GET',
                    url: '/api/v1/metadata',
                    withCredentials: true,
                });

                if (metadataResponse.status !== 'success') {
                    throw new Error('Failed to get metadata');
                }

                const minTime = metadataResponse.data.minTime;
                const maxTime = metadataResponse.data.maxTime;
                const duration = maxTime - minTime;

                // Use a reasonable time window - either 1 hour or the full range if it's shorter
                const windowDuration = Math.min(3600, duration); // 1 hour max
                const start = Math.max(minTime, maxTime - windowDuration);
                const step = Math.max(1, Math.floor(windowDuration / 100)); // About 100 data points

                const url = `/api/v1/query_range?query=${encodeURIComponent(vnode.state.query)}&start=${start}&end=${maxTime}&step=${step}`;

                const response = await m.request({
                    method: 'GET',
                    url,
                    withCredentials: true,
                });

                vnode.state.result = response;
            } catch (error) {
                vnode.state.error = error.message || 'Query failed';
            }

            vnode.state.loading = false;

            // Add to history if successful and no error
            if (
                !vnode.state.error &&
                vnode.state.result &&
                !vnode.state.queryHistory.includes(vnode.state.query)
            ) {
                vnode.state.queryHistory.unshift(vnode.state.query);
                // Keep only last 20 queries
                vnode.state.queryHistory = vnode.state.queryHistory.slice(
                    0,
                    20,
                );
                localStorage.setItem(
                    'promql_history',
                    JSON.stringify(vnode.state.queryHistory),
                );
            }

            m.redraw();
        };
    },

    oncreate(vnode) {
        // In live mode, re-execute the active query on the refresh interval
        if (liveMode) {
            vnode.state.liveInterval = setInterval(() => {
                if (recording && vnode.state.query && vnode.state.result && !vnode.state.loading) {
                    vnode.state.executeQuery();
                }
            }, 5000);
        }
    },

    onremove(vnode) {
        if (vnode.state.liveInterval) {
            clearInterval(vnode.state.liveInterval);
        }
        // Clean up the query explorer's chart state when component is removed
        if (vnode.state.queryChartsState) {
            vnode.state.queryChartsState.clear();
        }
    },

    view(vnode) {
        return m('div.query-explorer', [
            m('div.query-input-section', [
                m('h2', 'PromQL Query Explorer'),
                m('div.query-input-wrapper', [
                    m('textarea.query-input', {
                        placeholder:
                            'Enter a PromQL query (e.g., sum(rate(syscall[5m])) or rate(network_bytes{direction="transmit"}[5m]))',
                        value: vnode.state.query,
                        oninput: (e) => (vnode.state.query = e.target.value),
                        onkeydown: (e) => {
                            if (e.key === 'Enter' && e.ctrlKey) {
                                vnode.state.executeQuery();
                            }
                        },
                    }),
                    m(
                        'button.execute-btn',
                        {
                            onclick: () => vnode.state.executeQuery(),
                            disabled: vnode.state.loading,
                        },
                        vnode.state.loading
                            ? 'Running...'
                            : 'Execute Query (Ctrl+Enter)',
                    ),
                ]),

                // Query history dropdown
                vnode.state.queryHistory.length > 0 &&
                    m('div.query-history', [
                        m('h3', 'Recent Queries'),
                        m(
                            'select.history-select',
                            {
                                onchange: (e) => {
                                    vnode.state.query = e.target.value;
                                },
                            },
                            [
                                m(
                                    'option',
                                    { value: '' },
                                    '-- Select from history --',
                                ),
                                vnode.state.queryHistory.map((q) =>
                                    m(
                                        'option',
                                        { value: q },
                                        q.length > 80
                                            ? q.substring(0, 77) + '...'
                                            : q,
                                    ),
                                ),
                            ],
                        ),
                    ]),
            ]),

            // Error display
            vnode.state.error &&
                m('div.error-message', [
                    m('strong', 'Error: '),
                    vnode.state.error,
                ]),

            // Result display
            vnode.state.result &&
                m('div.query-result', [
                    m('h3', 'Result'),
                    vnode.state.result.status === 'success'
                        ? m('div.result-data', [
                              vnode.state.result.data &&
                              vnode.state.result.data.result &&
                              vnode.state.result.data.result.length > 0
                                  ? [
                                        // Create a chart if we have time series data
                                        (() => {
                                            // Check if we have multiple series (from by() clause)
                                            const resultData =
                                                vnode.state.result.data.result;
                                            const hasMultipleSeries =
                                                resultData.length > 1;

                                            if (hasMultipleSeries) {
                                                // Multi-series chart (e.g., from "sum by (cpu) (...)")
                                                const seriesNames = [];
                                                const allData = [];
                                                let timestamps = null;

                                                resultData.forEach(
                                                    (item, idx) => {
                                                        if (
                                                            item.values &&
                                                            Array.isArray(
                                                                item.values,
                                                            )
                                                        ) {
                                                            // Extract series name from metric labels
                                                            let seriesName =
                                                                'Series ' +
                                                                (idx + 1);
                                                            if (item.metric) {
                                                                // Get all labels except __name__, sorted properly
                                                                const labels = [];
                                                                let hasId = false;

                                                                // First check if 'id' label exists
                                                                if (item.metric.id !== undefined) {
                                                                    labels.push(`id=${item.metric.id}`);
                                                                    hasId = true;
                                                                }

                                                                // Then add all other labels (except __name__, id, and metadata labels), sorted alphabetically
                                                                const excludedLabels = ['__name__', 'id', 'metric', 'metric_type', 'unit'];
                                                                const otherLabels = Object.entries(item.metric)
                                                                    .filter(([key, _]) => !excludedLabels.includes(key))
                                                                    .sort((a, b) => a[0].localeCompare(b[0]))
                                                                    .map(([key, value]) => `${key}=${value}`);

                                                                labels.push(...otherLabels);

                                                                if (labels.length > 0) {
                                                                    seriesName = labels.join(', ');
                                                                }
                                                            }
                                                            seriesNames.push(
                                                                seriesName,
                                                            );

                                                            // Extract timestamps (should be same for all series)
                                                            if (!timestamps) {
                                                                timestamps =
                                                                    item.values.map(
                                                                        ([
                                                                            ts,
                                                                            _,
                                                                        ]) =>
                                                                            ts,
                                                                    );
                                                                allData.push(
                                                                    timestamps,
                                                                );
                                                            }

                                                            // Extract values for this series
                                                            const values =
                                                                item.values.map(
                                                                    ([
                                                                        _,
                                                                        val,
                                                                    ]) =>
                                                                        parseFloat(
                                                                            val,
                                                                        ),
                                                                );
                                                            allData.push(
                                                                values,
                                                            );
                                                        }
                                                    },
                                                );

                                                if (allData.length > 1) {
                                                    // Use a stable chart key based on the query to prevent recreating
                                                    const chartKey = `query-chart-multi-${vnode.state.query}`;

                                                    const chartSpec = {
                                                        opts: {
                                                            id: chartKey,
                                                            title: 'Query Result',
                                                            style: 'multi',
                                                        },
                                                        data: allData,
                                                        series_names:
                                                            seriesNames,
                                                    };

                                                    return m(
                                                        'div.query-chart',
                                                        { key: chartKey },
                                                        [
                                                            m(Chart, {
                                                                spec: chartSpec,
                                                                chartsState: vnode.state.queryChartsState,
                                                            }),
                                                        ],
                                                    );
                                                }
                                            } else {
                                                // Single series chart
                                                const timestamps = [];
                                                const values = [];

                                                resultData.forEach(
                                                    (item, idx) => {
                                                        if (
                                                            item.values &&
                                                            Array.isArray(
                                                                item.values,
                                                            )
                                                        ) {
                                                            // Matrix result - multiple time points
                                                            item.values.forEach(
                                                                ([
                                                                    timestamp,
                                                                    value,
                                                                ]) => {
                                                                    timestamps.push(
                                                                        timestamp,
                                                                    );
                                                                    values.push(
                                                                        parseFloat(
                                                                            value,
                                                                        ),
                                                                    );
                                                                },
                                                            );
                                                        } else if (
                                                            item.value &&
                                                            Array.isArray(
                                                                item.value,
                                                            ) &&
                                                            item.value
                                                                .length === 2
                                                        ) {
                                                            // Vector result - single time point
                                                            timestamps.push(
                                                                item.value[0],
                                                            );
                                                            values.push(
                                                                parseFloat(
                                                                    item
                                                                        .value[1],
                                                                ),
                                                            );
                                                        }
                                                    },
                                                );

                                                if (timestamps.length > 0) {
                                                    const chartData = [
                                                        timestamps,
                                                        values,
                                                    ];
                                                    // Use a stable chart key based on the query to prevent recreating
                                                    const chartKey = `query-chart-line-${vnode.state.query}`;

                                                    const chartSpec = {
                                                        opts: {
                                                            id: chartKey,
                                                            title: 'Query Result',
                                                            style: 'line',
                                                        },
                                                        data: chartData,
                                                    };

                                                    return m(
                                                        'div.query-chart',
                                                        { key: chartKey },
                                                        [
                                                            m(Chart, {
                                                                spec: chartSpec,
                                                                chartsState: vnode.state.queryChartsState,
                                                            }),
                                                        ],
                                                    );
                                                }
                                            }
                                            return null;
                                        })(),
                                    ]
                                  : m('p', 'No data returned'),
                          ])
                        : m(
                              'div.error-message',
                              'Query failed: ' +
                                  (vnode.state.result.error || 'Unknown error'),
                          ),
                ]),

            // Example queries section
            m('div.example-queries', [
                m('h3', 'Example Queries'),
                m('ul', [
                    m(
                        'li',
                        m(
                            'code',
                            {
                                onclick: () => {
                                    vnode.state.query =
                                        'sum(irate(syscall[5m]))';
                                    vnode.state.executeQuery();
                                },
                            },
                            'sum(irate(syscall[5m]))',
                        ),
                    ),
                    m(
                        'li',
                        m(
                            'code',
                            {
                                onclick: () => {
                                    vnode.state.query =
                                        'sum(irate(cpu_usage[5m])) / 1e9 / cpu_cores';
                                    vnode.state.executeQuery();
                                },
                            },
                            'sum(irate(cpu_usage[5m])) / 1e9 / cpu_cores',
                        ),
                        ' - Average CPU utilization (0-1)',
                    ),
                    m(
                        'li',
                        m(
                            'code',
                            {
                                onclick: () => {
                                    vnode.state.query =
                                        'sum(irate(network_bytes{direction="transmit"}[5m])) * 8';
                                    vnode.state.executeQuery();
                                },
                            },
                            'sum(irate(network_bytes{direction="transmit"}[5m])) * 8',
                        ),
                        ' - Network transmit (bits/sec)',
                    ),
                    m(
                        'li',
                        m(
                            'code',
                            {
                                onclick: () => {
                                    vnode.state.query =
                                        'sum(irate(cpu_instructions[5m])) / sum(irate(cpu_cycles[5m]))';
                                    vnode.state.executeQuery();
                                },
                            },
                            'sum(irate(cpu_instructions[5m])) / sum(irate(cpu_cycles[5m]))',
                        ),
                        ' - IPC (Instructions per Cycle)',
                    ),
                    m(
                        'li',
                        m(
                            'code',
                            {
                                onclick: () => {
                                    vnode.state.query =
                                        'sum by (id) (irate(cpu_usage[5m])) / 1e9';
                                    vnode.state.executeQuery();
                                },
                            },
                            'sum by (id) (irate(cpu_usage[5m])) / 1e9',
                        ),
                        ' - Per-CPU usage (cores)',
                    ),
                    m(
                        'li',
                        m(
                            'code',
                            {
                                onclick: () => {
                                    vnode.state.query =
                                        'sum by (state) (irate(cpu_usage[5m])) / 1e9';
                                    vnode.state.executeQuery();
                                },
                            },
                            'sum by (state) (irate(cpu_usage[5m])) / 1e9',
                        ),
                        ' - CPU by state (user/system)',
                    ),
                    m(
                        'li',
                        m(
                            'code',
                            {
                                onclick: () => {
                                    vnode.state.query =
                                        'sum by (direction) (irate(network_bytes[5m]))';
                                    vnode.state.executeQuery();
                                },
                            },
                            'sum by (direction) (irate(network_bytes[5m]))',
                        ),
                        ' - Network by direction',
                    ),
                    m(
                        'li',
                        m(
                            'code',
                            {
                                onclick: () => {
                                    vnode.state.query =
                                        'histogram_quantile(0.95, scheduler_runqueue_latency)';
                                    vnode.state.executeQuery();
                                },
                            },
                            'histogram_quantile(0.95, scheduler_runqueue_latency)',
                        ),
                        ' - P95 scheduler latency',
                    ),
                    m(
                        'li',
                        m(
                            'code',
                            {
                                onclick: () => {
                                    vnode.state.query =
                                        'sum by (op) (irate(syscall[5m]))';
                                    vnode.state.executeQuery();
                                },
                            },
                            'sum by (op) (irate(syscall[5m]))',
                        ),
                        ' - Syscalls by operation',
                    ),
                ]),
            ]),
        ]);
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
                m(QueryExplorer),
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
                m(CgroupSelector, { groups: attrs.groups }),
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

        return m('div#section-content', [
            m('h1.section-title', titleText),
            m(
                'div#groups',
                attrs.groups.map((group) => m(Group, { ...group, sectionRoute, sectionName, interval })),
            ),
        ]);
    },
};

// Cgroup selector component for selecting which cgroups to view individually
const CgroupSelector = {
    oninit(vnode) {
        vnode.state.selectedCgroups = new Set();
        vnode.state.availableCgroups = new Set();
        vnode.state.loading = true;
        vnode.state.error = null;

        // Fetch available cgroups from the data
        this.fetchAvailableCgroups(vnode);
    },

    async fetchAvailableCgroups(vnode) {
        try {
            // Try multiple queries to find cgroups
            const queries = [
                'sum by (name) (cgroup_cpu_usage)',
                'group by (name) (cgroup_cpu_usage)',
                'cgroup_cpu_usage',
                'sum by (name) (rate(cgroup_cpu_usage[1m]))',
            ];

            let cgroups = new Set();
            let foundData = false;

            for (const query of queries) {
                try {
                    const result = await executePromQLRangeQuery(query);

                    if (
                        result.status === 'success' &&
                        result.data &&
                        result.data.result &&
                        result.data.result.length > 0
                    ) {
                        result.data.result.forEach((series) => {
                            // Check different possible locations for the name label
                            if (series.metric) {
                                if (series.metric.name) {
                                    cgroups.add(series.metric.name);
                                    foundData = true;
                                }
                                // Also check all other labels that might contain cgroup names
                                Object.entries(series.metric).forEach(
                                    ([key, value]) => {
                                        if (
                                            key === 'name' ||
                                            key.includes('cgroup') ||
                                            key === 'container'
                                        ) {
                                            if (value && value !== '') {
                                                cgroups.add(value);
                                                foundData = true;
                                            }
                                        }
                                    },
                                );
                            }
                        });

                        if (foundData) {
                            break;
                        }
                    }
                } catch (queryError) {
                    console.warn(`Query failed: ${query}`, queryError);
                }
            }

            if (!foundData) {
                // If no cgroup metrics found, try to extract from any plots that have cgroup in the query

                // Look for any existing cgroup data in the plots
                if (vnode.attrs.groups) {
                    vnode.attrs.groups.forEach((group) => {
                        if (group.plots) {
                            group.plots.forEach((plot) => {
                                if (
                                    plot.promql_query &&
                                    plot.promql_query.includes('cgroup')
                                ) {
                                    // Try to extract cgroup names from the query results if they exist
                                }
                            });
                        }
                    });
                }

                // If no cgroup data found, show error with empty list
                if (cgroups.size === 0) {
                    vnode.state.error = 'No cgroup data found';
                }
            }

            vnode.state.availableCgroups = cgroups;
            vnode.state.loading = false;
            m.redraw();
        } catch (error) {
            console.error('Failed to fetch available cgroups:', error);
            vnode.state.error = 'Failed to load cgroups: ' + error.message;
            vnode.state.loading = false;

            // Show empty list on error
            vnode.state.availableCgroups = new Set();
            m.redraw();
        }
    },

    async updateQueries(vnode) {
        // Cancel any in-flight requests
        if (vnode.state.updateInProgress) {
            vnode.state.cancelUpdate = true;
            return;
        }

        vnode.state.updateInProgress = true;
        vnode.state.cancelUpdate = false;

        // Update all PromQL queries with the selected cgroups
        const selectedArray = Array.from(vnode.state.selectedCgroups);
        // Build alternation pattern for Labels::matches
        // No escaping needed — Labels::matches uses simple string equality,
        // and cgroup names don't contain | which is the only special character.
        const selectedPattern =
            selectedArray.length > 1
                ? '(' + selectedArray.join('|') + ')'
                : selectedArray.length === 1
                  ? selectedArray[0]
                  : ''; // Empty string matches nothing

        // Store globally so live refresh can re-apply the pattern
        activeCgroupPattern = selectedPattern || null;

        // Store the original queries if not already stored
        if (!vnode.state.originalQueries) {
            vnode.state.originalQueries = new Map();
            vnode.attrs.groups.forEach((group, groupIdx) => {
                if (group.plots) {
                    group.plots.forEach((plot, plotIdx) => {
                        if (plot.promql_query) {
                            const key = `${groupIdx}-${plotIdx}`;
                            vnode.state.originalQueries.set(
                                key,
                                plot.promql_query,
                            );
                        }
                    });
                }
            });
        }

        // Track the update generation to ignore stale results
        const updateGeneration = ++vnode.state.updateGeneration || 1;
        vnode.state.updateGeneration = updateGeneration;

        // Collect all plots that need updating
        const plotsToUpdate = [];
        vnode.attrs.groups.forEach((group, groupIdx) => {
            if (group.plots) {
                group.plots.forEach((plot, plotIdx) => {
                    const key = `${groupIdx}-${plotIdx}`;
                    const originalQuery = vnode.state.originalQueries.get(key);

                    if (
                        originalQuery &&
                        originalQuery.includes('__SELECTED_CGROUPS__')
                    ) {
                        const updatedQuery = substituteCgroupPattern(
                            originalQuery,
                            selectedPattern || null,
                        );
                        plotsToUpdate.push({
                            plot,
                            updatedQuery,
                            originalQuery,
                        });
                    }
                });
            }
        });

        // Execute queries in batches to avoid overwhelming the server
        const BATCH_SIZE = 5;
        for (let i = 0; i < plotsToUpdate.length; i += BATCH_SIZE) {
            // Check if this update was cancelled
            if (
                vnode.state.cancelUpdate ||
                vnode.state.updateGeneration !== updateGeneration
            ) {
                vnode.state.updateInProgress = false;
                return;
            }

            const batch = plotsToUpdate.slice(i, i + BATCH_SIZE);
            const promises = batch.map(
                async ({ plot, updatedQuery, originalQuery }) => {
                    plot.promql_query = updatedQuery;

                    try {
                        const result =
                            await executePromQLRangeQuery(updatedQuery);

                        // Check if this result is still relevant
                        if (vnode.state.updateGeneration !== updateGeneration) {
                            return;
                        }

                        if (result.status === 'success' && result.data) {
                            // Update the plot data directly
                            if (
                                result.data.result &&
                                result.data.result.length > 0
                            ) {
                                // Handle multi-series data
                                if (
                                    plot.opts.style === 'multi' ||
                                    plot.opts.style === 'heatmap'
                                ) {
                                    const seriesData = [];
                                    const seriesNames = [];

                                    result.data.result.forEach((series) => {
                                        if (
                                            series.values &&
                                            series.values.length > 0
                                        ) {
                                            const timestamps =
                                                series.values.map(
                                                    ([ts, _]) => ts,
                                                );
                                            const values = series.values.map(
                                                ([_, val]) => parseFloat(val),
                                            );

                                            // Use the first series for timestamps
                                            if (seriesData.length === 0) {
                                                seriesData.push(timestamps);
                                            }
                                            seriesData.push(values);

                                            // Extract series name from metric labels
                                            const name =
                                                series.metric.name ||
                                                series.metric.id ||
                                                series.metric.__name__ ||
                                                `Series ${seriesNames.length + 1}`;
                                            seriesNames.push(name);
                                        }
                                    });

                                    if (seriesData.length > 1) {
                                        plot.data = seriesData;
                                        plot.series_names = seriesNames;
                                    } else {
                                        plot.data = [];
                                    }
                                } else {
                                    // Single series data
                                    const sample = result.data.result[0];
                                    if (
                                        sample.values &&
                                        Array.isArray(sample.values)
                                    ) {
                                        const timestamps = sample.values.map(
                                            ([ts, _]) => ts,
                                        );
                                        const values = sample.values.map(
                                            ([_, val]) => parseFloat(val),
                                        );
                                        plot.data = [timestamps, values];
                                    } else {
                                        plot.data = [];
                                    }
                                }
                            } else {
                                plot.data = [];
                            }
                        } else {
                            console.warn(`No data for query: ${updatedQuery}`);
                            plot.data = [];
                        }
                    } catch (error) {
                        console.error(
                            `Failed to execute query for plot ${plot.opts.title}:`,
                            error,
                        );
                        plot.data = [];
                    }
                },
            );

            // Wait for this batch to complete before starting the next
            await Promise.all(promises);
        }

        // Only redraw if this update is still current
        if (vnode.state.updateGeneration === updateGeneration) {
            m.redraw();
        }

        vnode.state.updateInProgress = false;
    },

    addCgroup(vnode, cgroup) {
        vnode.state.selectedCgroups.add(cgroup);
        this.debouncedUpdateQueries(vnode);
    },

    removeCgroup(vnode, cgroup) {
        vnode.state.selectedCgroups.delete(cgroup);
        this.debouncedUpdateQueries(vnode);
    },

    debouncedUpdateQueries(vnode) {
        // Cancel any pending update
        if (vnode.state.updateTimer) {
            clearTimeout(vnode.state.updateTimer);
        }

        // Schedule a new update after a short delay
        vnode.state.updateTimer = setTimeout(() => {
            this.updateQueries(vnode);
        }, 300); // 300ms debounce
    },

    view(vnode) {
        const unselectedCgroups = Array.from(vnode.state.availableCgroups)
            .filter((cg) => !vnode.state.selectedCgroups.has(cg))
            .sort();
        const selectedCgroups = Array.from(vnode.state.selectedCgroups).sort();

        // Track which items are selected in the lists
        if (!vnode.state.leftSelected) vnode.state.leftSelected = new Set();
        if (!vnode.state.rightSelected) vnode.state.rightSelected = new Set();

        return m('div.cgroup-selector', [
            m('h3', 'Cgroup Selection'),
            vnode.state.error && m('div.error-message', vnode.state.error),
            m('div.selector-container', [
                m('div.selector-column', [
                    m('h4', 'Available Cgroups (Aggregate)'),
                    m(
                        'select.cgroup-select[multiple]',
                        {
                            size: 10,
                            onchange: (e) => {
                                vnode.state.leftSelected.clear();
                                Array.from(e.target.selectedOptions).forEach(
                                    (option) => {
                                        vnode.state.leftSelected.add(
                                            option.value,
                                        );
                                    },
                                );
                            },
                        },
                        vnode.state.loading
                            ? [m('option[disabled]', 'Loading cgroups...')]
                            : unselectedCgroups.length === 0
                              ? [m('option[disabled]', 'No cgroups available')]
                              : unselectedCgroups.map((cgroup) =>
                                    m(
                                        'option',
                                        {
                                            value: cgroup,
                                            selected:
                                                vnode.state.leftSelected.has(
                                                    cgroup,
                                                ),
                                        },
                                        cgroup,
                                    ),
                                ),
                    ),
                ]),
                m('div.selector-controls', [
                    m(
                        'button',
                        {
                            title: 'Move selected to individual',
                            disabled: vnode.state.leftSelected.size === 0,
                            onclick: () => {
                                // Batch add cgroups
                                vnode.state.leftSelected.forEach((cg) => {
                                    vnode.state.selectedCgroups.add(cg);

                                });
                                vnode.state.leftSelected.clear();
                                // Single update for all additions
                                this.debouncedUpdateQueries(vnode);
                            },
                        },
                        '>',
                    ),
                    m(
                        'button',
                        {
                            title: 'Move all to individual',
                            disabled: unselectedCgroups.length === 0,
                            onclick: () => {
                                // Batch add all unselected cgroups
                                unselectedCgroups.forEach((cg) => {
                                    vnode.state.selectedCgroups.add(cg);

                                });
                                vnode.state.leftSelected.clear();
                                // Single update for all additions
                                this.debouncedUpdateQueries(vnode);
                            },
                        },
                        '>>',
                    ),
                    m(
                        'button',
                        {
                            title: 'Move all to aggregate',
                            disabled: selectedCgroups.length === 0,
                            onclick: () => {
                                // Batch remove all selected cgroups
                                selectedCgroups.forEach((cg) => {
                                    vnode.state.selectedCgroups.delete(cg);

                                });
                                vnode.state.rightSelected.clear();
                                // Single update for all removals
                                this.debouncedUpdateQueries(vnode);
                            },
                        },
                        '<<',
                    ),
                    m(
                        'button',
                        {
                            title: 'Move selected to aggregate',
                            disabled: vnode.state.rightSelected.size === 0,
                            onclick: () => {
                                // Batch remove cgroups
                                vnode.state.rightSelected.forEach((cg) => {
                                    vnode.state.selectedCgroups.delete(cg);

                                });
                                vnode.state.rightSelected.clear();
                                // Single update for all removals
                                this.debouncedUpdateQueries(vnode);
                            },
                        },
                        '<',
                    ),
                ]),
                m('div.selector-column', [
                    m('h4', 'Individual Cgroups'),
                    m(
                        'select.cgroup-select[multiple]',
                        {
                            size: 10,
                            onchange: (e) => {
                                vnode.state.rightSelected.clear();
                                Array.from(e.target.selectedOptions).forEach(
                                    (option) => {
                                        vnode.state.rightSelected.add(
                                            option.value,
                                        );
                                    },
                                );
                            },
                        },
                        selectedCgroups.length === 0
                            ? [m('option[disabled]', 'No cgroups selected')]
                            : selectedCgroups.map((cgroup) =>
                                  m(
                                      'option',
                                      {
                                          value: cgroup,
                                          selected:
                                              vnode.state.rightSelected.has(
                                                  cgroup,
                                              ),
                                      },
                                      cgroup,
                                  ),
                              ),
                    ),
                ]),
            ]),
            m('div.selector-info', [
                m(
                    'small',
                    `${unselectedCgroups.length} available, ${selectedCgroups.length} selected`,
                ),
            ]),
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
                                m(Chart, { spec: heatmapSpec, chartsState, interval }),
                                expandLink(spec),
                            ]);
                        }

                        const prefixedSpec = { ...spec, opts: prefixTitle(spec.opts), noCollapse: attrs.noCollapse };
                        return m('div.chart-wrapper', [
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

// Fetch time range metadata from the backend (cached per refresh cycle)
let cachedMetadata = null;

const fetchMetadata = async () => {
    const metadataResponse = await m.request({
        method: 'GET',
        url: '/api/v1/metadata',
        withCredentials: true,
        background: true, // Prevent auto-redraw during refresh
    });

    if (metadataResponse.status !== 'success') {
        throw new Error('Failed to get metadata');
    }

    return metadataResponse.data;
};

/**
 * Substitute __SELECTED_CGROUPS__ in a PromQL query.
 *
 * - For =~ (positive match): replaces the placeholder with the pattern.
 * - For !~ (negative match): the PromQL engine doesn't support !~, so
 *   the entire label matcher is stripped. This means aggregate charts
 *   show the total rather than "total minus selected".
 * - When pattern is null (no selection): =~ matchers are left as-is
 *   (query will return empty), !~ matchers are stripped (query returns all).
 */
const substituteCgroupPattern = (query, pattern) => {
    // Strip !~ matchers entirely — the engine can't handle them.
    // Match patterns like: {name!~"..."} or ,name!~"..."  within braces.
    query = query.replace(/,?\s*name!~"[^"]*"/g, '');
    // Clean up empty braces left behind: metric{} -> metric
    query = query.replace(/\{\s*\}/g, '');

    if (pattern) {
        // Substitute =~ matchers with the actual pattern
        query = query.replace(/__SELECTED_CGROUPS__/g, pattern);
    }
    return query;
};

// Execute a PromQL range query using pre-fetched or cached metadata
const executePromQLRangeQuery = async (query, metadata) => {
    // Use provided metadata, cached metadata, or fetch fresh
    const meta = metadata || cachedMetadata || await fetchMetadata();

    const minTime = meta.minTime;
    const maxTime = meta.maxTime;
    const duration = maxTime - minTime;

    // Use a reasonable time window - either 1 hour or the full range if it's shorter
    const windowDuration = Math.min(3600, duration); // 1 hour max
    const start = Math.max(minTime, maxTime - windowDuration);
    // Target ~500 data points for good LTTB downsampling in the frontend
    const step = Math.max(1, Math.floor(windowDuration / 500));

    const url = `/api/v1/query_range?query=${encodeURIComponent(query)}&start=${start}&end=${maxTime}&step=${step}`;

    return m.request({
        method: 'GET',
        url,
        withCredentials: true,
        background: true, // Prevent auto-redraw during refresh
    });
};

// Apply a PromQL result to its plot, transforming into the chart data format.
const applyResultToPlot = (plot, result) => {
    if (
        result.status === 'success' &&
        result.data &&
        result.data.result &&
        result.data.result.length > 0
    ) {
        const hasMultipleSeries =
            result.data.result.length > 1 ||
            (plot.opts &&
                (plot.opts.style === 'multi' ||
                    plot.opts.style === 'scatter' ||
                    plot.opts.style === 'heatmap'));

        if (hasMultipleSeries) {
            if (plot.opts && plot.opts.style === 'heatmap') {
                // Transform to heatmap format: [time_index, cpu_id, value]
                const heatmapData = [];
                const timeSet = new Set();

                result.data.result.forEach((item) => {
                    if (item.values && Array.isArray(item.values)) {
                        item.values.forEach(([timestamp, _]) => {
                            timeSet.add(timestamp);
                        });
                    }
                });

                const timestamps = Array.from(timeSet).sort((a, b) => a - b);
                const timestampToIndex = new Map();
                timestamps.forEach((ts, idx) => {
                    timestampToIndex.set(ts, idx);
                });

                result.data.result.forEach((item, idx) => {
                    if (item.values && Array.isArray(item.values)) {
                        let cpuId = idx;
                        if (item.metric && item.metric.id) {
                            cpuId = parseInt(item.metric.id);
                        }

                        item.values.forEach(([timestamp, value]) => {
                            const timeIndex = timestampToIndex.get(timestamp);
                            heatmapData.push([
                                timeIndex,
                                cpuId,
                                parseFloat(value),
                            ]);
                        });
                    }
                });

                let minValue = Infinity;
                let maxValue = -Infinity;
                heatmapData.forEach(([_, __, value]) => {
                    minValue = Math.min(minValue, value);
                    maxValue = Math.max(maxValue, value);
                });

                plot.data = heatmapData;
                plot.time_data = timestamps;
                plot.min_value = minValue;
                plot.max_value = maxValue;
            } else {
                // Multi-series line chart data
                const allData = [];
                const seriesNames = [];
                let timestamps = null;

                result.data.result.forEach((item, idx) => {
                    if (item.values && Array.isArray(item.values)) {
                        let seriesName = 'Series ' + (idx + 1);
                        if (item.metric) {
                            for (const [key, value] of Object.entries(
                                item.metric,
                            )) {
                                if (key !== '__name__') {
                                    seriesName = value;
                                    break;
                                }
                            }
                        }

                        if (item.values.length > 0) {
                            seriesNames.push(seriesName);

                            if (!timestamps) {
                                timestamps = item.values.map(([ts, _]) => ts);
                                allData.push(timestamps);
                            }

                            const values = item.values.map(([_, val]) =>
                                parseFloat(val),
                            );
                            allData.push(values);
                        }
                    }
                });

                if (allData.length > 1) {
                    plot.data = allData;
                    plot.series_names = seriesNames;
                } else {
                    plot.data = [];
                }
            }
        } else {
            // Single series data
            const sample = result.data.result[0];
            if (sample.values && Array.isArray(sample.values)) {
                const timestamps = sample.values.map(([ts, _]) => ts);
                const values = sample.values.map(([_, val]) =>
                    parseFloat(val),
                );
                plot.data = [timestamps, values];
            } else {
                plot.data = [];
            }
        }
    } else {
        plot.data = [];
    }
};

// Process dashboard data — fire all PromQL queries in parallel.
const processDashboardData = async (data) => {
    const metadata = await fetchMetadata();
    cachedMetadata = metadata;

    // Collect all plots that need queries
    const queryPlots = [];
    for (const group of data.groups || []) {
        for (const plot of group.plots || []) {
            if (plot.promql_query) {
                // Skip cgroup placeholder queries when there's no active
                // selection — they'll either parse-error (!~) or return
                // empty results (=~), wasting a round-trip.
                if (plot.promql_query.includes('__SELECTED_CGROUPS__')) {
                    if (activeCgroupPattern) {
                        plot.promql_query = substituteCgroupPattern(
                            plot.promql_query,
                            activeCgroupPattern,
                        );
                    } else {
                        // No selection: aggregate queries should show all
                        // data (strip the !~ matcher), individual queries
                        // (=~) have nothing to show.
                        if (plot.promql_query.includes('!~')) {
                            plot.promql_query = substituteCgroupPattern(
                                plot.promql_query,
                                null,
                            );
                        } else {
                            continue;
                        }
                    }
                }
                queryPlots.push(plot);
            }
        }
    }

    // Fire all queries concurrently
    const results = await Promise.allSettled(
        queryPlots.map((plot) =>
            executePromQLRangeQuery(plot.promql_query, metadata),
        ),
    );

    // Apply results to their plots
    for (let i = 0; i < queryPlots.length; i++) {
        const plot = queryPlots[i];
        const outcome = results[i];
        if (outcome.status === 'fulfilled') {
            applyResultToPlot(plot, outcome.value);
        } else {
            console.error(
                `Failed to execute PromQL query "${plot.promql_query}":`,
                outcome.reason,
            );
            plot.data = [];
        }
    }

    return data;
};

// Fetch data for a section and cache it.
const preloadSection = async (section) => {
    // Skip preloading in live mode - data changes constantly
    if (liveMode || sectionResponseCache[section]) {
        return Promise.resolve();
    }

    const url = `/data/${section}.json`;
    console.time(`Preload ${url}`);
    const data = await m.request({
        method: 'GET',
        url,
        withCredentials: true,
    });

    const processedData = await processDashboardData(data);
    console.timeEnd(`Preload ${url}`);
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
        const promises = [processDashboardData(data)];
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

// Single-chart expanded view — opened in a new tab from the "Expand" link.
// Shows one chart at full width with an editable PromQL query input below it.
const SingleChartView = {
    oninit(vnode) {
        vnode.state.singleChartsState = new ChartsState();
        vnode.state.query = '';
        vnode.state.title = '';
        vnode.state.description = '';
        vnode.state.plot = null;
        vnode.state.loading = false;
        vnode.state.error = null;
    },

    onremove(vnode) {
        vnode.state.singleChartsState.clear();
    },

    view(vnode) {
        const data = vnode.attrs.data;
        const chartId = vnode.attrs.chartId;
        if (!data) return m('div', 'Loading...');

        // Find the plot by chart ID across all groups
        if (!vnode.state.plot) {
            for (const group of data.groups || []) {
                for (const plot of group.plots || []) {
                    if (plot.opts.id === chartId) {
                        vnode.state.plot = plot;
                        vnode.state.query = plot.promql_query || '';
                        vnode.state.title = plot.opts.title || '';
                        vnode.state.description = plot.opts.description || '';
                        break;
                    }
                }
                if (vnode.state.plot) break;
            }
        }

        const plot = vnode.state.plot;
        if (!plot) return m('div.single-chart-view', m('p', `Chart "${chartId}" not found`));

        const spec = {
            ...plot,
            opts: { ...plot.opts, title: vnode.state.title, description: vnode.state.description },
        };

        const executeQuery = async () => {
            if (!vnode.state.query.trim()) return;
            vnode.state.loading = true;
            vnode.state.error = null;

            try {
                const meta = await m.request({ method: 'GET', url: '/api/v1/metadata', withCredentials: true });
                const minTime = meta.data.minTime;
                const maxTime = meta.data.maxTime;
                const duration = maxTime - minTime;
                const step = Math.max(1, Math.floor(duration / 1000));

                const url = `/api/v1/query_range?query=${encodeURIComponent(vnode.state.query)}&start=${minTime}&end=${maxTime}&step=${step}`;
                const response = await m.request({ method: 'GET', url, withCredentials: true });

                if (response.status === 'success' && response.data && response.data.result) {
                    applyResultToPlot(plot, response);
                    // Force chart re-render by bumping the data reference
                    vnode.state.singleChartsState.clear();
                } else {
                    vnode.state.error = response.error || 'Query returned no data';
                }
            } catch (e) {
                vnode.state.error = e.message || 'Query failed';
            }

            vnode.state.loading = false;
            m.redraw();
        };

        const applyFields = () => {
            // Directly reconfigure every chart in this view so title/description update
            vnode.state.singleChartsState.charts.forEach(chart => {
                chart.spec = spec;
                chart.configureChartByType();
            });
        };

        return m('div.single-chart-view', [
            m('div.single-chart-header', [
                m('h1.section-title', 'Single Chart View'),
            ]),
            m('div.single-chart-container', [
                m(Chart, { spec, chartsState: vnode.state.singleChartsState, interval: data.interval }),
            ]),
            m('div.single-chart-fields', [
                m('div.single-chart-field', [
                    m('label', 'Title'),
                    m('div.field-input-row', [
                        m('input.field-input', {
                            type: 'text',
                            value: vnode.state.title,
                            oninput: (e) => { vnode.state.title = e.target.value; },
                            onkeydown: (e) => {
                                if (e.key === 'Enter') applyFields();
                            },
                        }),
                        m('button.field-apply-btn', { onclick: applyFields }, 'Apply'),
                    ]),
                ]),
                m('div.single-chart-field', [
                    m('label', 'Description'),
                    m('div.field-input-row', [
                        m('input.field-input', {
                            type: 'text',
                            value: vnode.state.description,
                            oninput: (e) => { vnode.state.description = e.target.value; },
                            onkeydown: (e) => {
                                if (e.key === 'Enter') applyFields();
                            },
                        }),
                        m('button.field-apply-btn', { onclick: applyFields }, 'Apply'),
                    ]),
                ]),
            ]),
            m('div.single-chart-query', [
                m('label', 'PromQL Query'),
                m('div.query-input-wrapper', [
                    m('textarea.query-input', {
                        value: vnode.state.query,
                        oninput: (e) => vnode.state.query = e.target.value,
                        onkeydown: (e) => {
                            if (e.key === 'Enter' && e.ctrlKey) executeQuery();
                        },
                        rows: 2,
                    }),
                    m('button.execute-btn', {
                        onclick: executeQuery,
                        disabled: vnode.state.loading,
                    }, vnode.state.loading ? 'Running...' : 'Execute (Ctrl+Enter)'),
                ]),
                vnode.state.error && m('div.error-message', vnode.state.error),
            ]),
        ]);
    },
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
                        }),
                        m('main.single-chart-main', [
                            m(SingleChartView, {
                                data,
                                chartId: decodeURIComponent(params.chartId),
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
                    const processedData = await processDashboardData(data);
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
                // Reset charts state.
                chartsState.clear();

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
            console.time(`Load ${url}`);
            return m
                .request({
                    method: 'GET',
                    url,
                    withCredentials: true,
                })
                .then(async (data) => {
                    console.timeEnd(`Load ${url}`);

                    // Process PromQL queries for this section
                    const processedData = await processDashboardData(data);
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
