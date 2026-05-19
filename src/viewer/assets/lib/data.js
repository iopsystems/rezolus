import { ViewerApi } from './viewer_api.js';
import { resolveStyle } from './charts/metric_types.js';
import { collectGroupPlots } from './features/group_utils.js';

// Capture-id constants. Typos become grep-able; use these in place of
// raw 'baseline' / 'experiment' string literals.
export const CAPTURE_BASELINE = 'baseline';
export const CAPTURE_EXPERIMENT = 'experiment';

let _stepOverride = null;
const setStepOverride = (step) => { _stepOverride = step; };
const getStepOverride = () => _stepOverride;

const defaultGetMetadata = () => ViewerApi.getMetadata();
// Default queryRange threads the current `selectedNode` through to the
// SQL backend so dashboard queries respect the node picker without
// every callsite re-specifying it.
const defaultQueryRange = (query, start, end, step, captureId = 'baseline', opts) => {
    const merged = { ...(opts || {}) };
    if (merged.node === undefined) {
        const sel = getSelectedNode();
        if (sel) merged.node = sel;
    }
    return ViewerApi.queryRange(query, start, end, step, captureId, merged);
};

export const queryRangeForCapture = (captureId, query, start, end, step, opts) =>
    defaultQueryRange(query, start, end, step, captureId, opts);

let _selectedNode = null;
let _selectedInstances = {};  // { serviceName: instanceId | null }

const setSelectedNode = (node) => { _selectedNode = node; };
const getSelectedNode = () => _selectedNode;

const setSelectedInstance = (serviceName, instanceId) => {
    _selectedInstances[serviceName] = instanceId;
};
const getSelectedInstance = (serviceName) => _selectedInstances[serviceName] || null;

// SQL range-query result → plot-data shape helpers. Same transforms the
// baseline path (applyResultToPlot) and the compare path
// (extractExperimentCapture in viewer_core) apply. Extracted so the two
// callers can't drift.

const parseNumeric = (v) => {
    if (v === null || v === undefined) return null;
    const n = typeof v === 'number' ? v : Number(v);
    return Number.isNaN(n) ? null : n;
};

// Convert `result.data.result` (a SQL range-query series array projected
// to the Prometheus matrix shape) into a flat [timeIdx, y, value] triple
// table plus the sorted timestamps. `y` is parsed from `item.metric.id`
// when present, else the series index. Missing/NaN values are preserved
// as null so null-cell paths can paint them. Returns null-valued
// min/max when no numeric samples.
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

// Convert the first series in a SQL range-query result into a pair
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

// Build a Map<label, {timeData, valueData}> from a SQL range-query
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
        // Explicit style (set by dynamic specs) wins;
        // otherwise resolve from metric type.
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
                // Parallel to seriesNames; the raw metrics let compare-
                // mode's baseline path re-derive labels symmetrically
                // with the experiment path (composeScatterLabel needs
                // the full label set, not the lossy series_names string).
                const seriesMetrics = [];
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
                            seriesMetrics.push(item.metric || {});

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
                    plot.series_metrics = seriesMetrics;
                } else {
                    plot.data = [];
                    plot.series_names = [];
                    plot.series_metrics = [];
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
} = {}) => {
    let cachedMetadata = null;

    const fetchMetadata = async () => {
        const metadataResponse = await getMetadata();

        if (metadataResponse.status !== 'success') {
            throw new Error('Failed to get metadata');
        }

        return metadataResponse.data;
    };

    // The SQL backend resolves cgroup selection, histogram fan-out,
    // counter rates, and topology filters server-side from the
    // capture registry — the frontend just needs to hand it the
    // pre-emitted `sql_query`. Plots without a `sql_query` are KPIs
    // whose query lives in parquet metadata and hasn't been
    // translated yet; returning null makes processDashboardData skip
    // them and the section view paints a "query not yet available"
    // placeholder.
    const buildEffectiveQuery = (plot, _opts = {}) => plot.sql_query || null;

    const processDashboardData = async (data, _activeCgroupPattern, _sectionRoute) => {
        const metadata = cachedMetadata || await fetchMetadata();
        cachedMetadata = metadata;

        const queryPlots = [];
        for (const group of data.groups || []) {
            for (const plot of collectGroupPlots(group)) {
                // KPI plots whose query lives in parquet metadata
                // haven't been translated to SQL yet — mark them
                // unavailable so the section view paints a placeholder
                // instead of a silently-empty chart.
                if (!plot.sql_query) {
                    plot._unavailable = true;
                    plot._unavailableMessage =
                        '(query not yet available — translation pending)';
                    continue;
                }
                const queryToRun = buildEffectiveQuery(plot);
                if (queryToRun == null) continue;
                queryPlots.push({ plot, query: queryToRun });
            }
        }

        const minTime = metadata.minTime;
        const maxTime = metadata.maxTime;
        const duration = maxTime - minTime;
        const windowDuration = Math.min(3600, duration);
        const start = Math.max(minTime, maxTime - windowDuration);
        const step = _stepOverride || Math.max(1, Math.floor(windowDuration / 500));

        const results = await Promise.allSettled(
            queryPlots.map(({ query }) =>
                queryRange(query, start, maxTime, step),
            ),
        );

        for (let i = 0; i < queryPlots.length; i++) {
            const { plot } = queryPlots[i];
            const outcome = results[i];
            if (outcome.status === 'fulfilled') {
                applyResultToPlot(plot, outcome.value);
            } else {
                console.error(
                    `Failed to execute SQL query "${plot.sql_query}":`,
                    outcome.reason,
                );
                plot.data = [];
            }
        }

        // Surface no-data plots at the bottom (mirrors service KPI UX)
        // instead of leaving silent empty chart cards mid-section.
        // Plots flagged `_unavailable` (KPIs whose query lives in
        // parquet metadata and isn't SQL-transcribed yet) survive
        // — viewer_core's plotHasData treats the flag as "render the
        // placeholder card." Without that carve-out the placeholder
        // never reaches the renderer and KPI-heavy service sections
        // appear silently empty on pre-SQL-migration parquets.
        //
        // Filtered-out plots are still kept in `data._allPlots` so the
        // pinned single-chart view (`/chart/:section/:chartId`) can
        // resolve any chart the user has a deep link to, even when
        // the parquet carries no data for it. The array is
        // pre-allocated by `loadSection` so it survives the shallow
        // copy `storeSectionResponse` takes before this function runs.
        if (!data._allPlots) data._allPlots = [];
        const unavailable = [];
        const plotHasData = (plot) =>
            Array.isArray(plot.data) && plot.data.some((s) => Array.isArray(s) && s.length > 0);
        for (const group of data.groups || []) {
            for (const sg of group.subgroups || []) {
                const surviving = [];
                for (const plot of (sg.plots || [])) {
                    data._allPlots.push(plot);
                    if (!plot.sql_query
                        || plot._unavailable
                        || plotHasData(plot)) {
                        surviving.push(plot);
                    } else {
                        unavailable.push({
                            group: group.name,
                            subgroup: sg.name || null,
                            title: plot.opts?.title || '(unnamed chart)',
                            query: plot.sql_query,
                        });
                    }
                }
                sg.plots = surviving;
            }
            group.subgroups = (group.subgroups || []).filter((sg) => (sg.plots || []).length > 0);
        }
        data.groups = (data.groups || []).filter((g) => (g.subgroups || []).length > 0);
        if (unavailable.length > 0) {
            data.metadata = data.metadata || {};
            data.metadata.unavailable_charts = unavailable;
        }

        return data;
    };

    const clearMetadataCache = () => {
        cachedMetadata = null;
    };

    return {
        applyResultToPlot,
        processDashboardData,
        clearMetadataCache,
        buildEffectiveQuery,
    };
};

