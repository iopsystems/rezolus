import { ViewerApi } from './viewer_api.js';
import { resolveStyle, buildHistogramQuery, isHistogramPlot } from './charts/metric_types.js';
import { collectGroupPlots } from './group_utils.js';

// Capture-id constants. Typos become grep-able; use these in place of
// raw 'baseline' / 'experiment' string literals.
export const CAPTURE_BASELINE = 'baseline';
export const CAPTURE_EXPERIMENT = 'experiment';

let _stepOverride = null;
const setStepOverride = (step) => { _stepOverride = step; };
const getStepOverride = () => _stepOverride;

// ---------------------------------------------------------------------------
// Concurrency-capped query pool. Shared by every fetch path (baseline
// per-plot, compare-mode experiment, heatmap) so we never have more than
// `_cap` PromQL evaluations running concurrently. Tasks queued past the
// cap run as earlier ones drain. The pool returns the original promise
// resolution / rejection — callers handle errors as if they had run the
// task directly.
// ---------------------------------------------------------------------------

class QueryPool {
    constructor(cap) {
        this._cap = Math.max(1, cap | 0);
        this._running = 0;
        this._queue = [];
    }
    setCap(cap) {
        this._cap = Math.max(1, cap | 0);
        this._drain();
    }
    enqueue(taskFn) {
        return new Promise((resolve, reject) => {
            const run = () => {
                this._running++;
                let result;
                try {
                    result = taskFn();
                } catch (e) {
                    this._running--;
                    this._drain();
                    reject(e);
                    return;
                }
                Promise.resolve(result).then(resolve, reject).finally(() => {
                    this._running--;
                    this._drain();
                });
            };
            if (this._running < this._cap) run();
            else this._queue.push(run);
        });
    }
    _drain() {
        while (this._running < this._cap && this._queue.length > 0) {
            const next = this._queue.shift();
            next();
        }
    }
    get pending() { return this._queue.length + this._running; }
}

const queryPool = new QueryPool(8);
const setQueryConcurrencyCap = (cap) => queryPool.setCap(cap);

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
const defaultQueryRange = (query, start, end, step, captureId = 'baseline') =>
    ViewerApi.queryRange(query, start, end, step, captureId);

export const queryRangeForCapture = (captureId, query, start, end, step) =>
    defaultQueryRange(query, start, end, step, captureId);

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

        // Check context before the identifier
        const before = query.substring(0, offset);

        // Skip identifiers inside by(...) / without(...) grouping clauses —
        // these are label names, not metric names.
        if (/\b(?:by|without)\s*\([^)]*$/.test(before)) return match;

        // Check if inside braces (label name/value) or square brackets (duration)
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

// ── PromQL result → plot-data shape helpers (pure) ───────────────────
// These are the same transforms the baseline path (applyResultToPlot)
// and the compare path (extractExperimentCapture in viewer_core) apply.
// Extracted so the two callers can't drift.

const parseNumeric = (v) => {
    if (v === null || v === undefined) return null;
    const n = typeof v === 'number' ? v : Number(v);
    return Number.isNaN(n) ? null : n;
};

// Convert `result.data.result` (a PromQL range-query series array) into
// a flat [timeIdx, y, value] triple table plus the sorted timestamps.
// `y` is parsed from `item.metric.id` when present, else the series
// index. Missing/NaN values are preserved as null so null-cell paths
// can paint them. Returns null-valued min/max when no numeric samples.
export const promqlResultToHeatmapTriples = (results) => {
    const timeSet = new Set();
    for (const item of results) {
        for (const [ts] of item.values || []) timeSet.add(ts);
    }
    const timestamps = Array.from(timeSet).sort((a, b) => a - b);
    const timestampToIndex = new Map();
    timestamps.forEach((ts, idx) => timestampToIndex.set(ts, idx));

    const triples = [];
    let minValue = Infinity;
    let maxValue = -Infinity;
    results.forEach((item, idx) => {
        let y = idx;
        if (item.metric && item.metric.id != null) {
            const parsed = parseInt(item.metric.id, 10);
            if (!Number.isNaN(parsed)) y = parsed;
        }
        for (const [ts, rawVal] of item.values || []) {
            const ti = timestampToIndex.get(ts);
            if (ti === undefined) continue;
            const v = parseNumeric(rawVal);
            if (v != null) {
                if (v < minValue) minValue = v;
                if (v > maxValue) maxValue = v;
            }
            triples.push([ti, y, v]);
        }
    });
    return {
        timestamps,
        triples,
        minValue: Number.isFinite(minValue) ? minValue : null,
        maxValue: Number.isFinite(maxValue) ? maxValue : null,
    };
};

