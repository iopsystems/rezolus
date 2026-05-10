import { ChartsState, Chart } from './charts/chart.js';
import { executePromQLRangeQuery, fetchHeatmapForPlot, getSelectedNode, injectLabel } from './data.js';
import { isHistogramPlot, buildHistogramHeatmapSpec } from './charts/metric_types.js';
import { collectGroupPlots } from './group_utils.js';

// ── Unit selector ───────────────────────────────────────────────────

const UNIT_OPTIONS = [
    { value: '', label: 'Auto (none)' },
    { value: 'count', label: 'Count' },
    { value: 'rate', label: 'Rate (/s)' },
    { value: 'time', label: 'Time (ns)' },
    { value: 'bytes', label: 'Bytes' },
    { value: 'datarate', label: 'Data Rate (B/s)' },
    { value: 'bitrate', label: 'Bit Rate (bps)' },
    { value: 'percentage', label: 'Percentage (0–1 → %)' },
    { value: 'frequency', label: 'Frequency (Hz)' },
];

/** Render a unit-type selector dropdown with label (inline). */
const unitSelector = (current, onchange) =>
    m('label.unit-select-label', [
        'Unit: ',
        m('select.unit-select', {
            value: current,
            onchange: (e) => onchange(e.target.value),
            title: 'Y-axis unit type',
        }, UNIT_OPTIONS.map(o =>
            m('option', { value: o.value }, o.label),
        )),
    ]);

