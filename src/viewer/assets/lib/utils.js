// utils.js - Common utility functions for chart rendering with fixed time axis handling

/**
 * Calculate shared visible ticks for consistent tick spacing across all charts
 * @param {number} dataLength - Length of the data array
 * @param {number} zoomStart - Start of zoom range (percentage)
 * @param {number} zoomEnd - End of zoom range (percentage)
 * @returns {Array} Array of tick indices
 */
export function calculateSharedVisibleTicks(dataLength, zoomStart, zoomEnd) {
    // Full view zoom (special case to prevent label pile-up)
    if (zoomStart === 0 && zoomEnd === 100) {
        // For full view, create fewer evenly spaced ticks
        const maxTicks = Math.min(8, dataLength);
        const interval = Math.max(1, Math.floor(dataLength / maxTicks));

        const ticks = [];
        for (let i = 0; i < dataLength; i += interval) {
            ticks.push(i);
        }

        // Add last tick if not already included
        if (dataLength > 0 && (dataLength - 1) % interval !== 0) {
            ticks.push(dataLength - 1);
        }

        return ticks;
    }

    // Normal zoom case:
    // Convert start and end percentages to indices
    let startIdx = Math.floor(dataLength * (zoomStart / 100));
    let endIdx = Math.ceil(dataLength * (zoomEnd / 100));

    // Ensure bounds
    startIdx = Math.max(0, startIdx);
    endIdx = Math.min(dataLength - 1, endIdx);

    // Calculate number of visible data points
    const visiblePoints = endIdx - startIdx;

    // Determine desired number of ticks - 8-10 is usually good for readability
    const desiredTicks = Math.min(10, Math.max(4, visiblePoints));

    // Calculate tick interval
    const interval = Math.max(1, Math.floor(visiblePoints / desiredTicks));

    // Generate tick array
    const ticks = [];
    for (let i = startIdx; i <= endIdx; i += interval) {
        ticks.push(i);
    }

    // Ensure we have the end tick if not already included
    if (ticks.length > 0 && ticks[ticks.length - 1] !== endIdx) {
        ticks.push(endIdx);
    }

    return ticks;
}

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
 * Check if a timestamp is aligned with human-friendly time boundaries
 * @param {Date} date - JavaScript Date object
 * @param {number} intervalSeconds - Current interval in seconds
 * @returns {boolean} True if the timestamp is aligned
 */
function isTimeAligned(date, intervalSeconds) {
    const seconds = date.getSeconds();
    const minutes = date.getMinutes();
    const hours = date.getHours();

    // Hour boundary alignment
    if (intervalSeconds >= 3600) {
        const hourInterval = intervalSeconds / 3600;
        return hours % hourInterval === 0 && minutes === 0 && seconds === 0;
    }

    // Minute boundary alignment
    if (intervalSeconds >= 60) {
        const minuteInterval = intervalSeconds / 60;
        return minutes % minuteInterval === 0 && seconds === 0;
    }

    // Second boundary alignment
    return seconds % intervalSeconds === 0;
}

/**
 * Calculate tick positions at human-friendly time intervals
 * IMPROVED: Ensures better distribution of ticks after zooming in and out
 * @param {Array} timeData - Array of timestamps
 * @param {number} startPercent - Start of zoom range (percentage)
 * @param {number} endPercent - End of zoom range (percentage)
 * @returns {Array} Array of indices to show tick marks at
 */
