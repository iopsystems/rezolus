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
// activeCgroupPattern is passed in from the caller to avoid circular deps.
const processDashboardData = async (data, activeCgroupPattern) => {
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

/**
 * Fetch heatmap data for a single histogram-percentiles plot.
 * Returns the heatmap payload object or null if the plot isn't a histogram or the query fails.
 */
const fetchHeatmapForPlot = async (plot) => {
    const query = plot.promql_query;
    if (!query || !query.includes('histogram_percentiles')) return null;

    const match = query.match(/histogram_percentiles\s*\(\s*\[[^\]]*\]\s*,\s*(.+)\)$/);
    if (!match) return null;

    const metricSelector = match[1].trim();
    const result = await executePromQLRangeQuery(`histogram_heatmap(${metricSelector})`);

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

/**
 * Fetch heatmap data for all histogram charts in a set of groups.
 * Returns a Map<chartId, heatmapPayload>.
 */
const fetchHeatmapsForGroups = async (groups) => {
    const plots = [];
    for (const group of groups || []) {
        for (const plot of group.plots || []) {
            if (plot.promql_query && plot.promql_query.includes('histogram_percentiles')) {
                plots.push(plot);
            }
        }
    }

    const results = await Promise.allSettled(plots.map(p => fetchHeatmapForPlot(p)));

    const heatmapData = new Map();
    for (let i = 0; i < plots.length; i++) {
        if (results[i].status === 'fulfilled' && results[i].value) {
            heatmapData.set(plots[i].opts.id, results[i].value);
        } else if (results[i].status === 'rejected') {
            console.error('Failed to fetch histogram heatmap:', results[i].reason);
        }
    }
    return heatmapData;
};

export {
    executePromQLRangeQuery,
    applyResultToPlot,
    fetchHeatmapForPlot,
    fetchHeatmapsForGroups,
    substituteCgroupPattern,
    processDashboardData,
};
