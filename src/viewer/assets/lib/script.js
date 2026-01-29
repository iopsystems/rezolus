import { ChartsState, Chart } from './charts/chart.js';

// Top navigation bar component
const TopNav = {
    view() {
        return m('div#topnav', [
            m('div.logo', 'REZOLUS'),
            m('div.topnav-actions', [
                m('button', { onclick: () => chartsState.resetZoom() }, 'RESET ZOOM'),
            ]),
        ]);
    },
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
            samplerSections.map((section) =>
                m(
                    m.route.Link,
                    {
                        class:
                            attrs.activeSection === section ? 'selected' : '',
                        href: section.route,
                    },
                    section.name,
                ),
            ),

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

// Main component
const Main = {
    view({
        attrs: { activeSection, groups, sections, source, version, filename, interval, filesize },
    }) {
        // Format file size
        const formatSize = (bytes) => {
            if (!bytes) return '';
            if (bytes < 1024) return bytes + ' B';
            if (bytes < 1024 * 1024) return (bytes / 1024).toFixed(1) + ' KB';
            return (bytes / (1024 * 1024)).toFixed(1) + ' MB';
        };

        // Format interval
        const formatInterval = (ms) => {
            if (!ms) return '';
            if (ms < 1000) return ms + 'ms';
            return (ms / 1000).toFixed(0) + 's';
        };

        return m(
            'div',
            m(TopNav),
            m('header', [
                m('div.file-info', [
                    m('div.filename', filename || 'metrics.parquet'),
                    m('div.metadata', [
                        source && m('span', source),
                        version && m('span', version),
                        interval && m('span', formatInterval(interval) + ' sampling'),
                        filesize && m('span', formatSize(filesize)),
                    ]),
                ]),
            ]),
            m('main', [
                m(Sidebar, {
                    activeSection,
                    sections,
                }),
                m(SectionContent, {
                    section: activeSection,
                    groups,
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

    onremove(vnode) {
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
        const hasHistogramCharts = sectionHasHistogramCharts(attrs.groups);
        const heatmapState = sectionHeatmapState.get(sectionRoute) || { enabled: false, heatmapData: new Map(), loading: false };

        // Toggle handler for heatmap mode
        const toggleHeatmapMode = async () => {
            const newEnabled = !heatmapState.enabled;
            const newState = { ...heatmapState, enabled: newEnabled };
            sectionHeatmapState.set(sectionRoute, newState);

            if (newEnabled && heatmapState.heatmapData.size === 0) {
                // Fetch heatmap data for all histogram charts
                await fetchSectionHeatmapData(sectionRoute, attrs.groups);
            } else {
                m.redraw();
            }
        };

        // Heatmap toggle button (shown when section has histogram charts)
        const heatmapToggle = hasHistogramCharts ? m('button.heatmap-mode-toggle', {
            onclick: toggleHeatmapMode,
            disabled: heatmapState.loading,
        }, heatmapState.loading ? 'Loading...' : (heatmapState.enabled ? 'Show Percentiles' : 'Show Heatmaps')) : null;

        // Special handling for Query Explorer
        if (attrs.section.name === 'Query Explorer') {
            return m('div#section-content', [
                m('div.zoom-hint', 'Drag to zoom · Scroll to zoom · Double-click to reset'),
                m(QueryExplorer),
            ]);
        }

        // Special handling for cgroups with selector
        if (attrs.section.route === '/cgroups') {
            return m('div#section-content.cgroups-section', [
                m('div.section-header-row', [
                    m('div.zoom-hint', 'Drag to zoom · Scroll to zoom · Double-click to reset'),
                    heatmapToggle,
                ]),
                m('div.section-breadcrumb', [
                    'Samplers » ',
                    m('span.section-name', attrs.section.name),
                ]),
                m(CgroupSelector, { groups: attrs.groups }),
                m('div.cgroups-layout', [
                    m(
                        'div.cgroups-left',
                        attrs.groups
                            .filter(
                                (g) => g.metadata && g.metadata.side === 'left',
                            )
                            .map((group) => m(Group, { ...group, sectionRoute })),
                    ),
                    m(
                        'div.cgroups-right',
                        attrs.groups
                            .filter(
                                (g) =>
                                    g.metadata && g.metadata.side === 'right',
                            )
                            .map((group) => m(Group, { ...group, sectionRoute })),
                    ),
                ]),
            ]);
        }

        return m('div#section-content', [
            m('div.section-header-row', [
                m('div.zoom-hint', 'Drag to zoom · Scroll to zoom · Double-click to reset'),
                heatmapToggle,
            ]),
            m('div.section-breadcrumb', [
                'Samplers » ',
                m('span.section-name', attrs.section.name),
            ]),
            m(
                'div#groups',
                attrs.groups.map((group) => m(Group, { ...group, sectionRoute })),
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
        // Build regex pattern: escape special chars and wrap alternation in parentheses
        // Escape dots (.) since they're regex wildcards, but not slashes (/) as they're literals
        const selectedRegex =
            selectedArray.length > 1
                ? '(' +
                  selectedArray
                      .map((cg) => cg.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'))
                      .join('|') +
                  ')'
                : selectedArray.length === 1
                  ? selectedArray[0].replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
                  : '^$'; // Match nothing if no selection

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
                        // Replace the placeholder with actual selected cgroups
                        const updatedQuery = originalQuery.replace(
                            /__SELECTED_CGROUPS__/g,
                            selectedRegex,
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
            // Force a redraw of all charts
            chartsState.charts.forEach((chart) => {
                if (chart.isInitialized()) {
                    chart.reinitialize();
                }
            });
            m.redraw();
        }

        vnode.state.updateInProgress = false;
    },

    addCgroup(vnode, cgroup) {
        vnode.state.selectedCgroups.add(cgroup);
        // Assign a color to this cgroup
        chartsState.colorMapper.selectCgroup(cgroup);
        this.debouncedUpdateQueries(vnode);
    },

    removeCgroup(vnode, cgroup) {
        vnode.state.selectedCgroups.delete(cgroup);
        // Remove color assignment for this cgroup
        chartsState.colorMapper.deselectCgroup(cgroup);
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
                                    chartsState.colorMapper.selectCgroup(cg);
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
                                    chartsState.colorMapper.selectCgroup(cg);
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
                                    chartsState.colorMapper.deselectCgroup(cg);
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
                                    chartsState.colorMapper.deselectCgroup(cg);
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

// Page-level heatmap mode state
// Key: section route, Value: { enabled: boolean, heatmapData: Map<chartId, data>, loading: boolean }
const sectionHeatmapState = new Map();

// Helper function to check if a section has histogram charts
const sectionHasHistogramCharts = (groups) => {
    if (!groups) return false;
    return groups.some(group =>
        group.plots && group.plots.some(plot =>
            plot.promql_query && plot.promql_query.includes('histogram_percentiles')
        )
    );
};

// Fetch heatmap data for all histogram charts in a section
const fetchSectionHeatmapData = async (sectionRoute, groups) => {
    const state = sectionHeatmapState.get(sectionRoute) || { enabled: false, heatmapData: new Map(), loading: false };
    state.loading = true;
    sectionHeatmapState.set(sectionRoute, state);
    m.redraw();

    const heatmapData = new Map();

    for (const group of groups || []) {
        for (const plot of group.plots || []) {
            if (plot.promql_query && plot.promql_query.includes('histogram_percentiles')) {
                // Convert histogram_percentiles query to histogram_heatmap query
                const match = plot.promql_query.match(/histogram_percentiles\s*\(\s*\[[^\]]*\]\s*,\s*(.+)\)$/);
                if (!match) continue;

                const metricSelector = match[1].trim();
                const heatmapQuery = `histogram_heatmap(${metricSelector})`;

                try {
                    const result = await executePromQLRangeQuery(heatmapQuery);

                    if (result.status === 'success' && result.data && result.data.resultType === 'histogram_heatmap') {
                        const heatmapResult = result.data.result;
                        heatmapData.set(plot.opts.id, {
                            time_data: heatmapResult.timestamps,
                            bucket_bounds: heatmapResult.bucket_bounds,
                            data: heatmapResult.data.map(([timeIdx, bucketIdx, count]) => [timeIdx, bucketIdx, count]),
                            min_value: heatmapResult.min_value,
                            max_value: heatmapResult.max_value,
                        });
                    }
                } catch (error) {
                    console.error('Failed to fetch histogram heatmap:', error);
                }
            }
        }
    }

    state.heatmapData = heatmapData;
    state.loading = false;
    sectionHeatmapState.set(sectionRoute, state);
    m.redraw();
};

// Group component
const Group = {
    view({ attrs }) {
        const sectionRoute = attrs.sectionRoute;
        const heatmapState = sectionHeatmapState.get(sectionRoute);
        const isHeatmapMode = heatmapState?.enabled && !heatmapState?.loading;

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

                        if (isHistogramChart && isHeatmapMode && heatmapState?.heatmapData?.has(spec.opts.id)) {
                            // Create heatmap spec from the fetched data
                            const heatmapData = heatmapState.heatmapData.get(spec.opts.id);
                            const heatmapSpec = {
                                ...spec,
                                opts: {
                                    ...spec.opts,
                                    style: 'histogram_heatmap',
                                },
                                time_data: heatmapData.time_data,
                                bucket_bounds: heatmapData.bucket_bounds,
                                data: heatmapData.data,
                                min_value: heatmapData.min_value,
                                max_value: heatmapData.max_value,
                            };
                            return m(Chart, { spec: heatmapSpec, chartsState });
                        }

                        return m(Chart, { spec, chartsState });
                    }),
                ),
            ],
        );
    },
};

// Application state management
const chartsState = new ChartsState();

const sectionResponseCache = {};

// Execute a PromQL range query
const executePromQLRangeQuery = async (query) => {
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

    const url = `/api/v1/query_range?query=${encodeURIComponent(query)}&start=${start}&end=${maxTime}&step=${step}`;

    return m.request({
        method: 'GET',
        url,
        withCredentials: true,
    });
};

// Process dashboard data and execute PromQL queries where needed
const processDashboardData = async (data) => {
    for (const group of data.groups || []) {
        for (const plot of group.plots || []) {
            if (plot.promql_query) {
                try {
                    const result = await executePromQLRangeQuery(
                        plot.promql_query,
                    );

                    // Convert PromQL result to chart data format
                    if (
                        result.status === 'success' &&
                        result.data &&
                        result.data.result &&
                        result.data.result.length > 0
                    ) {
                        // Check if we have multiple series (from by() clause)
                        const hasMultipleSeries =
                            result.data.result.length > 1 ||
                            (plot.opts &&
                                (plot.opts.style === 'multi' ||
                                    plot.opts.style === 'heatmap'));

                        if (hasMultipleSeries) {
                            // Check if this is a heatmap chart
                            if (plot.opts && plot.opts.style === 'heatmap') {
                                // Transform to heatmap format: [time_index, cpu_id, value]
                                const heatmapData = [];
                                const timeSet = new Set();

                                // First pass: collect all unique timestamps
                                result.data.result.forEach((item) => {
                                    if (
                                        item.values &&
                                        Array.isArray(item.values)
                                    ) {
                                        item.values.forEach(
                                            ([timestamp, _]) => {
                                                timeSet.add(timestamp);
                                            },
                                        );
                                    }
                                });

                                // Convert to sorted array and create timestamp index map
                                const timestamps = Array.from(timeSet).sort(
                                    (a, b) => a - b,
                                );
                                const timestampToIndex = new Map();
                                timestamps.forEach((ts, idx) => {
                                    timestampToIndex.set(ts, idx);
                                });

                                // Second pass: create heatmap data with time indices
                                result.data.result.forEach((item, idx) => {
                                    if (
                                        item.values &&
                                        Array.isArray(item.values)
                                    ) {
                                        // Extract CPU ID from metric labels
                                        let cpuId = idx; // Default to index
                                        if (item.metric && item.metric.id) {
                                            cpuId = parseInt(item.metric.id);
                                        }

                                        // Add each data point as [time_index, cpu_id, value]
                                        item.values.forEach(
                                            ([timestamp, value]) => {
                                                const timeIndex =
                                                    timestampToIndex.get(
                                                        timestamp,
                                                    );
                                                heatmapData.push([
                                                    timeIndex,
                                                    cpuId,
                                                    parseFloat(value),
                                                ]);
                                            },
                                        );
                                    }
                                });

                                // Calculate min and max values for heatmap scaling
                                let minValue = Infinity;
                                let maxValue = -Infinity;
                                heatmapData.forEach(([_, __, value]) => {
                                    minValue = Math.min(minValue, value);
                                    maxValue = Math.max(maxValue, value);
                                });

                                plot.data = heatmapData;
                                plot.time_data = timestamps; // Store timestamps for x-axis
                                plot.min_value = minValue;
                                plot.max_value = maxValue;
                            } else {
                                // Multi-series line chart data
                                const allData = [];
                                const seriesNames = [];
                                let timestamps = null;

                                result.data.result.forEach((item, idx) => {
                                    if (
                                        item.values &&
                                        Array.isArray(item.values)
                                    ) {
                                        // Extract series name from metric labels
                                        let seriesName = 'Series ' + (idx + 1);
                                        if (item.metric) {
                                            // Find first non-__name__ label
                                            for (const [
                                                key,
                                                value,
                                            ] of Object.entries(item.metric)) {
                                                if (key !== '__name__') {
                                                    seriesName = value; // Just use the value for cleaner names
                                                    break;
                                                }
                                            }
                                        }

                                        // Only add series with data
                                        if (item.values.length > 0) {
                                            seriesNames.push(seriesName);

                                            // Extract timestamps (should be same for all series)
                                            if (!timestamps) {
                                                timestamps = item.values.map(
                                                    ([ts, _]) => ts,
                                                );
                                                allData.push(timestamps);
                                            }

                                            // Extract values for this series
                                            const values = item.values.map(
                                                ([_, val]) => parseFloat(val),
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
                                // Extract timestamps and values
                                const timestamps = sample.values.map(
                                    ([ts, _]) => ts,
                                );
                                const values = sample.values.map(([_, val]) =>
                                    parseFloat(val),
                                );
                                plot.data = [timestamps, values];
                            } else {
                                plot.data = [];
                            }
                        }
                    } else {
                        console.warn(
                            `Empty or unsuccessful result for query "${plot.promql_query}":`,
                            result,
                        );
                        plot.data = [];
                    }
                } catch (error) {
                    console.error(
                        `Failed to execute PromQL query "${plot.promql_query}":`,
                        error,
                    );
                    // Check if it's a 404 or similar error
                    if (error.message && error.message.includes('404')) {
                        console.warn(
                            `Metric not found for query: ${plot.promql_query}`,
                        );
                    }
                    // Keep empty data on error
                    plot.data = [];
                }
            }
        }
    }
    return data;
};

// Fetch data for a section and cache it.
const preloadSection = async (section) => {
    if (sectionResponseCache[section]) {
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

// Track active preloading to allow cancellation
let preloadingTimeout = null;
let activePreloadPromise = null;

// Preload data for all sections in the background.
const preloadSections = (allSections) => {
    // Cancel any existing preloading
    if (preloadingTimeout) {
        clearTimeout(preloadingTimeout);
        preloadingTimeout = null;
    }

    // Wait longer before starting preloading to allow user to navigate
    preloadingTimeout = setTimeout(() => {
        // Create a queue of sections to preload
        const sectionsToPreload = allSections
            .filter((section) => !sectionResponseCache[section.route])
            .map((section) => section.route.substring(1));

        const preloadNext = () => {
            if (sectionsToPreload.length === 0) {
                preloadingTimeout = null;
                return;
            }

            const nextSection = sectionsToPreload.shift();
            activePreloadPromise = preloadSection(nextSection)
                .then(() => {
                    activePreloadPromise = null;
                    // Schedule the next preload during idle time
                    if (window.requestIdleCallback) {
                        window.requestIdleCallback(preloadNext, {
                            timeout: 5000,
                        });
                    } else {
                        // Longer delay between preloads to minimize impact
                        preloadingTimeout = setTimeout(preloadNext, 500);
                    }
                })
                .catch(() => {
                    // If preloading fails, continue with next section
                    activePreloadPromise = null;
                    if (window.requestIdleCallback) {
                        window.requestIdleCallback(preloadNext, {
                            timeout: 5000,
                        });
                    } else {
                        preloadingTimeout = setTimeout(preloadNext, 500);
                    }
                });
        };

        // Start preloading the first section
        // We use requestIdleCallback if available to minimize performance impact.
        if (window.requestIdleCallback) {
            window.requestIdleCallback(preloadNext, { timeout: 5000 });
        } else {
            // Fallback to a fixed delay if requestIdleCallback is not supported (e.g. Safari)
            preloadingTimeout = setTimeout(preloadNext, 1000);
        }
    }, 5000); // Wait 5 seconds before starting any preloading
};

// Main application entry point
m.route.prefix = ''; // use regular paths for navigation, eg. /overview
m.route(document.body, '/overview', {
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

                // Cancel any ongoing preloading to prioritize user navigation
                if (preloadingTimeout) {
                    clearTimeout(preloadingTimeout);
                    preloadingTimeout = null;
                }
            }

            if (sectionResponseCache[params.section]) {
                const data = sectionResponseCache[params.section];
                const activeSection = data.sections.find(
                    (section) => section.route === requestedPath,
                );
                return {
                    view() {
                        return m(Main, {
                            ...data,
                            activeSection,
                        });
                    },
                };
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
                    const activeSection = processedData.sections.find(
                        (section) => section.route === requestedPath,
                    );

                    // Preload other sections after initial load
                    preloadSections(processedData.sections);

                    return {
                        view() {
                            return m(Main, {
                                ...processedData,
                                activeSection,
                            });
                        },
                    };
                });
        },
    },
});
