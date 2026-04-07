import { ViewerApi } from './viewer_api.js';

const defaultGetMetadata = () => ViewerApi.getMetadata();
const defaultQueryRange = (query, start, end, step) =>
    ViewerApi.queryRange(query, start, end, step);

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
        const hasMultipleSeries =
            result.data.result.length > 1 ||
            (plot.opts &&
                (plot.opts.style === 'multi' ||
                    plot.opts.style === 'scatter' ||
                    plot.opts.style === 'heatmap'));

        if (hasMultipleSeries) {
            if (plot.opts && plot.opts.style === 'heatmap') {
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
        }
    } else {
        plot.data = [];
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
        const step = Math.max(1, Math.floor(windowDuration / 500));

        return queryRange(query, start, maxTime, step);
    };

    const processDashboardData = async (data, activeCgroupPattern) => {
        const metadata = cachedMetadata || await fetchMetadata();
        cachedMetadata = metadata;

        const queryPlots = [];
        for (const group of data.groups || []) {
            for (const plot of group.plots || []) {
                if (plot.promql_query) {
                    let queryToRun = plot.promql_query;
                    if (plot.promql_query.includes('__SELECTED_CGROUPS__')) {
                        if (activeCgroupPattern) {
                            queryToRun = substituteCgroupPattern(
                                plot.promql_query,
                                activeCgroupPattern,
                            );
                        } else if (plot.promql_query.includes('!~')) {
                            queryToRun = substituteCgroupPattern(
                                plot.promql_query,
                                null,
                            );
                        } else {
                            continue;
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
                total_counts: hr.total_counts || null,
                min_bucket_upperbounds: hr.min_bucket_upperbounds || null,
                max_bucket_upperbounds: hr.max_bucket_upperbounds || null,
            };
        }
        return null;
    };

    const fetchHeatmapsForGroups = async (groups) => {
        const plots = [];
        for (const group of groups || []) {
            for (const plot of group.plots || []) {
                if (plot.promql_query && plot.promql_query.includes('histogram_percentiles')) {
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

    return {
        executePromQLRangeQuery,
        applyResultToPlot,
        fetchHeatmapForPlot,
        fetchHeatmapsForGroups,
        substituteCgroupPattern,
        processDashboardData,
    };
};

const defaultDataApi = createDataApi();

const {
    executePromQLRangeQuery,
    fetchHeatmapForPlot,
    fetchHeatmapsForGroups,
    processDashboardData,
} = defaultDataApi;

export {
    executePromQLRangeQuery,
    applyResultToPlot,
    fetchHeatmapForPlot,
    fetchHeatmapsForGroups,
    substituteCgroupPattern,
    processDashboardData,
    createDataApi,
};
