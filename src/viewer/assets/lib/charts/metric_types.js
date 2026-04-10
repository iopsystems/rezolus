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
 * @returns {string} The wrapped PromQL query
 */
export function buildHistogramQuery(baseQuery, subtype) {
    if (subtype === 'buckets') {
        return `histogram_heatmap(${baseQuery})`;
    }
    // Default to percentiles
    return `histogram_percentiles([0.5, 0.9, 0.99, 0.999, 0.9999], ${baseQuery})`;
}