export function calculateHumanFriendlyTicks(timeData, startPercent, endPercent) {
    if (!timeData || timeData.length === 0) return [];

    // Convert percentages to indices
    const startIdx = Math.floor(timeData.length * (startPercent / 100));
    const endIdx = Math.min(Math.ceil(timeData.length * (endPercent / 100)), timeData.length - 1);

    // Calculate time range in seconds
    const startTime = timeData[startIdx];
    const endTime = timeData[endIdx];
    const timeSpanSeconds = endTime - startTime;

    // IMPROVEMENT: Calculate maximum number of ticks based on visible range
    // The wider the range, the fewer ticks we want to show
    const visibleRange = endIdx - startIdx + 1;
    const maxTicks = Math.min(12, Math.max(4, Math.ceil(visibleRange / 20)));

    // ADAPTIVE INTERVAL: Choose appropriate interval based on timespan
    let intervalSeconds;

    if (timeSpanSeconds <= 1) {
        // For very small ranges (less than 1 second), just show start and end
        const ticks = [startIdx];
        if (startIdx !== endIdx) ticks.push(endIdx);
        return ticks;
    } else if (timeSpanSeconds <= 5) {
        intervalSeconds = 1; // 1 second intervals
    } else if (timeSpanSeconds <= 10) {
        intervalSeconds = 2; // 2 second intervals
    } else if (timeSpanSeconds <= 30) {
        intervalSeconds = 5; // 5 second intervals
    } else if (timeSpanSeconds <= 60) {
        intervalSeconds = 10; // 10 second intervals
    } else if (timeSpanSeconds <= 300) {
        intervalSeconds = 30; // 30 second intervals
    } else if (timeSpanSeconds <= 900) {
        intervalSeconds = 60; // 1 minute intervals
    } else if (timeSpanSeconds <= 3600) {
        intervalSeconds = 300; // 5 minute intervals
    } else if (timeSpanSeconds <= 14400) {
        intervalSeconds = 900; // 15 minute intervals
    } else if (timeSpanSeconds <= 86400) {
        intervalSeconds = 3600; // 1 hour intervals
    } else {
        intervalSeconds = 21600; // 6 hour intervals
    }

    // ADAPTIVE ADJUSTMENT: If we still have too many ticks, increase the interval
    let adjustedInterval = intervalSeconds;
    let estimatedTicks = Math.ceil(timeSpanSeconds / intervalSeconds);

    while (estimatedTicks > maxTicks) {
        adjustedInterval *= 2;
        estimatedTicks = Math.ceil(timeSpanSeconds / adjustedInterval);
    }

    intervalSeconds = adjustedInterval;

    // Find aligned time boundaries
    const startDate = new Date(startTime * 1000);
    const startYear = startDate.getFullYear();
    const startMonth = startDate.getMonth();
    const startDay = startDate.getDate();
    const startHour = startDate.getHours();
    const startMinute = startDate.getMinutes();
    const startSecond = startDate.getSeconds();

    // Create a properly aligned first tick based on interval
    let firstTickDate;

    if (intervalSeconds < 60) {
        // For seconds-based intervals
        const targetSecond = Math.ceil(startSecond / intervalSeconds) * intervalSeconds;
        firstTickDate = new Date(startYear, startMonth, startDay, startHour, startMinute,
            targetSecond >= 60 ? 0 : targetSecond);

        // Handle minute rollover
        if (targetSecond >= 60) {
            firstTickDate.setMinutes(startMinute + 1);
        }
    } else if (intervalSeconds < 3600) {
        // For minutes-based intervals
        const minuteInterval = intervalSeconds / 60;
        const targetMinute = Math.ceil(startMinute / minuteInterval) * minuteInterval;

        firstTickDate = new Date(startYear, startMonth, startDay, startHour,
            targetMinute >= 60 ? 0 : targetMinute, 0);

        // Handle hour rollover
        if (targetMinute >= 60) {
            firstTickDate.setHours(startHour + 1);
        }
    } else {
        // For hours-based intervals
        const hourInterval = intervalSeconds / 3600;
        const targetHour = Math.ceil(startHour / hourInterval) * hourInterval;

        firstTickDate = new Date(startYear, startMonth, startDay,
            targetHour >= 24 ? 0 : targetHour, 0, 0);

        // Handle day rollover
        if (targetHour >= 24) {
            firstTickDate.setDate(startDay + 1);
        }
    }

    // Generate ticks from the first aligned time boundary
    const ticks = [];
    const ticksMap = new Set(); // Use Set to avoid duplicate indices

    // Always include the first and last data points
    ticksMap.add(startIdx);
    ticksMap.add(endIdx);

    // Generate aligned time ticks
    let currentTickTime = firstTickDate.getTime() / 1000;

    // IMPROVED: Limit the number of ticks we'll generate to prevent bunching
    while (currentTickTime <= endTime && ticksMap.size < maxTicks) {
        // Find the closest data point to this time
        const closestIdx = findClosestTimeIndex(timeData, currentTickTime, startIdx, endIdx);

        if (closestIdx >= startIdx && closestIdx <= endIdx && !ticksMap.has(closestIdx)) {
            ticksMap.add(closestIdx);
        }

        // Move to next interval
        currentTickTime += intervalSeconds;
    }

    // Convert the Set back to a sorted array
    return Array.from(ticksMap).sort((a, b) => a - b);
}

/**
 * Find the index of the closest timestamp in an array
 * @param {Array} timeArray - Array of timestamps
 * @param {number} targetTime - Target timestamp to find
 * @param {number} startIdx - Start index for search (optional)
 * @param {number} endIdx - End index for search (optional)
 * @returns {number} Index of the closest timestamp
 */
