// heatmap.js - Heatmap chart configuration with fixed time axis handling

import {
  createAxisLabelFormatter
} from './units.js';
import {
  calculateHumanFriendlyTicks,
  formatTimeAxisLabel,
  formatDateTime
} from './utils.js';

/**
 * Creates a heatmap chart configuration for ECharts with reliable time axis
 * 
 * @param {Object} baseOption - Base chart options
 * @param {Object} plotSpec - Plot specification with data and options
 * @param {Object} state - Global state object for synchronization
 * @returns {Object} ECharts configuration object
 */
export function createHeatmapOption(baseOption, plotSpec, state) {
  const {
    time_data,
    data,
    min_value,
    max_value,
    opts
  } = plotSpec;

  if (!data || data.length < 1) {
    return baseOption;
  }

  // Store original timestamps for calculations - critical for reliable zooming
  const originalTimeData = time_data.slice();

  // Format timestamps for display
  const formattedTimeData = originalTimeData.map(timestamp =>
    formatDateTime(timestamp, 'time')
  );

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

    // Use original timestamp for reliable display even during zoom/pan
    const fullTime = timeIndex >= 0 && timeIndex < originalTimeData.length ?
      originalTimeData[timeIndex] :
      Date.now() / 1000;

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
    left: '14%', // Fixed generous margin for all charts
    right: '5%',
    top: '40',
    bottom: '40',
    containLabel: false
  };

  // THE FIX: Create a more reliable X-axis configuration
  const xAxis = {
    type: 'category',
    data: formattedTimeData,
    splitArea: {
      show: true
    },
    axisLabel: {
      color: '#ABABAB',
      // The critical part: use actual timestamps instead of index-based formatting
      formatter: function(value, index) {
        // Use the original timestamp data for consistent time display
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
        return value;
      },
      // Use human-friendly tick intervals
      interval: function(index) {
        return ticks.includes(index);
      }
    }
  };

  return {
    ...baseOption,
    tooltip: {
      position: 'top',
      formatter: tooltipFormatter
    },
    grid: updatedGrid,
    xAxis: xAxis,
    yAxis: {
      type: 'category',
      name: yAxisLabel || 'CPU',
      nameLocation: 'middle',
      nameGap: 40,
      nameTextStyle: {
        color: '#E0E0E0',
        fontSize: 14,
        padding: [0, 0, 0, 20]
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
      progressive: 1000,
      progressiveThreshold: 3000,
      animation: false
    }]
  };
}