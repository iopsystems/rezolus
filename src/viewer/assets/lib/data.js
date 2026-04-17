import { ViewerApi } from './viewer_api.js';
import { resolveStyle, buildHistogramQuery, isHistogramPlot } from './charts/metric_types.js';

let _stepOverride = null;
const setStepOverride = (step) => { _stepOverride = step; };
const getStepOverride = () => _stepOverride;

// ---------------------------------------------------------------------------
// Query rewriting for non-default granularity (step override)
// ---------------------------------------------------------------------------
// When the user picks a coarser step (e.g. 15s instead of auto ~1s), raw
// queries must be adjusted so that values are properly smoothed over the
// wider window rather than just down-sampled.
//
//   Counter:   irate(m[5m]) → rate(m[Ns])   (true average rate over window)
//   Gauge:     no rewrite needed (engine samples at step points)
//   Histogram: stride parameter passed to histogram_percentiles / histogram_heatmap

// Replace all irate(...[window]) with rate(...[Ns]) in a query string.
const rewriteCounterQuery = (query, stepSecs) => {
    const window = stepSecs + 's';
    return query.replace(/\birate\s*\(([^)]*?)\[\d+[smhd]\]/g, `rate($1[${window}]`);
};

// Gauge queries don't need rewriting — the PromQL engine samples the
// instantaneous value at each step point, which is correct for gauges.

const defaultGetMetadata = () => ViewerApi.getMetadata();
const defaultQueryRange = (query, start, end, step) =>
    ViewerApi.queryRange(query, start, end, step);

// Module-level state for label injection
let _selectedNode = null;
let _selectedInstances = {};  // { serviceName: instanceId | null }

const setSelectedNode = (node) => { _selectedNode = node; };
const getSelectedNode = () => _selectedNode;
const setSelectedInstance = (serviceName, instanceId) => {
    _selectedInstances[serviceName] = instanceId;
};
const getSelectedInstance = (serviceName) => _selectedInstances[serviceName] || null;

// Inject a label selector into all metric selectors in a query.
const PROMQL_KEYWORDS = new Set([
    'by', 'without', 'on', 'ignoring', 'group_left', 'group_right',
    'bool', 'sum', 'avg', 'min', 'max', 'count', 'rate', 'irate', 'increase',
    'histogram_percentiles', 'histogram_heatmap', 'topk', 'bottomk', 'offset',
    'abs', 'absent', 'ceil', 'floor', 'round', 'sqrt', 'exp', 'ln', 'log2',
    'log10', 'clamp', 'clamp_max', 'clamp_min', 'delta', 'deriv', 'idelta',
    'predict_linear', 'resets', 'changes', 'label_replace', 'label_join',
    'sort', 'sort_desc', 'time', 'timestamp', 'vector', 'scalar', 'sgn',
    'stddev', 'stdvar', 'quantile', 'count_values', 'group',
]);

