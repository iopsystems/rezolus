// utils.js - Common utility functions for chart rendering

/**
 * Helper function to format dates consistently across chart types
 * @param {number} timestamp - Unix timestamp in seconds
 * @param {string} format - Format type: 'time', 'short', or 'full'
 * @returns {string} Formatted date/time string
 */
export function formatDateTime(timestamp, format = 'time') {
  const date = new Date(timestamp * 1000);
  const isoString = date.toISOString().replace('T', ' ').substr(0, 19);

  if (format === 'time') {
    // Return only the time portion (HH:MM:SS)
    return isoString.split(' ')[1];
  } else if (format === 'short') {
    // Return HH:MM format for compact display
    return isoString.split(' ')[1].substr(0, 5);
  } else {
    // Return the full datetime
    return isoString;
  }
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
 * Updates all charts after a zoom operation
 * @param {number} start - Start of zoom range (percentage)
 * @param {number} end - End of zoom range (percentage)
 * @param {Object} state - Global state object
 */
export function updateChartsAfterZoom(start, end, state) {
  // Clear existing tick configuration
  state.sharedAxisConfig.visibleTicks = [];
  state.sharedAxisConfig.lastUpdate = 0;

  // Get the first chart to calculate shared ticks (if available)
  let sharedTicks = [];
  const firstChart = state.initializedCharts.values().next().value;
  if (firstChart) {
    const chartOption = firstChart.getOption();
    if (chartOption.xAxis && chartOption.xAxis[0] && chartOption.xAxis[0].data) {
      const dataLength = chartOption.xAxis[0].data.length;
      sharedTicks = calculateSharedVisibleTicks(dataLength, start, end);

      // Store in shared config for other charts
      state.sharedAxisConfig.visibleTicks = sharedTicks;
      state.sharedAxisConfig.lastUpdate = Date.now();
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

    // Update the chart with new axis configuration
    chart.setOption({
      xAxis: {
        axisLabel: {
          interval: function(index) {
            return state.sharedAxisConfig.visibleTicks.includes(index);
          }
        }
      }
    });
  });
}

/**
 * Set up synchronization between charts
 * @param {Array} charts - Array of ECharts instances to synchronize
 * @param {Object} state - Global state object
 */
export function setupChartSync(charts, state) {
  charts.forEach(mainChart => {
    // Setup brush events for zooming
    mainChart.on('brushSelected', function(params) {
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
    mainChart.getZr().on('dblclick', function() {
      // Prevent infinite recursion
      if (state.isZoomSyncing) return;

      try {
        // Set synchronization flag
        state.isZoomSyncing = true;

        // Reset the global zoom state
        state.globalZoom.start = 0;
        state.globalZoom.end = 100;
        state.globalZoom.isZoomed = false;

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
    mainChart.on('updateAxisPointer', function(event) {
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
    mainChart.on('dataZoom', function(event) {
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