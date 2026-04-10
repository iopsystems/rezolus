// explorers.js - QueryExplorer and SingleChartView components

import { ChartsState, Chart } from './charts/chart.js';
import { executePromQLRangeQuery, fetchHeatmapForPlot } from './data.js';
import { isHistogramPlot, buildHistogramHeatmapSpec } from './charts/metric_types.js';

// ── Helpers ─────────────────────────────────────────────────────────

/** Build a human-readable series label from a PromQL metric map. */
const buildSeriesLabel = (metric, fallbackIdx) => {
    if (!metric) return 'Series ' + (fallbackIdx + 1);

    const labels = [];
    if (metric.id !== undefined) labels.push(`id=${metric.id}`);

    const excluded = ['__name__', 'id', 'metric', 'metric_type', 'unit'];
    const other = Object.entries(metric)
        .filter(([k]) => !excluded.includes(k))
        .sort((a, b) => a[0].localeCompare(b[0]))
        .map(([k, v]) => `${k}=${v}`);
    labels.push(...other);

    return labels.length > 0 ? labels.join(', ') : 'Series ' + (fallbackIdx + 1);
};

/** Transform multi-series PromQL result into { allData, seriesNames } or null. */
const buildMultiSeriesData = (resultData) => {
    const seriesNames = [];
    const allData = [];
    let timestamps = null;

    for (let i = 0; i < resultData.length; i++) {
        const item = resultData[i];
        if (!item.values || !Array.isArray(item.values)) continue;

        seriesNames.push(buildSeriesLabel(item.metric, i));

        if (!timestamps) {
            timestamps = item.values.map(([ts]) => ts);
            allData.push(timestamps);
        }
        allData.push(item.values.map(([, val]) => parseFloat(val)));
    }

    return allData.length > 1 ? { allData, seriesNames } : null;
};

/** Transform single-series PromQL result into [timestamps, values] or null. */
const buildSingleSeriesData = (resultData) => {
    const timestamps = [];
    const values = [];

    for (const item of resultData) {
        if (item.values && Array.isArray(item.values)) {
            for (const [ts, val] of item.values) {
                timestamps.push(ts);
                values.push(parseFloat(val));
            }
        } else if (item.value && Array.isArray(item.value) && item.value.length === 2) {
            timestamps.push(item.value[0]);
            values.push(parseFloat(item.value[1]));
        }
    }

    return timestamps.length > 0 ? [timestamps, values] : null;
};

/** Render a Chart component for a query result. */
const renderQueryChart = (resultData, query, chartsState) => {
    if (!resultData || resultData.length === 0) return m('p', 'No data returned');

    const isMulti = resultData.length > 1;

    if (isMulti) {
        const multi = buildMultiSeriesData(resultData);
        if (!multi) return null;

        const key = `query-chart-multi-${query}`;
        return m('div.query-chart', { key }, [
            m(Chart, {
                spec: {
                    opts: { id: key, title: 'Query Result', style: 'multi' },
                    data: multi.allData,
                    series_names: multi.seriesNames,
                },
                chartsState,
            }),
        ]);
    }

    const data = buildSingleSeriesData(resultData);
    if (!data) return null;

    const key = `query-chart-line-${query}`;
    return m('div.query-chart', { key }, [
        m(Chart, {
            spec: {
                opts: { id: key, title: 'Query Result', style: 'line' },
                data,
            },
            chartsState,
        }),
    ]);
};

/** Render a clickable example query item. */
const exampleQuery = (state, query, description) =>
    m('li', [
        m('code', {
            onclick: () => { state.query = query; state.executeQuery(); },
        }, query),
        description && (' - ' + description),
    ]);

/** Render a labeled input field with an Apply button. */
const fieldRow = (label, value, oninput, onApply) =>
    m('div.single-chart-field', [
        m('label', label),
        m('div.field-input-row', [
            m('input.field-input', {
                type: 'text',
                value,
                oninput,
                onkeydown: (e) => { if (e.key === 'Enter') onApply(); },
            }),
            m('button.field-apply-btn', { onclick: onApply }, 'Apply'),
        ]),
    ]);

