// utils.js - Common utility functions for chart rendering with fixed time axis handling

/**
 * Set up synchronization between charts
 * @param {Array} charts - Array of ECharts instances to synchronize
 * @param {Object} state - Global state object
 */
export function setupChartSync(charts, state) {
    charts.forEach(mainChart => {
        // Setup brush events for zooming
        mainChart.on('brushSelected', function (params) {
            // Prevent infinite recursion
            if (state.isZoomSyncing) return;

            try {
                // Set synchronization flag
                state.isZoomSyncing = true;

                // Only handle rectangle brush type (for zooming)
                if (params.brushType === 'rect') {
                    // Get the range from the brush
                    const areas = params.areas[0];
                    if (areas && areas.coordRange) {
                        const [start, end] = areas.coordRange;

                        // Get x-axis data range
                        const xAxis = mainChart.getModel().getComponent('xAxis', 0);
                        const axisExtent = xAxis.axis.scale.getExtent();
                        const axisRange = axisExtent[1] - axisExtent[0];

                        // Calculate percentage
                        const startPercent = ((start - axisExtent[0]) / axisRange) * 100;
                        const endPercent = ((end - axisExtent[0]) / axisRange) * 100;

                        // Update the global zoom state
                        state.globalZoom.start = startPercent;
                        state.globalZoom.end = endPercent;
                        state.globalZoom.isZoomed = true;

                        // NEW: Update the tracked zoom state
                        state.sharedAxisConfig.lastZoomState = `${startPercent}-${endPercent}`;

                        // Force recalculation of ticks with new zoom level
                        state.sharedAxisConfig.visibleTicks = [];
                        state.sharedAxisConfig.lastUpdate = 0;

                        // Apply zoom only to visible charts, mark others for lazy update
                        state.initializedCharts.forEach((chart, chartId) => {
                            const chartDom = chart.getDom();

                            if (isChartVisible(chartDom)) {
                                // Update visible charts immediately
                                chart.dispatchAction({
                                    type: 'dataZoom',
                                    start: startPercent,
                                    end: endPercent
                                });

                                // Clear the brush
                                chart.dispatchAction({
                                    type: 'brush',
                                    command: 'clear',
                                    areas: []
                                });

                                // Remove from charts needing update
                                state.chartsNeedingZoomUpdate.delete(chartId);
                            } else {
                                // Mark invisible charts for lazy update
                                state.chartsNeedingZoomUpdate.add(chartId);
                            }
                        });
                    }
                }
            } finally {
                // Reset flag after a short delay
                setTimeout(() => {
                    state.isZoomSyncing = false;
                }, 0);
            }
        });

        // Setup double-click handler for zoom reset
        mainChart.getZr().on('dblclick', function () {
            // Prevent infinite recursion
            if (state.isZoomSyncing) return;

            try {
                // Set synchronization flag
                state.isZoomSyncing = true;

                // Reset the global zoom state
                state.globalZoom.start = 0;
                state.globalZoom.end = 100;
                state.globalZoom.isZoomed = false;

                // NEW: Update the tracked zoom state
                state.sharedAxisConfig.lastZoomState = "0-100";

                // Clear the charts needing update set
                state.chartsNeedingZoomUpdate.clear();

                // Reset shared tick configuration to force recalculation
                state.sharedAxisConfig.visibleTicks = [];
                state.sharedAxisConfig.lastUpdate = 0;

                // Use the dedicated function to update all charts with reset zoom
                updateChartsAfterZoom(0, 100, state);
            } finally {
                // Reset flag after a short delay
                setTimeout(() => {
                    state.isZoomSyncing = false;
                }, 0);
            }
        });

        // Sync cursor across charts
        mainChart.on('updateAxisPointer', function (event) {
            // Prevent infinite recursion
            if (state.isCursorSyncing) return;

            try {
                // Set synchronization flag
                state.isCursorSyncing = true;

                // Update all other charts when this chart's cursor moves
                state.initializedCharts.forEach(chart => {
                    if (chart !== mainChart) {
                        chart.dispatchAction({
                            type: 'updateAxisPointer',
                            currTrigger: 'mousemove',
                            x: event.topX,
                            y: event.topY
                        });
                    }
                });
            } finally {
                // Reset flag after a short delay
                setTimeout(() => {
                    state.isCursorSyncing = false;
                }, 0);
            }
        });

        // Sync zooming across charts and update global state
        mainChart.on('dataZoom', function (event) {
            // Prevent infinite recursion by using a flag
            if (state.isZoomSyncing) return;

            // Only sync zooming actions initiated by user interaction
            if (event.batch) {
                try {
                    // Set synchronization flag to prevent recursion
                    state.isZoomSyncing = true;

                    // Get zoom range from the event
                    const {
                        start,
                        end
                    } = event.batch[0];

                    // Update global zoom state
                    state.globalZoom.start = start;
                    state.globalZoom.end = end;
                    state.globalZoom.isZoomed = start !== 0 || end !== 100;

                    // NEW: Update the tracked zoom state
                    state.sharedAxisConfig.lastZoomState = `${start}-${end}`;

                    // Update all charts with new zoom level and tick settings
                    updateChartsAfterZoom(start, end, state);
                } finally {
                    // Always reset the synchronization flag to allow future events
                    setTimeout(() => {
                        state.isZoomSyncing = false;
                    }, 0);
                }
            }
        });
    });
}