/** Build a format object for the given unit override (or null if empty). */
const buildFormatOverride = (unit) => {
    if (!unit) return undefined;
    const fmt = { unit_system: unit, precision: 2 };
    if (unit === 'percentage') fmt.range = { min: 0, max: 1 };
    return fmt;
};

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
const renderQueryChart = (resultData, query, chartsState, format) => {
    if (!resultData || resultData.length === 0) return m('p', 'No data returned');

    const isMulti = resultData.length > 1;

    if (isMulti) {
        const multi = buildMultiSeriesData(resultData);
        if (!multi) return null;

        const key = `query-chart-multi-${query}`;
        return m('div.query-chart', { key }, [
            m(Chart, {
                spec: {
                    opts: { id: key, title: 'Query Result', style: 'multi', format },
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
                opts: { id: key, title: 'Query Result', style: 'line', format },
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
        vnode.state.unitOverride = '';
        vnode.state.queryHistory = JSON.parse(
            localStorage.getItem('promql_history') || '[]',
        );
        vnode.state.queryChartsState = new ChartsState();

        vnode.state.executeQuery = async () => {
            if (!vnode.state.query.trim()) return;

            vnode.state.loading = true;
            vnode.state.error = null;

            try {
                let q = vnode.state.query;
                const node = getSelectedNode();
                if (node) q = injectLabel(q, 'node', node);
                vnode.state.result = await executePromQLRangeQuery(q);
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
                    m('div.query-controls', [
                        m('button.execute-btn', {
                            onclick: () => st.executeQuery(),
                            disabled: st.loading,
                        }, st.loading ? 'Running...' : 'Execute Query (Ctrl+Enter)'),
                        unitSelector(st.unitOverride, (v) => {
                            st.unitOverride = v;
                            st.queryChartsState.clear();
                        }),
                    ]),
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
                            buildFormatOverride(st.unitOverride),
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
        vnode.state.unitOverride = '';
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
                for (const plot of collectGroupPlots(group)) {
                    if (plot.opts.id === chartId) {
                        vnode.state.plot = plot;
                        vnode.state.query = plot.promql_query || '';
                        vnode.state.title = plot.opts.title || '';
                        vnode.state.description = plot.opts.description || '';
                        vnode.state.unitOverride = (plot.opts.format && plot.opts.format.unit_system) || '';
                        break;
                    }
                }
                if (vnode.state.plot) break;
            }
        }

        const plot = vnode.state.plot;
        if (!plot) return m('div.single-chart-view', m('p', `Chart "${chartId}" not found`));

        const st = vnode.state;

        const formatOverride = buildFormatOverride(st.unitOverride);
        const spec = {
            ...plot,
            opts: { ...plot.opts, title: st.title, description: st.description, format: formatOverride || plot.opts.format },
        };

        const executeQuery = async () => {
            if (!st.query.trim()) return;
            st.loading = true;
            st.error = null;

            try {
                let q = st.query;
                const node = getSelectedNode();
                if (node) q = injectLabel(q, 'node', node);
                const response = await executePromQLRangeQuery(q);

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
                    m('div.query-controls', [
                        m('button.execute-btn', {
                            onclick: executeQuery,
                            disabled: st.loading,
                        }, st.loading ? 'Running...' : 'Execute (Ctrl+Enter)'),
                        unitSelector(st.unitOverride, (v) => {
                            st.unitOverride = v;
                            st.singleChartsState.clear();
                        }),
                    ]),
                ]),
                st.error && m('div.error-message', st.error),
            ]),
        ]);
    },
};

// ── NLQueryExplorer: Natural Language to Chart ──────────────────────

let _transformersModule = null;
let _pipeline = null;
let _modelLoaded = false;
let _modelLoading = false;
let _modelError = null;

/**
 * Lazy-load the Transformers.js pipeline. Returns the pipeline
 * instance, downloading the model on first call with progress updates.
 */
const loadModel = async (onProgress) => {
    if (_modelLoaded) return _pipeline;
    if (_modelLoading) throw new Error('Model load already in progress');

    _modelLoading = true;
    _modelError = null;

    try {
        if (!_transformersModule) {
            // Dynamic import of Transformers.js — only triggers on first
            // use of the NL Query tab. The ESM import in index.html ensures
            // the module is available on globalThis.
            _transformersModule = await import('https://cdn.jsdelivr.net/npm/@xenova/transformers@2.17.2');
        }

        _pipeline = await _transformersModule.pipeline(
            'text-generation',
            'onnx-community/Qwen2.5-0.5B-Instruct-ONNX',
            {
                progress: (report) => {
                    if (typeof onProgress === 'function') {
                        onProgress(report);
                    }
                },
                config: {
                    // Quantized model for faster browser inference
                    dtype: 'q4f16',
                },
            }
        );

        _modelLoaded = true;
        _modelLoading = false;
    } catch (err) {
        _modelError = err.message || String(err);
        _modelLoading = false;
        throw err;
    }
};

/**
 * Convert natural language to a PromQL query using the loaded model.
 */
const nlToPromQL = async (pipeline, naturalLanguage) => {
    const systemPrompt = `You are a Rezolus telemetry assistant. Rezolus collects system performance metrics including CPU, scheduler, block I/O, network, and syscall metrics. Convert the user's natural language request into a valid PromQL query that would display the requested data as a chart.

Available metric prefixes:
- cpu_usage, cpu_frequency, cpu_instructions, cpu_cycles (CPU metrics)
- scheduler_runqueue (scheduler metrics)
- blockio_* (disk I/O metrics)
- network_bytes, network_packets (network metrics)
- syscall (syscall counts)

Rules:
1. Return ONLY the PromQL query, nothing else
2. Use rate()/irate() for counters, direct queries for gauges
3. Use a 5-minute window [5m] for rate calculations
4. For time-series charts, use range queries
5. Keep queries simple and readable

Examples:
  "Show me CPU usage over time" → sum(irate(cpu_usage[5m])) / 1e9 / cpu_cores
  "Show network traffic" → sum(rate(network_bytes{direction="transmit"}[5m]))
  "Show scheduler runqueue" → scheduler_runqueue`;

    const response = await pipeline(
        [{ role: 'system', content: systemPrompt }, { role: 'user', content: naturalLanguage }],
        { max_new_tokens: 128, temperature: 0.1, repetition_penalty: 1.1 },
    );

    let text = response?.[0]?.generated_text?.trim() || '';
    // Strip any markdown code fences
    text = text.replace(/^```(?:promql|sql|query)?\n?/i, '').replace(/```$/, '').trim();
    return text;
};

// Attrs: chartsState: ChartsState, queryRangeFn: (query, start, end, step) => Promise
export const NLQueryExplorer = {
    oninit(vnode) {
        vnode.state.query = '';
        vnode.state.result = null;
        vnode.state.error = null;
        vnode.state.loading = false;
        vnode.state.modelLoading = false;
        vnode.state.modelProgress = -1; // -1 = indeterminate
        vnode.state.modelLoaded = false;
        vnode.state.modelDownloaded = 0; // bytes downloaded
        vnode.state.modelTotal = 0; // total bytes
        vnode.state.chartState = new ChartsState();
        vnode.state.rawResult = null;
    },

    view(vnode) {
        const st = vnode.state;

        // Build progress info for display
        const formatBytes = (bytes) => {
            if (!bytes) return '';
            if (bytes < 1024) return bytes + ' B';
            if (bytes < 1024 * 1024) return (bytes / (1024 * 1024)).toFixed(1) + ' MB';
            return (bytes / (1024 * 1024 * 1024)).toFixed(1) + ' GB';
        };

        const modelProgressText = st.modelTotal > 0
            ? `${formatBytes(st.modelDownloaded)} / ${formatBytes(st.modelTotal)}`
            : st.modelLoading ? 'Downloading model...' : '';

        const modelProgressBar = st.modelLoading || st.modelLoaded
            ? m('div', { style: 'margin-top: 1rem' }, [
                m('div.progress-bar', [
                    m('div.progress-fill', {
                        style: st.modelTotal > 0
                            ? `width: ${Math.round((st.modelDownloaded / st.modelTotal) * 100)}%`
                            : undefined,
                        class: st.modelTotal === 0 ? 'indeterminate' : undefined,
                    }),
                ]),
                st.modelTotal > 0 && m('p', {
                    style: 'font-size: 0.8rem; color: var(--fg-muted); margin-top: 0.5rem',
                }, modelProgressText),
            ])
            : null;

        const handleExecute = async () => {
            if (!st.query.trim()) return;

            st.loading = true;
            st.error = null;
            st.result = null;
            st.rawResult = null;

            try {
                // Step 1: Ensure model is loaded
                if (!st.modelLoaded) {
                    st.modelLoading = true;
                    st.modelProgress = -1;
                    m.redraw();

                    await loadModel((report) => {
                        st.modelLoading = true;
                        if (report.total) {
                            st.modelTotal = report.total;
                        }
                        if (report.loaded != null) {
                            st.modelDownloaded = report.loaded;
                        }
                        // Indeterminate during init, determinate during actual downloads
                        if (report.total === undefined && report.loaded === undefined) {
                            st.modelTotal = 0;
                            st.modelDownloaded = 0;
                        }
                        m.redraw();
                    });
                    st.modelLoaded = true;
                    st.modelLoading = false;
                }

                // Step 2: Convert NL to PromQL
                const promql = await nlToPromQL(_pipeline, st.query);
                st.rawResult = promql;
                m.redraw();

                // Step 3: Execute the PromQL query
                const result = await vnode.attrs.queryRangeFn(promql);

                if (result.status === 'success' && result.data?.result) {
                    st.result = result;
                } else {
                    st.error = result.error || `Query returned no data`;
                }
            } catch (err) {
                console.error('[NL Query] Error:', err);
                st.error = err.message || String(err);
            } finally {
                st.loading = false;
                m.redraw();
            }
        };

        return m('div.nl-query-explorer', [
            // Header
            m('div.nl-query-header', [
                m('h2', 'NL Query'),
                m('p.nl-query-desc', 'Describe the chart you want in natural language. The AI converts it to a PromQL query and renders the result.'),
            ]),

            // Input section
            m('div.nl-query-input-section', [
                m('div.nl-query-wrapper', [
                    m('textarea.nl-query-input', {
                        placeholder: 'e.g., "Show me CPU usage over the last 5 minutes"',
                        value: st.query,
                        oninput: (e) => { st.query = e.target.value; },
                        onkeydown: (e) => {
                            if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) handleExecute();
                        },
                        rows: 3,
                        disabled: st.loading || st.modelLoading,
                    }),
                    m('div.nl-query-controls', [
                        m('button.nl-query-execute-btn', {
                            onclick: handleExecute,
                            disabled: st.loading || st.modelLoading || !st.query.trim(),
                        }, st.loading ? 'Processing...' : 'Generate & Execute (Ctrl+Enter)'),
                    ]),
                ]),
            ]),

            // Model loading progress (shown when model not yet loaded)
            modelProgressBar,

            // Model error
            _modelError && m('div.nl-query-error', [
                m('strong', 'Model Error: '), _modelError,
                m('button', {
                    onclick: () => { _modelLoaded = false; _modelLoading = false; _modelError = null; m.redraw(); },
                    style: 'margin-left: 1rem',
                }, 'Retry'),
            ]),

            // Raw PromQL (shown when available)
            st.rawResult && !st.result && m('div.nl-query-promql', [
                m('strong', 'Generated PromQL: '),
                m('code', st.rawResult),
            ]),

            // Error
            st.error && m('div.nl-query-error', m('strong', 'Error: '), st.error),

            // Result chart
            st.result && m('div.nl-query-result', [
                m('h3', 'Result'),
                st.result.status === 'success'
                    ? m('div.nl-query-chart', [
                        renderQueryChart(
                            st.result.data && st.result.data.result,
                            st.query,
                            st.chartState,
                            null,
                        ),
                    ])
                    : m('div.nl-query-error', 'Query failed: ' + (st.result.error || 'Unknown error')),
            ]),
        ]);
    },
};

