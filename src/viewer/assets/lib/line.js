// line.js - Line chart configuration and rendering with fixed time axis handling

import {
  createAxisLabelFormatter,
  createTooltipFormatter
} from './units.js';
import {
  calculateHumanFriendlyTicks,
  formatTimeAxisLabel,
  formatDateTime,
  interpolateValue
} from './utils.js';

/**
 * Creates a line chart configuration for ECharts with reliable time axis
 * Enhanced to properly handle sparse timeseries data
 * 
 * @param {Object} baseOption - Base chart options
 * @param {Object} plotSpec - Plot specification with data and options
 * @param {Object} state - Global state object for synchronization
 * @returns {Object} ECharts configuration object
 */
export function createLineChartOption(baseOption, plotSpec, state) {
  const {
    data,
    opts
  } = plotSpec;

  if (!data || data.length < 2) {
    return baseOption;
  }

  // For line charts, we expect the classic 2-row format: [times, values]
  const timeData = data[0];

  // Store original timestamps for calculations - critical for reliable zooming
  const originalTimeData = timeData.slice();

  // Format timestamps for display
  const formattedTimeData = originalTimeData.map(timestamp =>
    formatDateTime(timestamp, 'time')
  );

  // Process value data to properly handle missing points
  const rawValueData = data[1];
  const valueData = [];

  // Create a clean array with null values for missing data points
  for (let i = 0; i < rawValueData.length; i++) {
    if (rawValueData[i] !== undefined && rawValueData[i] !== null && !isNaN(rawValueData[i])) {
      valueData.push(rawValueData[i]);
    } else {
      // Use null to represent missing data points
      valueData.push(null);
    }
  }

  // Calculate human-friendly ticks
  let ticks;
  if (state.sharedAxisConfig.visibleTicks.length === 0 ||
    Date.now() - state.sharedAxisConfig.lastUpdate > 1000) {

    // Calculate ticks based on zoom state
    ticks = calculateHumanFriendlyTicks(
      originalTimeData,
      state.globalZoom.start,
      state.globalZoom.end
    );

    // Store in shared config for chart synchronization
    state.sharedAxisConfig.visibleTicks = ticks;
    state.sharedAxisConfig.lastUpdate = Date.now();
  } else {
    // Use existing ticks from shared config
    ticks = state.sharedAxisConfig.visibleTicks;
  }

  // Access format properties using snake_case naming to match Rust serialization
  const format = opts.format || {};
  const unitSystem = format.unit_system;
  const yAxisLabel = format.y_axis_label || format.axis_label;
  const valueLabel = format.value_label;
  const logScale = format.log_scale;
  const minValue = format.min;
  const maxValue = format.max;

  // Configure tooltip with unit formatting if specified
  let tooltipFormatter;
  if (unitSystem) {
    tooltipFormatter = {
      formatter: function(params) {
        // Handle both array of params and single param
        if (!Array.isArray(params)) params = [params];

        // Get the timestamp from the original data, not the formatted string
        const index = params[0].dataIndex;
        
        // Use the original timestamp to ensure correct time display
        const fullTimestamp = (index >= 0 && index < originalTimeData.length) ?
          formatDateTime(originalTimeData[index], 'full') :
          formatDateTime(Date.now() / 1000, 'full');

        // Start with the timestamp
        let result = `<div>${fullTimestamp}</div>`;

        // Add each series with right-justified values using flexbox
        params.forEach(param => {
          // Check if the value is valid
          if (param.value !== null && param.value !== undefined && !isNaN(param.value)) {
            // Format the value according to unit system
            let formattedValue = createAxisLabelFormatter(unitSystem)(param.value);

            // Create a flex container with the series on the left and value on the right
            result +=
              `<div style="display:flex;justify-content:space-between;align-items:center;margin:3px 0;">
                <div>
                  <span style="display:inline-block;margin-right:5px;border-radius:50%;width:10px;height:10px;background-color:${param.color};"></span> 
                  ${param.seriesName}
                </div>
                <div style="margin-left:15px;"><strong>${formattedValue}</strong></div>
              </div>`;
          } else {
            // If the value is invalid (null/undefined/NaN), optionally show a placeholder
            // or simply skip showing this series in the tooltip
            /* 
            result +=
              `<div style="display:flex;justify-content:space-between;align-items:center;margin:3px 0;opacity:0.5;">
                <div>
                  <span style="display:inline-block;margin-right:5px;border-radius:50%;width:10px;height:10px;background-color:${param.color};"></span> 
                  ${param.seriesName}
                </div>
                <div style="margin-left:15px;"><strong>N/A</strong></div>
              </div>`;
            */
          }
        });

        return result;
      }
    };
  }

  // Standardized grid with consistent spacing for all charts
  const updatedGrid = {
    left: '14%', // Fixed generous margin for all charts
    right: '5%',
    top: '40',
    bottom: '40',
    containLabel: false
  };

  // Create Y-axis configuration with label and unit formatting
  const yAxis = {
    type: logScale ? 'log' : 'value',
    logBase: 10,
    scale: true,
    axisLine: {
      lineStyle: {
        color: '#ABABAB'
      }
    },
    axisLabel: {
      color: '#ABABAB',
      margin: 16, // Fixed consistent margin for all charts
      formatter: unitSystem ?
        createAxisLabelFormatter(unitSystem) :
        function(value) {
          // Use scientific notation for large/small numbers
          if (Math.abs(value) > 10000 || (Math.abs(value) > 0 && Math.abs(value) < 0.01)) {
            return value.toExponential(1);
          }
          return value;
        }
    },
    splitLine: {
      lineStyle: {
        color: 'rgba(171, 171, 171, 0.2)'
      }
    }
  };

  // Set min/max if specified
  if (minValue !== undefined) yAxis.min = minValue;
  if (maxValue !== undefined) yAxis.max = maxValue;

  // THE FIX: Use a more reliable time axis configuration that preserves the relationship
  // between data indices and their corresponding timestamps during zoom operations
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
      // The critical part: use actual timestamps instead of index-based formatting
      formatter: function(value, index) {
        // Use the original timestamp data for consistent time display
        // This ensures time labels remain accurate during zoom/pan operations
        if (index >= 0 && index < originalTimeData.length) {
          const timestamp = originalTimeData[index];
          const date = new Date(timestamp * 1000);

          const seconds = date.getSeconds();
          const minutes = date.getMinutes();
          const hours = date.getHours();

          // Format based on time boundaries for better readability
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
        // Fallback to the provided value if we can't find the timestamp
        return value;
      },
      // Use human-friendly tick intervals as calculated
      interval: function(index) {
        return ticks.includes(index);
      }
    }
  };

  // Return line chart configuration with reliable time axis and null handling
  return {
    ...baseOption,
    grid: updatedGrid,
    tooltip: tooltipFormatter ? {...baseOption.tooltip,
      ...tooltipFormatter
    } : baseOption.tooltip,
    xAxis: xAxis,
    yAxis: yAxis,
    series: [{
      data: valueData,
      type: 'line',
      name: opts.title,
      showSymbol: false,
      // Add connectNulls to handle sparse data by drawing lines across gaps
      connectNulls: true,
      // Sampling for large datasets
      sampling: 'average',
      emphasis: {
        focus: 'series'
      },
      lineStyle: {
        width: 2
      },
      // Turn off animation for better performance with sparse data
      animation: false,
      // Large dataset optimization
      large: true,
      largeThreshold: 1000
    }]
  };
}