/**
 * Format a date for display with different formats based on context
 * @param {number} timestamp - Unix timestamp in seconds
 * @param {string} format - Format type: 'time', 'short', 'full', or 'axis'
 * @returns {string} Formatted date/time string
 */
export function formatDateTime(timestamp, format = 'time') {
    const date = new Date(timestamp * 1000);

    if (format === 'time') {
        // Simple time format HH:MM:SS
        return `${String(date.getHours()).padStart(2, '0')}:${String(date.getMinutes()).padStart(2, '0')}:${String(date.getSeconds()).padStart(2, '0')}`;
    } else if (format === 'short') {
        // Return HH:MM format for compact display
        return `${String(date.getHours()).padStart(2, '0')}:${String(date.getMinutes()).padStart(2, '0')}`;
    } else if (format === 'axis') {
        // For axis labels, forward to formatTimeAxisLabel
        return formatTimeAxisLabel('', -1, [timestamp]);
    } else {
        // Full format with date
        return `${date.toISOString().slice(0, 10)} ${String(date.getHours()).padStart(2, '0')}:${String(date.getMinutes()).padStart(2, '0')}:${String(date.getSeconds()).padStart(2, '0')}`;
    }
}

/**
 * Enhanced formatter function for time axis labels that doesn't rely on index
 * @param {string} value - Formatted time value (unused)
 * @param {number} index - Index in the data array (unused in fixed version)
 * @param {Array} timeData - Original timestamp array
 * @returns {string} Formatted time label
 */
export function formatTimeAxisLabel(value, index, timeData) {
    // In the fixed version, we ignore index and use the actual timestamp directly
    if (!timeData || timeData.length === 0) return value;

    // For the new approach, we expect timeData to contain the specific timestamp
    // for this label, not the entire array of timestamps
    const timestamp = index >= 0 && index < timeData.length ? timeData[index] : timeData[0];
    const date = new Date(timestamp * 1000);

    const seconds = date.getSeconds();
    const minutes = date.getMinutes();
    const hours = date.getHours();

    // On the hour boundary
    if (seconds === 0 && minutes === 0) {
        return `${String(hours).padStart(2, '0')}:00`;
    }
    // On the minute boundary
    else if (seconds === 0) {
        return `${String(hours).padStart(2, '0')}:${String(minutes).padStart(2, '0')}`;
    }
    // On a nice 30-second boundary
    else if (seconds % 30 === 0) {
        return `${String(hours).padStart(2, '0')}:${String(minutes).padStart(2, '0')}:${String(seconds).padStart(2, '0')}`;
    }
    // On a nice 15-second boundary
    else if (seconds % 15 === 0) {
        return `${String(hours).padStart(2, '0')}:${String(minutes).padStart(2, '0')}:${String(seconds).padStart(2, '0')}`;
    }
    // On a nice 5-second boundary
    else if (seconds % 5 === 0) {
        return `${String(hours).padStart(2, '0')}:${String(minutes).padStart(2, '0')}:${String(seconds).padStart(2, '0')}`;
    }
    // Default case - the original timestamp, properly formatted
    return `${String(hours).padStart(2, '0')}:${String(minutes).padStart(2, '0')}:${String(seconds).padStart(2, '0')}`;
}

/**
 * Helper function to check if a chart element is visible in the viewport
 * @param {HTMLElement} chartDom - Chart DOM element
 * @returns {boolean} True if chart is visible in viewport
 */
export function isChartVisible(chartDom) {
    if (!chartDom) return false;

    const rect = chartDom.getBoundingClientRect();
    const windowHeight = window.innerHeight || document.documentElement.clientHeight;
    const windowWidth = window.innerWidth || document.documentElement.clientWidth;

    // Consider charts partially in view to be visible
    return (
        rect.top <= windowHeight &&
        rect.bottom >= 0 &&
        rect.left <= windowWidth &&
        rect.right >= 0
    );
}