// Inject a label selector into all metric references in a PromQL query.
// Handles three forms:
//   metric{existing}  → metric{existing,label="value"}
//   metric[5m]        → metric{label="value"}[5m]
//   metric            → metric{label="value"}   (bare, in expressions)
const injectLabel = (query, labelName, labelValue) => {
    if (!labelName || !labelValue) return query;
    const selector = `${labelName}="${labelValue}"`;

    // Single-pass regex that matches either:
    //   (1) identifier{...}  — metric with existing label selector
    //   (2) identifier       — bare identifier (metric, keyword, or other)
    // We handle both in one pass to avoid offset issues.
    return query.replace(/\b([a-z_]\w*)(\{[^}]*\})?/gi, (match, name, braces, offset) => {
        // Skip keywords (functions, aggregation operators, modifiers)
        if (PROMQL_KEYWORDS.has(name)) return match;

        // Skip if starts with digit (not a valid metric name)
        if (/^\d/.test(name)) return match;

        // If has braces: insert selector before closing brace
        if (braces) {
            return `${name}{${braces.slice(1, -1)},${selector}}`;
        }

        // Bare identifier — check context to decide if it's a metric name

        // Skip short tokens without underscores — likely time units (m, s, h, d),
        // PromQL modifiers, or label fragments, not metric names
        if (name.length < 3 && !name.includes('_')) return match;

        // Look ahead: if followed by '(' it's a function call, skip
        const after = query.substring(offset + match.length);
        if (/^\s*\(/.test(after)) return match;

        // Check if inside braces (label name/value) or square brackets (duration)
        const before = query.substring(0, offset);
        const lastOpenBrace = before.lastIndexOf('{');
        const lastCloseBrace = before.lastIndexOf('}');
        if (lastOpenBrace > lastCloseBrace) return match;
        const lastOpenBracket = before.lastIndexOf('[');
        const lastCloseBracket = before.lastIndexOf(']');
        if (lastOpenBracket > lastCloseBracket) return match;

        // Check if inside a string literal
        const quotesBefore = (before.match(/"/g) || []).length;
        if (quotesBefore % 2 !== 0) return match;

        // It's a bare metric name — add label selector
        return `${name}{${selector}}`;
    });
};

const substituteCgroupPattern = (query, pattern) => {
    query = query.replace(/,?\s*name!~"[^"]*"/g, '');
    query = query.replace(/\{\s*\}/g, '');

    if (pattern) {
        query = query.replace(/__SELECTED_CGROUPS__/g, pattern);
    }
    return query;
};

const applyResultToPlot = (plot, result) => {
    if (
        result.status === 'success' &&
        result.data &&
        result.data.result &&
        result.data.result.length > 0
    ) {
        // Resolve chart style from metric type (if present) or fall back to
        // explicit style (used by query explorer dynamic specs).
        const style = plot.opts.style || resolveStyle(
            plot.opts.type,
            plot.opts.subtype,
            result,
        );
        plot._resolvedStyle = style;

        const hasMultipleSeries =
            result.data.result.length > 1 ||
            (style === 'multi' ||
                style === 'scatter' ||
                style === 'heatmap');

        if (hasMultipleSeries) {
            if (style === 'heatmap') {
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
                            heatmapData.push([timeIndex, cpuId, parseFloat(value)]);
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
                const allData = [];
                const seriesNames = [];
                let timestamps = null;

                result.data.result.forEach((item, idx) => {
                    if (item.values && Array.isArray(item.values)) {
                        let seriesName = 'Series ' + (idx + 1);
                        if (item.metric) {
                            for (const [key, value] of Object.entries(item.metric)) {
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

                            const values = item.values.map(([_, val]) => parseFloat(val));
                            allData.push(values);
                        }
                    }
                });

                if (allData.length > 1) {
                    plot.data = allData;
                    plot.series_names = seriesNames;
                } else {
                    plot.data = [];
                    plot.series_names = [];
                }
            }
        } else {
            const sample = result.data.result[0];
            if (sample.values && Array.isArray(sample.values)) {
                const timestamps = sample.values.map(([ts, _]) => ts);
                const values = sample.values.map(([_, val]) => parseFloat(val));
                plot.data = [timestamps, values];
            } else {
                plot.data = [];
            }
            // Line-style plots have no series legend; clear any stale entries
            // from a prior multi-series render so legends don't "ghost".
            plot.series_names = [];
        }
    } else {
        plot.data = [];
        plot.series_names = [];
    }
};

const createDataApi = ({
    getMetadata = defaultGetMetadata,
    queryRange = defaultQueryRange,
    logHeatmapErrors = true,
} = {}) => {
    let cachedMetadata = null;

    const fetchMetadata = async () => {
        const metadataResponse = await getMetadata();

        if (metadataResponse.status !== 'success') {
            throw new Error('Failed to get metadata');
        }

        return metadataResponse.data;
    };

    const executePromQLRangeQuery = async (query, metadata) => {
        const meta = metadata || cachedMetadata || await fetchMetadata();

        const minTime = meta.minTime;
        const maxTime = meta.maxTime;
        const duration = maxTime - minTime;

        const windowDuration = Math.min(3600, duration);
        const start = Math.max(minTime, maxTime - windowDuration);
        const step = _stepOverride || Math.max(1, Math.floor(windowDuration / 500));

        return queryRange(query, start, maxTime, step);
    };

    const processDashboardData = async (data, activeCgroupPattern, sectionRoute) => {
        const metadata = cachedMetadata || await fetchMetadata();
        cachedMetadata = metadata;

        const queryPlots = [];
        for (const group of data.groups || []) {
            for (const plot of group.plots || []) {
                if (plot.promql_query) {
                    let queryToRun = plot.promql_query;
                    const stepActive = _stepOverride && _stepOverride > 1;

                    // Wrap histogram queries with the appropriate function,
                    // passing stride when a coarser step is selected.
                    if (plot.opts.type === 'histogram') {
                        queryToRun = buildHistogramQuery(
                            queryToRun, plot.opts.subtype, plot.opts.percentiles,
                            stepActive ? _stepOverride : undefined,
                        );
                    }

                    // Rewrite counter/gauge queries for coarser granularity
                    if (stepActive) {
                        if (plot.opts.type === 'delta_counter') {
                            queryToRun = rewriteCounterQuery(queryToRun, _stepOverride);
                        }
                    }

                    if (queryToRun.includes('__SELECTED_CGROUPS__')) {
                        if (activeCgroupPattern) {
                            queryToRun = substituteCgroupPattern(
                                queryToRun,
                                activeCgroupPattern,
                            );
                        } else if (queryToRun.includes('!~')) {
                            queryToRun = substituteCgroupPattern(
                                queryToRun,
                                null,
                            );
                        } else {
                            continue;
                        }
                    }
                    // Inject node label filter for non-service sections only.
                    if (_selectedNode && sectionRoute && !sectionRoute.startsWith('/service/')) {
                        queryToRun = injectLabel(queryToRun, 'node', _selectedNode);
                    }

                    // Inject instance label filter when a specific instance is selected
                    if (data.metadata?.service_name) {
                        const inst = _selectedInstances[data.metadata.service_name];
                        if (inst) {
                            queryToRun = injectLabel(queryToRun, 'instance', inst);
                        }
                    }

                    queryPlots.push({ plot, query: queryToRun });
                }
            }
        }

        const results = await Promise.allSettled(
            queryPlots.map(({ query }) =>
                executePromQLRangeQuery(query, metadata),
            ),
        );

        for (let i = 0; i < queryPlots.length; i++) {
            const { plot } = queryPlots[i];
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

    const fetchHeatmapForPlot = async (plot) => {
        const query = plot.promql_query;
        if (!query) return null;

        // For typed histogram specs, promql_query is already the base metric selector
        let metricSelector;
        if (plot.opts.type === 'histogram') {
            metricSelector = query;
        } else if (query.includes('histogram_percentiles')) {
            // Legacy fallback: extract base metric from wrapped query
            const match = query.match(/histogram_percentiles\s*\(\s*\[[^\]]*\]\s*,\s*(.+)\)$/);
            if (!match) return null;
            metricSelector = match[1].trim();
        } else {
            return null;
        }

        const strideSuffix = (_stepOverride && _stepOverride > 1) ? `, ${_stepOverride}` : '';
        const result = await executePromQLRangeQuery(`histogram_heatmap(${metricSelector}${strideSuffix})`);

        if (result.status === 'success' && result.data && result.data.resultType === 'histogram_heatmap') {
            const hr = result.data.result;
            return {
                time_data: hr.timestamps,
                bucket_bounds: hr.bucket_bounds,
                data: hr.data,
                min_value: hr.min_value,
                max_value: hr.max_value,
            };
        }
        return null;
    };

    const fetchHeatmapsForGroups = async (groups) => {
        const plots = [];
        for (const group of groups || []) {
            for (const plot of group.plots || []) {
                if (plot.promql_query && isHistogramPlot(plot)) {
                    plots.push(plot);
                }
            }
        }

        const results = await Promise.allSettled(plots.map((p) => fetchHeatmapForPlot(p)));

        const heatmapData = new Map();
        for (let i = 0; i < plots.length; i++) {
            if (results[i].status === 'fulfilled' && results[i].value) {
                heatmapData.set(plots[i].opts.id, results[i].value);
            } else if (results[i].status === 'rejected' && logHeatmapErrors) {
                console.error('Failed to fetch histogram heatmap:', results[i].reason);
            }
        }
        return heatmapData;
    };

    const clearMetadataCache = () => {
        cachedMetadata = null;
    };

    return {
        executePromQLRangeQuery,
        applyResultToPlot,
        fetchHeatmapForPlot,
        fetchHeatmapsForGroups,
        substituteCgroupPattern,
        processDashboardData,
        clearMetadataCache,
    };
};

const defaultDataApi = createDataApi();

const {
    executePromQLRangeQuery,
    fetchHeatmapForPlot,
    fetchHeatmapsForGroups,
    processDashboardData,
    clearMetadataCache,
} = defaultDataApi;

export {
    executePromQLRangeQuery,
    applyResultToPlot,
    fetchHeatmapForPlot,
    fetchHeatmapsForGroups,
    substituteCgroupPattern,
    processDashboardData,
    clearMetadataCache,
    createDataApi,
    setStepOverride,
    getStepOverride,
    setSelectedNode,
    getSelectedNode,
    setSelectedInstance,
    getSelectedInstance,
    injectLabel,
};
