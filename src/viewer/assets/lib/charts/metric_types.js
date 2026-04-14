/** Default percentile quantiles used across the viewer. */
export const DEFAULT_PERCENTILES = [0.5, 0.9, 0.99, 0.999, 0.9999];

/**
 * Maps semantic metric types to compatible chart styles.
 *
 * The backend specifies *what* the metric is (gauge, delta_counter, histogram),
 * and this module determines *how* it should be rendered.
 */

/**
 * Returns the list of chart styles compatible with the given metric type.
 *
 * @param {string} type_ - The metric type: 'gauge', 'delta_counter', or 'histogram'
 * @returns {string[]} Compatible chart style names
 */
export function compatibleStyles(type_) {
    switch (type_) {
        case 'gauge':
        case 'delta_counter':
            return ['line', 'heatmap', 'multi'];
        case 'histogram':
            return ['scatter', 'histogram_heatmap'];
        default:
            return ['line'];
    }
}

/**
 * Resolves the concrete chart style from a metric type, optional subtype,
 * and query result shape.
 *
 * For gauge/delta_counter the style is inferred from the query result:
 *   - Single series          -> 'line'
 *   - Multi-series with `id` -> 'heatmap'
 *   - Multi-series otherwise -> 'multi'
 *
 * For histogram the subtype selects the style:
 *   - 'percentiles' (default) -> 'scatter'
 *   - 'buckets'               -> 'histogram_heatmap'
 *
 * @param {string} type_    - Metric type from the plot spec
 * @param {string} [subtype] - Optional subtype (e.g. 'percentiles', 'buckets')
 * @param {object} [result]  - PromQL query result (used for gauge/counter inference)
 * @returns {string} The resolved chart style name
 */
export function resolveStyle(type_, subtype, result) {
    if (type_ === 'histogram') {
        return subtype === 'buckets' ? 'histogram_heatmap' : 'scatter';
    }

    // gauge or delta_counter: infer from result shape
    if (result?.data?.result?.length > 1) {
        const first = result.data.result[0];
        if (first.metric && first.metric.id != null) {
            return 'heatmap';
        }
        return 'multi';
    }
    return 'line';
}

/**
 * Wraps a base histogram metric selector with the appropriate PromQL function
 * based on the subtype.
 *
 * @param {string} baseQuery - The raw metric selector (e.g. 'tcp_packet_latency')
 * @param {string} [subtype]  - 'percentiles' or 'buckets'
 * @param {number[]} [percentiles] - Custom quantiles (default: DEFAULT_PERCENTILES)
 * @param {number} [strideSecs] - Optional stride in seconds for coarser granularity
 * @returns {string} The wrapped PromQL query
 */
export function buildHistogramQuery(baseQuery, subtype, percentiles, strideSecs) {
    const strideSuffix = strideSecs ? `, ${strideSecs}` : '';
    if (subtype === 'buckets') {
        return `histogram_heatmap(${baseQuery}${strideSuffix})`;
    }
    const quantiles = percentiles || DEFAULT_PERCENTILES;
    return `histogram_percentiles([${quantiles.join(', ')}], ${baseQuery}${strideSuffix})`;
}

/**
 * Returns true if the given plot spec represents a histogram chart.
 * Checks the semantic type first, with a legacy fallback for specs that
 * still use the old histogram_percentiles query format.
 *
 * @param {object} plot - A plot spec with opts and optional promql_query
 * @returns {boolean}
 */
export function isHistogramPlot(plot) {
    return plot.opts?.type === 'histogram' ||
        (plot.promql_query && plot.promql_query.includes('histogram_percentiles'));
}

/**
 * Builds a histogram_heatmap chart spec by merging heatmap data into a base
 * scatter plot spec.
 *
 * @param {object} spec - The original scatter plot spec
 * @param {object} heatmapData - Data from fetchHeatmapForPlot()
 * @param {object} [optsOverrides] - Additional opts to merge (e.g. prefixed title)
 * @returns {object} A new spec configured for histogram_heatmap rendering
 */
export function buildHistogramHeatmapSpec(spec, heatmapData, optsOverrides) {
    return {
        ...spec,
        opts: {
            ...(optsOverrides || spec.opts),
            style: 'histogram_heatmap',
        },
        time_data: heatmapData.time_data,
        bucket_bounds: heatmapData.bucket_bounds,
        data: heatmapData.data,
        min_value: heatmapData.min_value,
        max_value: heatmapData.max_value,
    };
}