// Convert the first series in a PromQL range-query result into a pair
// of parallel timeData / valueData arrays. Missing/NaN values are
// preserved as null.
export const promqlResultToLinePair = (results) => {
    const first = results[0];
    const values = Array.isArray(first?.values) ? first.values : [];
    return {
        timeData: values.map((pair) => Number(pair[0])),
        valueData: values.map((pair) => parseNumeric(pair[1])),
    };
};

// Build a Map<label, {timeData, valueData}> from a PromQL range-query
// result. `labelFor(item, idx)` picks the series label; returning null
// skips the series.
export const promqlResultToSeriesMap = (results, labelFor) => {
    const map = new Map();
    results.forEach((item, idx) => {
        const label = labelFor(item, idx);
        if (label == null) return;
        const values = Array.isArray(item.values) ? item.values : [];
        map.set(String(label), {
            timeData: values.map((pair) => Number(pair[0])),
            valueData: values.map((pair) => parseNumeric(pair[1])),
        });
    });
    return map;
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
                const { timestamps, triples, minValue, maxValue } =
                    promqlResultToHeatmapTriples(result.data.result);
                plot.data = triples;
                plot.time_data = timestamps;
                plot.min_value = minValue != null ? minValue : Infinity;
                plot.max_value = maxValue != null ? maxValue : -Infinity;
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

    // Apply the same per-plot query transforms the baseline path applies.
    // Returns the query to actually execute, or `null` when the plot should
    // be skipped (e.g. cgroup pattern without a resolved selector).
    //
    // `opts`:
    //   sectionRoute       — route string, used for the service/node rule.
    //   activeCgroupPattern — resolved cgroup selector, if any.
    //   serviceName        — section's service_name, if any.
    //   crossCapture       — default false. When true, skip BOTH node and
    //                        instance label injection. Compare path sets
    //                        this because the experiment capture's
    //                        topology is independent of the baseline's,
    //                        and the injected labels would mis-target.
    //   stepOverride       — nullable; when > 1 triggers histogram-stride /
    //                        counter-rate rewriting. Defaults to the
    //                        module-level _stepOverride.
    const buildEffectiveQuery = (plot, opts = {}) => {
        if (!plot.promql_query) return null;
        const {
            sectionRoute = null,
            activeCgroupPattern = null,
            serviceName = null,
            crossCapture = false,
            stepOverride = _stepOverride,
        } = opts;
        const injectTopologyLabels = !crossCapture;

        let q = plot.promql_query;
        const stepActive = stepOverride && stepOverride > 1;

        if (plot.opts.type === 'histogram') {
            q = buildHistogramQuery(
                q, plot.opts.subtype, plot.opts.percentiles,
                stepActive ? stepOverride : undefined,
            );
        }
        if (stepActive && plot.opts.type === 'delta_counter') {
            q = rewriteCounterQuery(q, stepOverride);
        }
        if (q.includes('__SELECTED_CGROUPS__')) {
            if (activeCgroupPattern) {
                q = substituteCgroupPattern(q, activeCgroupPattern);
            } else if (q.includes('!~')) {
                q = substituteCgroupPattern(q, null);
            } else {
                return null;
            }
        }
        if (injectTopologyLabels && _selectedNode && sectionRoute && !sectionRoute.startsWith('/service/')) {
            q = injectLabel(q, 'node', _selectedNode);
        }
        if (injectTopologyLabels && serviceName) {
            const inst = _selectedInstances[serviceName];
            if (inst) q = injectLabel(q, 'instance', inst);
        }
        return q;
    };

    // Walk every plot in the section's groups, run the per-plot query
    // transforms (histogram wrap, counter rewrite, cgroup substitution,
    // topology label injection), and stash the result on
    // `plot._effectiveQuery`. Plots that should be skipped (e.g. cgroup
    // pattern with no resolved selector) get `_effectiveQuery: null` so
    // the lazy fetch path can short-circuit.
    //
    // The fetch ITSELF runs lazily — see `fetchPlotData` and the
    // `LazyChart` wrapper that calls it on viewport intersection.
    // Returns the data unchanged so callers can still cache it.
    const processDashboardData = async (data, activeCgroupPattern, sectionRoute) => {
        const metadata = cachedMetadata || await fetchMetadata();
        cachedMetadata = metadata;
        for (const group of data.groups || []) {
            for (const plot of collectGroupPlots(group)) {
                if (!plot.promql_query) continue;
                plot._effectiveQuery = buildEffectiveQuery(plot, {
                    sectionRoute,
                    activeCgroupPattern,
                    serviceName: data.metadata?.service_name,
                });
                // Drop any prior fetch state so a re-process (granularity
                // change, cgroup pattern resolution, etc.) re-fetches.
                plot._fetched = false;
                plot._fetchInFlight = false;
                plot._fetchGen = (plot._fetchGen | 0) + 1;
            }
        }
        return data;
    };

    // Fetch a single plot's PromQL result through the shared concurrency
    // pool and apply it to the plot. Idempotent per generation: a plot
    // already fetched (or in flight) is skipped. Returns the in-flight
    // promise so callers (e.g. `flushAll` for save) can await completion.
    const fetchPlotData = (plot) => {
        if (!plot || !plot.promql_query) return Promise.resolve();
        if (plot._effectiveQuery == null) {
            // Pattern unresolved (e.g. cgroup) — render empty.
            plot.data = [];
            plot._fetched = true;
            return Promise.resolve();
        }
        if (plot._fetched && !plot._fetchInFlight) return Promise.resolve();
        if (plot._fetchInFlight) return plot._fetchPromise || Promise.resolve();
        const gen = plot._fetchGen | 0;
        plot._fetchInFlight = true;
        const p = queryPool
            .enqueue(() => executePromQLRangeQuery(plot._effectiveQuery))
            .then(
                (result) => {
                    if ((plot._fetchGen | 0) !== gen) return;
                    applyResultToPlot(plot, result);
                },
                (err) => {
                    if ((plot._fetchGen | 0) !== gen) return;
                    console.error(
                        `Failed to execute PromQL query "${plot.promql_query}":`,
                        err,
                    );
                    plot.data = [];
                },
            )
            .finally(() => {
                if ((plot._fetchGen | 0) === gen) {
                    plot._fetched = true;
                    plot._fetchInFlight = false;
                    plot._fetchPromise = null;
                }
            });
        plot._fetchPromise = p;
        return p;
    };

    // Force every plot in the given groups to fetch (queueing through the
    // pool) and return a promise that resolves when all are done. Used by
    // the save-with-selection path which needs every chart's plot.data
    // populated before serializing.
    const flushAll = async (groups) => {
        const plots = [];
        for (const group of groups || []) {
            for (const plot of collectGroupPlots(group)) {
                if (plot.promql_query) plots.push(plot);
            }
        }
        await Promise.allSettled(plots.map(fetchPlotData));
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
        const result = await queryPool.enqueue(
            () => executePromQLRangeQuery(`histogram_heatmap(${metricSelector}${strideSuffix})`),
        );

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
            for (const plot of collectGroupPlots(group)) {
                if (plot.promql_query && isHistogramPlot(plot)) {
                    plots.push(plot);
                }
            }
        }

        // Each fetchHeatmapForPlot already routes through the pool, so
        // Promise.allSettled here just collects the cap-throttled results.
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
        fetchPlotData,
        flushAll,
        substituteCgroupPattern,
        processDashboardData,
        clearMetadataCache,
        buildEffectiveQuery,
    };
};

const defaultDataApi = createDataApi();

const {
    executePromQLRangeQuery,
    fetchHeatmapForPlot,
    fetchHeatmapsForGroups,
    fetchPlotData,
    flushAll,
    processDashboardData,
    clearMetadataCache,
    buildEffectiveQuery,
} = defaultDataApi;

export {
    executePromQLRangeQuery,
    applyResultToPlot,
    fetchHeatmapForPlot,
    fetchHeatmapsForGroups,
    fetchPlotData,
    flushAll,
    substituteCgroupPattern,
    processDashboardData,
    clearMetadataCache,
    createDataApi,
    setStepOverride,
    getStepOverride,
    setQueryConcurrencyCap,
    setSelectedNode,
    getSelectedNode,
    setSelectedInstance,
    getSelectedInstance,
    injectLabel,
    buildEffectiveQuery,
};
