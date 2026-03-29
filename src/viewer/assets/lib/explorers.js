// explorers.js - QueryExplorer and SingleChartView components

import { ChartsState, Chart } from './charts/chart.js';

// Query Explorer component for running custom PromQL queries
// Attrs: { liveMode: boolean, isRecording: () => boolean }
export const QueryExplorer = {
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
        if (vnode.attrs.liveMode) {
            vnode.state.liveInterval = setInterval(() => {
                const isRecording = vnode.attrs.isRecording;
                if (isRecording && isRecording() && vnode.state.query && vnode.state.result && !vnode.state.loading) {
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

// Single-chart expanded view — opened in a new tab from the "Expand" link.
// Shows one chart at full width with an editable PromQL query input below it.
// Attrs: { data, chartId, applyResultToPlot: (plot, result) => void }
export const SingleChartView = {
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
        const applyResultToPlot = vnode.attrs.applyResultToPlot;
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