// ── QueryExplorer ───────────────────────────────────────────────────

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
        vnode.state.queryChartsState = new ChartsState();

        vnode.state.executeQuery = async () => {
            if (!vnode.state.query.trim()) return;

            vnode.state.loading = true;
            vnode.state.error = null;

            try {
                vnode.state.result = await executePromQLRangeQuery(vnode.state.query);
            } catch (error) {
                vnode.state.error = error.message || 'Query failed';
            }

            vnode.state.loading = false;

            // Add to history if successful
            if (
                !vnode.state.error &&
                vnode.state.result &&
                !vnode.state.queryHistory.includes(vnode.state.query)
            ) {
                vnode.state.queryHistory.unshift(vnode.state.query);
                vnode.state.queryHistory = vnode.state.queryHistory.slice(0, 20);
                localStorage.setItem(
                    'promql_history',
                    JSON.stringify(vnode.state.queryHistory),
                );
            }

            m.redraw();
        };
    },

    oncreate(vnode) {
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
        if (vnode.state.liveInterval) clearInterval(vnode.state.liveInterval);
        if (vnode.state.queryChartsState) vnode.state.queryChartsState.clear();
    },

    view(vnode) {
        const st = vnode.state;

        return m('div.query-explorer', [
            // Input section
            m('div.query-input-section', [
                m('h2', 'PromQL Query Explorer'),
                m('div.query-input-wrapper', [
                    m('textarea.query-input', {
                        placeholder: 'Enter a PromQL query (e.g., sum(rate(syscall[5m])) or rate(network_bytes{direction="transmit"}[5m]))',
                        value: st.query,
                        oninput: (e) => { st.query = e.target.value; },
                        onkeydown: (e) => {
                            if (e.key === 'Enter' && e.ctrlKey) st.executeQuery();
                        },
                    }),
                    m('button.execute-btn', {
                        onclick: () => st.executeQuery(),
                        disabled: st.loading,
                    }, st.loading ? 'Running...' : 'Execute Query (Ctrl+Enter)'),
                ]),

                // Query history
                st.queryHistory.length > 0 && m('div.query-history', [
                    m('h3', 'Recent Queries'),
                    m('select.history-select', {
                        onchange: (e) => { st.query = e.target.value; },
                    }, [
                        m('option', { value: '' }, '-- Select from history --'),
                        st.queryHistory.map((q) =>
                            m('option', { value: q }, q.length > 80 ? q.substring(0, 77) + '...' : q),
                        ),
                    ]),
                ]),
            ]),

            // Error
            st.error && m('div.error-message', [m('strong', 'Error: '), st.error]),

            // Result
            st.result && m('div.query-result', [
                m('h3', 'Result'),
                st.result.status === 'success'
                    ? m('div.result-data', [
                        renderQueryChart(
                            st.result.data && st.result.data.result,
                            st.query,
                            st.queryChartsState,
                        ),
                    ])
                    : m('div.error-message', 'Query failed: ' + (st.result.error || 'Unknown error')),
            ]),

            // Example queries
            m('div.example-queries', [
                m('h3', 'Example Queries'),
                m('ul', [
                    exampleQuery(st, 'sum(irate(syscall[5m]))'),
                    exampleQuery(st, 'sum(irate(cpu_usage[5m])) / 1e9 / cpu_cores', 'Average CPU utilization (0-1)'),
                    exampleQuery(st, 'sum(irate(network_bytes{direction="transmit"}[5m])) * 8', 'Network transmit (bits/sec)'),
                    exampleQuery(st, 'sum(irate(cpu_instructions[5m])) / sum(irate(cpu_cycles[5m]))', 'IPC (Instructions per Cycle)'),
                    exampleQuery(st, 'sum by (id) (irate(cpu_usage[5m])) / 1e9', 'Per-CPU usage (cores)'),
                    exampleQuery(st, 'sum by (state) (irate(cpu_usage[5m])) / 1e9', 'CPU by state (user/system)'),
                    exampleQuery(st, 'sum by (direction) (irate(network_bytes[5m]))', 'Network by direction'),
                    exampleQuery(st, 'histogram_quantile(0.95, scheduler_runqueue_latency)', 'P95 scheduler latency'),
                    exampleQuery(st, 'sum by (op) (irate(syscall[5m]))', 'Syscalls by operation'),
                ]),
            ]),
        ]);
    },
};