export function findClosestTimeIndex(timeArray, targetTime, startIdx = 0, endIdx = timeArray.length - 1) {
    let closestIdx = startIdx;
    let minDiff = Math.abs(timeArray[startIdx] - targetTime);

    for (let i = startIdx + 1; i <= endIdx; i++) {
        if (i >= timeArray.length) break;

        const diff = Math.abs(timeArray[i] - targetTime);
        if (diff < minDiff) {
            minDiff = diff;
            closestIdx = i;
        }

        // If we're getting farther away, we can stop searching
        if (i > startIdx + 1 && diff > minDiff) {
            break;
        }
    }

    return closestIdx;
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

/**
 * Updates all charts after a zoom operation with reliable time ticks
 * IMPROVED: Ensures ticks are always visible and properly aligned
 * @param {number} start - Start of zoom range (percentage)
 * @param {number} end - End of zoom range (percentage)
 * @param {Object} state - Global state object
 */
export function updateChartsAfterZoom(start, end, state) {
    // Clear existing tick configuration to force recalculation
    state.sharedAxisConfig.visibleTicks = [];
    state.sharedAxisConfig.lastUpdate = 0;

    // Update the tracked zoom state
    state.sharedAxisConfig.lastZoomState = `${start}-${end}`;

    // Find the first chart with original time data
    let referenceTimeData = null;

    for (const chart of state.initializedCharts.values()) {
        if (chart.originalTimeData && chart.originalTimeData.length > 0) {
            referenceTimeData = chart.originalTimeData;
            break;
        }
    }

    // Calculate human-friendly ticks if we have reference time data
    if (referenceTimeData) {
        const ticks = calculateHumanFriendlyTicks(referenceTimeData, start, end);
        state.sharedAxisConfig.visibleTicks = ticks;
        state.sharedAxisConfig.lastUpdate = Date.now();
    }

    // IMPROVEMENT: Ensure we always have visible ticks
    if (state.sharedAxisConfig.visibleTicks.length === 0 && referenceTimeData) {
        // Emergency fallback - create basic ticks
        const startIdx = Math.floor(referenceTimeData.length * (start / 100));
        const endIdx = Math.ceil(referenceTimeData.length * (end / 100));

        // Add at least start and end points
        state.sharedAxisConfig.visibleTicks = [
            Math.max(0, startIdx),
            Math.min(referenceTimeData.length - 1, endIdx)
        ];

        // Add a middle point if range is large enough
        if (endIdx - startIdx > 2) {
            state.sharedAxisConfig.visibleTicks.splice(
                1, 0, Math.floor((startIdx + endIdx) / 2)
            );
        }
    }

    // Update all charts with new zoom level
    state.initializedCharts.forEach((chart) => {
        // Apply zoom to all charts
        chart.dispatchAction({
            type: 'dataZoom',
            start: start,
            end: end
        });

        // The key fix: Update the chart to use the original timestamps for tick labels
        // We'll use the axisLabel formatter to ensure correct time display regardless of zoom
        chart.setOption({
            xAxis: {
                axisLabel: {
                    // This interval function controls which tick marks are shown
                    interval: function (index) {
                        return state.sharedAxisConfig.visibleTicks.includes(index);
                    },
                    // The formatter will use the original timestamp data, not index-based lookup
                    formatter: function (value, index) {
                        // Only use this for visible ticks to avoid unnecessary processing
                        if (state.sharedAxisConfig.visibleTicks.includes(index) && chart.originalTimeData) {
                            const timestamp = chart.originalTimeData[index];
                            if (timestamp !== undefined) {
                                const date = new Date(timestamp * 1000);
                                const seconds = date.getSeconds();
                                const minutes = date.getMinutes();
                                const hours = date.getHours();

                                // Show more details based on boundary alignment
                                if (seconds === 0 && minutes === 0) {
                                    return `${String(hours).padStart(2, '0')}:00`;
                                } else if (seconds === 0) {
                                    return `${String(hours).padStart(2, '0')}:${String(minutes).padStart(2, '0')}`;
                                } else if (seconds % 30 === 0) {
                                    return `${String(hours).padStart(2, '0')}:${String(minutes).padStart(2, '0')}:${String(seconds).padStart(2, '0')}`;
                                } else if (seconds % 15 === 0) {
                                    return `${String(hours).padStart(2, '0')}:${String(minutes).padStart(2, '0')}:${String(seconds).padStart(2, '0')}`;
                                } else if (seconds % 5 === 0) {
                                    return `${String(hours).padStart(2, '0')}:${String(minutes).padStart(2, '0')}:${String(seconds).padStart(2, '0')}`;
                                }
                                return `${String(hours).padStart(2, '0')}:${String(minutes).padStart(2, '0')}:${String(seconds).padStart(2, '0')}`;
                            }
                        }
                        // Default case - use the provided value if no timestamp available
                        return value;
                    }
                }
            }
        });
    });
}