const defaultDataApi = createDataApi();

const {
    processDashboardData,
    clearMetadataCache,
    buildEffectiveQuery,
} = defaultDataApi;

/**
 * Fetch bucket-heatmap data for a single histogram plot. Returns
 * `{time_data, bucket_bounds, data, min_value, max_value}` —
 * exactly the shape `buildHistogramHeatmapSpec` consumes.
 *
 * Returns null for plots that don't carry a `metric` tag (e.g. the
 * combined-histogram `Overall` plots that fan out across multiple
 * `:buckets` columns and aren't single-metric addressable).
 */
const fetchHeatmapForPlot = async (plot, { captureId = 'baseline', node } = {}) => {
    const metric = plot?.opts?.metric;
    if (!metric) return null;
    const resp = await ViewerApi.heatmapRange({
        metric,
        kind: 'buckets',
        captureId,
        node: node || getSelectedNode() || undefined,
    });
    return resp;
};

/**
 * Batch fetch heatmaps for every histogram plot in `groups` that
 * carries a `metric` tag. Returns a Map keyed by `plot.opts.id` so
 * callers can populate `heatmapDataCache` directly.
 *
 * @param {Array} groups — section's `data.groups` array.
 * @param {object} [options]
 * @param {string} [options.captureId] — defaults to 'baseline'.
 * @param {string} [options.node] — optional R4 node filter.
 */
const fetchHeatmapsForGroups = async (groups, { captureId = 'baseline', node } = {}) => {
    const out = new Map();
    const targets = [];
    for (const g of groups || []) {
        for (const plot of collectGroupPlots(g)) {
            if (plot?.opts?.type !== 'histogram') continue;
            if (!plot.opts.metric) continue;
            targets.push(plot);
        }
    }
    if (targets.length === 0) return out;
    const settled = await Promise.allSettled(
        targets.map((plot) => fetchHeatmapForPlot(plot, { captureId, node })),
    );
    for (let i = 0; i < targets.length; i++) {
        const r = settled[i];
        if (r.status === 'fulfilled' && r.value) {
            out.set(targets[i].opts.id, r.value);
        }
    }
    return out;
};

/**
 * Fetch quantile-spectrum data for a single histogram plot's
 * `quantile_heatmap` rendering mode. Returns the unwrapped envelope
 * `{time_data, data, series_names, color_min_anchor}`.
 */
const fetchQuantileSpectrumForPlot = async (plot, { captureId = 'baseline', node, quantiles } = {}) => {
    const metric = plot?.opts?.metric;
    if (!metric) return null;
    // 100-quantile spectrum by default: p0..p100 in 0.01 steps, with
    // p0 peeled off server-side into `color_min_anchor`.
    const qs = quantiles || (() => {
        const out = [0.0];
        for (let i = 1; i <= 100; i++) out.push(i / 100);
        return out;
    })();
    const resp = await ViewerApi.heatmapRange({
        metric,
        kind: 'quantile_spectrum',
        quantiles: qs,
        captureId,
        node: node || getSelectedNode() || undefined,
    });
    return resp;
};

export {
    applyResultToPlot,
    processDashboardData,
    clearMetadataCache,
    createDataApi,
    setStepOverride,
    getStepOverride,
    setSelectedNode,
    getSelectedNode,
    setSelectedInstance,
    getSelectedInstance,
    buildEffectiveQuery,
    fetchHeatmapForPlot,
    fetchHeatmapsForGroups,
    fetchQuantileSpectrumForPlot,
};