// ── SingleChartView ─────────────────────────────────────────────────

// Expanded view for a single chart — opened in a new tab from the "Expand" link.
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
        vnode.state.heatmapMode = false;
        vnode.state.heatmapData = null;
        vnode.state.heatmapLoading = false;
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

        const st = vnode.state;

        const spec = {
            ...plot,
            opts: { ...plot.opts, title: st.title, description: st.description },
        };

        const executeQuery = async () => {
            if (!st.query.trim()) return;
            st.loading = true;
            st.error = null;

            try {
                const response = await executePromQLRangeQuery(st.query);

                if (response.status === 'success' && response.data && response.data.result) {
                    applyResultToPlot(plot, response);
                    st.singleChartsState.clear();
                } else {
                    st.error = response.error || 'Query returned no data';
                }
            } catch (e) {
                st.error = e.message || 'Query failed';
            }

            st.loading = false;
            m.redraw();
        };

        const applyFields = () => {
            st.singleChartsState.charts.forEach(chart => {
                chart.spec = spec;
                chart.configureChartByType();
            });
        };

        const isHistogram = isHistogramPlot(plot);

        const toggleHeatmap = async () => {
            if (st.heatmapMode) {
                st.heatmapMode = false;
                st.singleChartsState.resetAll();
                st.singleChartsState.clear();
                m.redraw();
                return;
            }
            if (!st.heatmapData) {
                st.heatmapLoading = true;
                m.redraw();
                st.heatmapData = await fetchHeatmapForPlot(plot);
                st.heatmapLoading = false;
            }
            if (st.heatmapData) {
                st.heatmapMode = true;
                st.singleChartsState.resetAll();
                st.singleChartsState.clear();
            }
            m.redraw();
        };

        const hasSelection = st.singleChartsState.hasActiveSelection();

        let chartSpec = spec;
        if (st.heatmapMode && st.heatmapData) {
            chartSpec = buildHistogramHeatmapSpec(spec, st.heatmapData);
        }

        return m('div.single-chart-view', [
            m('div.section-header-row', [
                m('h1.section-title', 'Single Chart View'),
                m('div.section-actions', [
                    hasSelection && m('button.section-action-btn', {
                        onclick: () => { st.singleChartsState.resetAll(); m.redraw(); },
                    }, 'RESET SELECTION'),
                    isHistogram && m('button.section-action-btn', {
                        onclick: toggleHeatmap,
                        disabled: st.heatmapLoading,
                    }, st.heatmapLoading ? 'LOADING...' : (st.heatmapMode ? 'SHOW PERCENTILES' : 'SHOW HEATMAP')),
                ]),
            ]),
            m('div.single-chart-container', [
                m('div.chart-wrapper', [
                    m(Chart, { spec: chartSpec, chartsState: st.singleChartsState, interval: data.interval }),
                ]),
            ]),
            m('div.single-chart-fields', [
                fieldRow('Title', st.title, (e) => { st.title = e.target.value; }, applyFields),
                fieldRow('Description', st.description, (e) => { st.description = e.target.value; }, applyFields),
            ]),
            m('div.single-chart-query', [
                m('label', 'PromQL Query'),
                m('div.query-input-wrapper', [
                    m('textarea.query-input', {
                        value: st.query,
                        oninput: (e) => { st.query = e.target.value; },
                        onkeydown: (e) => {
                            if (e.key === 'Enter' && e.ctrlKey) executeQuery();
                        },
                        rows: 2,
                    }),
                    m('button.execute-btn', {
                        onclick: executeQuery,
                        disabled: st.loading,
                    }, st.loading ? 'Running...' : 'Execute (Ctrl+Enter)'),
                ]),
                st.error && m('div.error-message', st.error),
            ]),
        ]);
    },
};
