// line.js - Line chart configuration and rendering with standardized spacing

import { createAxisLabelFormatter, createTooltipFormatter } from './units.js';
import { calculateSharedVisibleTicks, formatDateTime } from './utils.js';

/**
 * Creates a line chart configuration for ECharts
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

  // Use consistent formatting for timestamps
  const formattedTimeData = timeData.map(timestamp => formatDateTime(timestamp, 'time'));

  const valueData = data[1];

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
        
        // Get the timestamp
        const timestamp = params[0].axisValue;
        
        // Start with the timestamp
        let result = `<div>${timestamp}</div>`;
        
        // Add each series with right-justified values using flexbox
        params.forEach(param => {
          // Format the value according to unit system
          let formattedValue;
          if (param.value !== undefined && param.value !== null) {
            formattedValue = createAxisLabelFormatter(unitSystem)(param.value);
          } else {
            formattedValue = "N/A";
          }
          
          // Create a flex container with the series on the left and value on the right
          result += 
            `<div style="display:flex;justify-content:space-between;align-items:center;margin:3px 0;">
              <div>
                <span style="display:inline-block;margin-right:5px;border-radius:50%;width:10px;height:10px;background-color:${param.color};"></span> 
                ${param.seriesName}
              </div>
              <div style="margin-left:15px;"><strong>${formattedValue}</strong></div>
            </div>`;
        });
        
        return result;
      }
    };
  }

  // Standardized grid with consistent spacing for all charts
  const updatedGrid = {
    left: '14%',  // Fixed generous margin for all charts
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

  // Return line chart configuration
  return {
    ...baseOption,
    grid: updatedGrid,
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
    series: [{
      data: valueData,
      type: 'line',
      name: opts.title,
      showSymbol: false,
      emphasis: {
        focus: 'series'
      },
      lineStyle: {
        width: 2
      },
      animationDuration: 0
    }]
  };
}