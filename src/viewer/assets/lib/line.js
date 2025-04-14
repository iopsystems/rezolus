// line.js - Updated Line chart configuration and rendering with improved time axis

import { createAxisLabelFormatter, createTooltipFormatter } from './units.js';
import { calculateHumanFriendlyTicks, formatTimeAxisLabel } from './utils.js';

/**
 * Creates a line chart configuration for ECharts with improved time axis
 * 
 * @param {Object} baseOption - Base chart options
 * @param {Object} plotSpec - Plot specification with data and options
 * @param {Object} state - Global state object for synchronization
 * @returns {Object} ECharts configuration object
 */
export function createLineChartOption(baseOption, plotSpec, state) {
  const { data, opts } = plotSpec;

  if (!data || data.length < 2) {
    return baseOption;
  }

  // For line charts, we expect the classic 2-row format: [times, values]
  const timeData = data[0];

  // Store original timestamps for calculations
  const originalTimeData = timeData.slice();
  
  // Format timestamps for display
  const formattedTimeData = timeData.map(timestamp => {
    const date = new Date(timestamp * 1000);
    return `${String(date.getHours()).padStart(2, '0')}:${String(date.getMinutes()).padStart(2, '0')}:${String(date.getSeconds()).padStart(2, '0')}`;
  });

  const valueData = data[1];

  // Calculate ticks based on current zoom state
  let ticks;
  if (state.globalZoom.start === 0 && state.globalZoom.end === 100) {
    // For full view, calculate human-friendly ticks
    ticks = calculateHumanFriendlyTicks(
      originalTimeData,
      0,
      100
    );
  } else {
    // For zoomed view
    ticks = calculateHumanFriendlyTicks(
      originalTimeData,
      state.globalZoom.start,
      state.globalZoom.end
    );
  }
  
  // Store in shared config for chart synchronization
  state.sharedAxisConfig.visibleTicks = ticks;
  state.sharedAxisConfig.lastUpdate = Date.now();

  // Rest of your chart configuration code...
  
  // X-axis configuration with better labeling
  const xAxis = {
    type: 'category',
    data: formattedTimeData,
    axisLine: {
      lineStyle: {
        color: '#ABABAB'
      }
    },
    axisLabel: {
      color: '#ABABAB',
      formatter: function(value, index) {
        // Use our enhanced formatter for time axis labels
        return formatTimeAxisLabel(value, index, originalTimeData);
      },
      // Use calculated ticks for interval selection
      interval: function(index) {
        return state.sharedAxisConfig.visibleTicks.includes(index);
      }
    }
  };
  
  // Return the configured chart options
  return {
    ...baseOption,
    // Include your grid, tooltip, etc. configuration here
    xAxis: xAxis,
    // Include your yAxis and series configuration here
  };
}

/**
 * Updates all charts after a zoom operation with improved time ticks
 * @param {number} start - Start of zoom range (percentage)
 * @param {number} end - End of zoom range (percentage)
 * @param {Object} state - Global state object
 */
export function updateChartsAfterZoom(start, end, state) {
  // Clear existing tick configuration to force recalculation
  state.sharedAxisConfig.visibleTicks = [];
  state.sharedAxisConfig.lastUpdate = 0;

  // Find a visible chart with time data to use as reference
  let referenceTimeData = null;
  
  for (const chart of state.initializedCharts.values()) {
    if (isChartVisible(chart.getDom())) {
      const option = chart.getOption();
      if (option.xAxis && option.xAxis[0] && option.series && option.series[0]) {
        // For line charts, the original time data is often accessible
        if (option.series[0].type === 'line' && option.series[0]._rawData) {
          referenceTimeData = option.series[0]._rawData[0];
          break;
        }
      }
    }
  }

  // Calculate improved time ticks if we have reference data
  if (referenceTimeData) {
    const ticks = calculateHumanFriendlyTicks(referenceTimeData, start, end);
    state.sharedAxisConfig.visibleTicks = ticks;
    state.sharedAxisConfig.lastUpdate = Date.now();
  }

  // Update all charts
  state.initializedCharts.forEach((chart) => {
    // Apply zoom to chart
    chart.dispatchAction({
      type: 'dataZoom',
      start: start,
      end: end
    });
    
    // Update axis configuration for visible charts
    if (isChartVisible(chart.getDom())) {
      chart.setOption({
        xAxis: {
          axisLabel: {
            interval: function(index) {
              return state.sharedAxisConfig.visibleTicks.includes(index);
            }
          }
        }
      });
    } else {
      // Mark invisible charts for update when they become visible
      state.chartsNeedingZoomUpdate.add(chart);
    }
  });
}