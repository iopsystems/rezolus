// scatter.js - Scatter chart configuration and rendering

import { createAxisLabelFormatter, createTooltipFormatter } from './units.js';
import { calculateSharedVisibleTicks, formatDateTime } from './utils.js';

/**
 * Creates a scatter chart configuration for ECharts
 * 
 * @param {Object} baseOption - Base chart options
 * @param {Object} plotSpec - Plot specification with data and options
 * @param {Object} state - Global state object for synchronization
 * @returns {Object} ECharts configuration object
 */
export function createScatterChartOption(baseOption, plotSpec, state) {
  const { data, opts } = plotSpec;

  if (!data || data.length < 2) {
    return baseOption;
  }

  // For percentile data, the format is [times, percentile1Values, percentile2Values, ...]
  const timeData = data[0];

  // Use consistent formatting for timestamps
  const formattedTimeData = timeData.map(timestamp => formatDateTime(timestamp, 'time'));

  // Use shared ticks if already calculated, otherwise calculate new ones
  if (state.sharedAxisConfig.visibleTicks.length === 0 ||
    Date.now() - state.sharedAxisConfig.lastUpdate > 1000) {
    // For full view (no zoom), use fewer ticks to prevent label pile-up
    if (state.globalZoom.start === 0 && state.globalZoom.end === 100) {
      const maxTicks = Math.min(8, timeData.length);
      const interval = Math.max(1, Math.floor(timeData.length / maxTicks));

      const ticks = [];
      for (let i = 0; i < timeData.length; i += interval) {
        ticks.push(i);
      }

      // Add last tick if not already included
      if (timeData.length > 0 && (timeData.length - 1) % interval !== 0) {
        ticks.push(timeData.length - 1);
      }

      state.sharedAxisConfig.visibleTicks = ticks;
    } else {
      // For zoomed view, calculate normal ticks
      state.sharedAxisConfig.visibleTicks = calculateSharedVisibleTicks(
        timeData.length,
        state.globalZoom.start,
        state.globalZoom.end
      );
    }
    state.sharedAxisConfig.lastUpdate = Date.now();
  }

  // Create series for each percentile
  const series = [];

  // Determine percentiles based on the data structure
  // Assuming data format: [timestamps, p50values, p99values, ...]
  const percentileLabels = ['p50', 'p90', 'p99', 'p99.9', 'p99.99']; // Default labels, can be customized

  for (let i = 1; i < data.length; i++) {
    const percentileData = [];
    const percentileValues = data[i];

    // Create data points in the format [time, value]
    for (let j = 0; j < timeData.length; j++) {
      if (percentileValues[j] !== undefined && !isNaN(percentileValues[j])) {
        percentileData.push([formattedTimeData[j], percentileValues[j]]);
      }
    }

    // Add a series for this percentile
    series.push({
      name: percentileLabels[i - 1] || `Percentile ${i}`,
      type: 'scatter',
      data: percentileData,
      symbolSize: 6,
      emphasis: {
        focus: 'series',
        itemStyle: {
          shadowBlur: 10,
          shadowColor: 'rgba(255, 255, 255, 0.5)'
        }
      }
    });
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
        if (!Array.isArray(params)) params = [params];
        
        // Get the timestamp (first value in data array for scatter plots)
        const timestamp = params[0].data[0];
        
        // Start with the timestamp
        let result = `<div>${timestamp}</div>`;
        
        // Add each series with right-justified values using flexbox
        params.forEach(param => {
          // Get the value (second value in data array for scatter plots)
          const value = param.data[1];
          
          // Format the value according to unit system
          let formattedValue;
          if (value !== undefined && value !== null) {
            formattedValue = createAxisLabelFormatter(unitSystem)(value);
          } else {
            formattedValue = "N/A";
          }
          
          // Create a flex container with the series on the left and value on the right
          result += 
            `<div style="display:flex;justify-content:space-between;align-items:center;margin:3px 0;">
              <div>
                <span style="display:inline-block;margin-right:5px;border-radius:50%;width:10px;height:10px;background-color:${param.color};"></span> 
                ${param.seriesName}:
              </div>
              <div style="margin-left:15px;"><strong>${formattedValue}</strong></div>
            </div>`;
        });
        
        return result;
      }
    };
  }

  // Create Y-axis configuration with label and unit formatting
  const yAxis = {
    type: logScale ? 'log' : 'value',
    logBase: 10,
    scale: true,
    name: yAxisLabel || opts.title,
    nameLocation: 'middle',
    nameGap: 50,
    nameTextStyle: {
      color: '#E0E0E0',
      fontSize: 14
    },
    axisLine: {
      lineStyle: {
        color: '#ABABAB'
      }
    },
    axisLabel: {
      color: '#ABABAB',
      formatter: unitSystem 
        ? createAxisLabelFormatter(unitSystem)
        : function(value) {
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

  // Return scatter chart configuration
  return {
    ...baseOption,
    tooltip: tooltipFormatter ? {...baseOption.tooltip, ...tooltipFormatter} : baseOption.tooltip,
    xAxis: {
      type: 'category',
      data: formattedTimeData,
      axisLine: {
        lineStyle: {
          color: '#ABABAB'
        }
      },
      axisLabel: {
        color: '#ABABAB',
        formatter: function(value) {
          return value; // Already formatted properly by formatDateTime
        },
        // Using custom ticks based on our shared configuration
        interval: function(index) {
          return state.sharedAxisConfig.visibleTicks.includes(index);
        }
      }
    },
    yAxis: yAxis,
    series: series
  };
}