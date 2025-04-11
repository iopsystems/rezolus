// heatmap.js - Heatmap chart configuration and rendering with improved Y-axis positioning

import { createAxisLabelFormatter } from './units.js';
import { calculateSharedVisibleTicks, formatDateTime } from './utils.js';

/**
 * Creates a heatmap chart configuration for ECharts
 * 
 * @param {Object} baseOption - Base chart options
 * @param {Object} plotSpec - Plot specification with data and options
 * @param {Object} state - Global state object for synchronization
 * @returns {Object} ECharts configuration object
 */
export function createHeatmapOption(baseOption, plotSpec, state) {
  const { time_data, data, min_value, max_value, opts } = plotSpec;

  if (!data || data.length < 1) {
    return baseOption;
  }

  // Get unique x indices (timestamps) and y indices (CPUs)
  const xIndices = new Set();
  const yIndices = new Set();

  // Extract all unique CPU IDs and timestamp indices
  data.forEach(item => {
    xIndices.add(item[0]); // timestamp index
    yIndices.add(item[1]); // CPU ID
  });

  // Convert to array and sort numerically
  const cpuIds = Array.from(yIndices).sort((a, b) => a - b);

  // Ensure we have a continuous range of CPUs from 0 to max
  const maxCpuId = cpuIds.length > 0 ? Math.max(...cpuIds) : 0;
  const continuousCpuIds = Array.from({
    length: maxCpuId + 1
  }, (_, i) => i);

  // Use consistent formatting for time values
  const formattedTimeData = time_data.map(timestamp => formatDateTime(timestamp, 'time'));

  // Calculate min/max values if not provided by backend
  let minValue = min_value !== undefined ? min_value : Infinity;
  let maxValue = max_value !== undefined ? max_value : -Infinity;

  if (minValue === Infinity || maxValue === -Infinity) {
    data.forEach(item => {
      const value = item[2];
      minValue = Math.min(minValue, value);
      maxValue = Math.max(maxValue, value);
    });
  }

  // Use shared ticks for formatting
  if (state.sharedAxisConfig.visibleTicks.length === 0 ||
    Date.now() - state.sharedAxisConfig.lastUpdate > 1000) {
    // For full view (no zoom), use fewer ticks to prevent label pile-up
    if (state.globalZoom.start === 0 && state.globalZoom.end === 100) {
      const maxTicks = Math.min(8, time_data.length);
      const interval = Math.max(1, Math.floor(time_data.length / maxTicks));

      const ticks = [];
      for (let i = 0; i < time_data.length; i += interval) {
        ticks.push(i);
      }

      // Add last tick if not already included
      if (time_data.length > 0 && (time_data.length - 1) % interval !== 0) {
        ticks.push(time_data.length - 1);
      }

      state.sharedAxisConfig.visibleTicks = ticks;
    } else {
      // For zoomed view, calculate normal ticks
      state.sharedAxisConfig.visibleTicks = calculateSharedVisibleTicks(
        time_data.length,
        state.globalZoom.start,
        state.globalZoom.end
      );
    }
    state.sharedAxisConfig.lastUpdate = Date.now();
  }

  // Ensure maxValue is always at least slightly higher than minValue for visualization
  if (maxValue === minValue) {
    maxValue = minValue + 0.001;
  }

  // Access format properties using snake_case naming to match Rust serialization
  const format = opts.format || {};
  const unitSystem = format.unit_system;
  const yAxisLabel = format.y_axis_label || format.axis_label;
  const valueLabel = format.value_label;

  // Configure tooltip with unit formatting if specified
  let tooltipFormatter = function(params) {
    const value = params.data[2];
    const timeIndex = params.data[0];
    const fullTime = time_data[timeIndex];
    const formattedTime = formatDateTime(fullTime, 'full'); // Use full format for tooltip
    const cpu = params.data[1];
    
    if (unitSystem) {
      const formatter = createAxisLabelFormatter(unitSystem);
      const labelName = valueLabel || 'Value';
      return `Time: ${formattedTime}<br>CPU: ${cpu}<br>${labelName}: ${formatter(value)}`;
    } else {
      return `Time: ${formattedTime}<br>CPU: ${cpu}<br>Value: ${value.toFixed(6)}`;
    }
  };

  // Create formatted labels for visualMap if unit system is specified
  let visualMapFormatter;
  let visualMapText = ['High', 'Low'];
  
  if (unitSystem) {
    visualMapFormatter = createAxisLabelFormatter(unitSystem);
    
    // Create descriptive labels for the color scale
    if (valueLabel) {
      visualMapText = [`High ${valueLabel}`, `Low ${valueLabel}`];
    } else if (yAxisLabel) {
      visualMapText = [`High ${yAxisLabel}`, `Low ${yAxisLabel}`];
    }
  }

  // Standardized grid with consistent spacing for all charts
  const updatedGrid = {
    left: '14%',  // Fixed generous margin for all charts
    right: '5%',
    top: '40',
    bottom: '40',
    containLabel: false
  };

  return {
    ...baseOption,
    tooltip: {
      position: 'top',
      formatter: tooltipFormatter
    },
    grid: updatedGrid,
    xAxis: {
      type: 'category',
      data: formattedTimeData,
      splitArea: {
        show: true
      },
      axisLabel: {
        color: '#ABABAB',
        formatter: function(value) {
          // Show time only format for x-axis labels, already properly formatted
          return value;
        },
        // Use the same tick interval configuration as line charts
        interval: function(index) {
          return state.sharedAxisConfig.visibleTicks.includes(index);
        }
      }
    },
    yAxis: {
      type: 'category',
      name: yAxisLabel || 'CPU',
      nameLocation: 'middle',
      nameGap: 95, // Fixed consistent nameGap for all charts
      nameTextStyle: {
        color: '#E0E0E0',
        fontSize: 14
      },
      data: continuousCpuIds, // Use the continuous range of CPU IDs
      splitArea: {
        show: true
      },
      axisLabel: {
        color: '#ABABAB'
      }
    },
    visualMap: {
      min: minValue,
      max: maxValue,
      calculable: false,
      show: false, // Show the color scale
      orient: 'horizontal',
      left: 'center',
      bottom: '0%',
      textStyle: {
        color: '#E0E0E0'
      },
      formatter: visualMapFormatter,
      text: visualMapText,
      inRange: {
        color: [
          '#440154', '#481a6c', '#472f7d', '#414487', '#39568c',
          '#31688e', '#2a788e', '#23888e', '#1f988b', '#22a884',
          '#35b779', '#54c568', '#7ad151', '#a5db36', '#d2e21b', '#fde725'
        ]
      }
    },
    series: [{
      name: plotSpec.opts.title,
      type: 'heatmap',
      data: data,
      emphasis: {
        itemStyle: {
          shadowBlur: 10,
          shadowColor: 'rgba(0, 0, 0, 0.5)'
        }
      },
      progressive: 2000,
      animation: false
    }]
  };
}