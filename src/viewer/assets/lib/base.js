// base.js - Common chart configuration and axis handling to reduce duplication

import {
  createAxisLabelFormatter
} from './units.js';
import {
  calculateHumanFriendlyTicks,
  formatDateTime
} from './utils.js';

/**
 * Creates a base chart configuration that can be extended by specific chart types
 * 
 * @param {Object} baseOption - Base ECharts options
 * @param {Object} plotSpec - Plot specification with data and options
 * @param {Object} state - Global state object
 * @returns {Object} Common chart configuration
 */
export function createBaseChartConfig(baseOption, plotSpec, state) {
  const format = plotSpec.opts.format || {};
  const unitSystem = format.unit_system;
  const yAxisLabel = format.y_axis_label || format.axis_label;
  const logScale = format.log_scale;
  const minValue = format.min;
  const maxValue = format.max;
  
  // Extract time data from plot spec, avoiding duplicated logic
  let timeData, originalTimeData;
  
  if (plotSpec.time_data) {
    // For heatmaps which use a separate time_data property
    timeData = plotSpec.time_data;
    originalTimeData = timeData;
  } else if (plotSpec.data && plotSpec.data.length >= 1) {
    // For line, scatter and multi charts which use data[0] for time values
    timeData = plotSpec.data[0];
    originalTimeData = timeData;
  } else {
    // Fallback for empty data
    return baseOption;
  }
  
  // Calculate human-friendly ticks, avoiding duplicate calculations
  let ticks;
  const zoomState = `${state.globalZoom.start}-${state.globalZoom.end}`;
  
  // Check if we need to recalculate ticks
  if (state.sharedAxisConfig.lastZoomState !== zoomState ||
      state.sharedAxisConfig.visibleTicks.length === 0 ||
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
    state.sharedAxisConfig.lastZoomState = zoomState;
  } else {
    // Use existing ticks from shared config
    ticks = state.sharedAxisConfig.visibleTicks;
  }
  
  // Format time data just once
  const formattedTimeData = originalTimeData.map(timestamp =>
    formatDateTime(timestamp, 'time')
  );
  
  // Create common Y-axis configuration
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
      margin: 16, // Fixed consistent margin
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
  
  // Create common X-axis configuration
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
  
  // Create common tooltip formatter
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
          // Format the value according to unit system
          let formattedValue;
          if (param.value !== undefined && param.value !== null) {
            const value = Array.isArray(param.value) ? param.value[1] : param.value;
            formattedValue = createAxisLabelFormatter(unitSystem)(value);
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
  
  // Standardized grid with consistent spacing
  const grid = {
    left: '14%', // Fixed generous margin
    right: '5%',
    top: '40',
    bottom: '40',
    containLabel: false
  };
  
  // Return base chart configuration
  return {
    ...baseOption,
    grid: grid,
    tooltip: tooltipFormatter ? {...baseOption.tooltip, ...tooltipFormatter} : baseOption.tooltip,
    xAxis: xAxis,
    yAxis: yAxis,
    originalTimeData: originalTimeData, // Keep reference for internal use
    formattedTimeData: formattedTimeData
  };
}

/**
 * Creates a line chart configuration from the base configuration
 * 
 * @param {Object} baseConfig - Base chart configuration
 * @param {Object} plotSpec - Plot specification
 * @returns {Object} Line chart configuration
 */
export function createLineChartConfig(baseConfig, plotSpec) {
  if (!plotSpec.data || plotSpec.data.length < 2) {
    return baseConfig;
  }
  
  const valueData = plotSpec.data[1];
  
  return {
    ...baseConfig,
    series: [{
      data: valueData,
      type: 'line',
      name: plotSpec.opts.title,
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

/**
 * Creates a scatter chart configuration from the base configuration
 * 
 * @param {Object} baseConfig - Base chart configuration
 * @param {Object} plotSpec - Plot specification
 * @returns {Object} Scatter chart configuration
 */
export function createScatterChartConfig(baseConfig, plotSpec) {
  if (!plotSpec.data || plotSpec.data.length < 2) {
    return baseConfig;
  }
  
  // Create series for each percentile
  const series = [];
  const percentileLabels = ['p50', 'p90', 'p99', 'p99.9', 'p99.99']; // Default labels

  for (let i = 1; i < plotSpec.data.length; i++) {
    const percentileData = [];
    const percentileValues = plotSpec.data[i];
    const formattedTimeData = baseConfig.formattedTimeData;
    const originalTimeData = baseConfig.originalTimeData;

    // Create data points in the format [time, value, original_index]
    for (let j = 0; j < originalTimeData.length; j++) {
      if (percentileValues[j] !== undefined && !isNaN(percentileValues[j])) {
        percentileData.push([formattedTimeData[j], percentileValues[j], j]); // Store original index
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
  
  return {
    ...baseConfig,
    series: series
  };
}

/**
 * Creates a multi-series chart configuration from the base configuration
 * 
 * @param {Object} baseConfig - Base chart configuration
 * @param {Object} plotSpec - Plot specification 
 * @param {Object} state - Global state object
 * @returns {Object} Multi-series chart configuration
 */
export function createMultiSeriesChartConfig(baseConfig, plotSpec, state) {
  if (!plotSpec.data || plotSpec.data.length < 2) {
    return baseConfig;
  }
  
  // Create series configurations for each data series
  const series = [];
  
  // Use provided series names or generate default ones
  const names = plotSpec.series_names || [];
  
  // Get deterministic colors from the global mapper
  const cgroupColors = state.colorMapper.getColors(names);
  
  for (let i = 1; i < plotSpec.data.length; i++) {
    // Get the series name
    const name = (i <= names.length && names[i-1]) ? names[i-1] : `Series ${i}`;
    
    series.push({
      name: name,
      type: 'line',
      data: plotSpec.data[i],
      // Use deterministic color from global mapper
      itemStyle: {
        color: (i <= cgroupColors.length) ? cgroupColors[i-1] : undefined
      },
      lineStyle: {
        color: (i <= cgroupColors.length) ? cgroupColors[i-1] : undefined,
        width: 2
      },
      showSymbol: false,
      emphasis: {
        focus: 'series'
      },
      animationDuration: 0
    });
  }
  
  return {
    ...baseConfig,
    series: series,
    color: cgroupColors // Use consistent colors across charts
  };
}

/**
 * Creates a heatmap chart configuration from the base configuration
 * 
 * @param {Object} baseConfig - Base chart configuration
 * @param {Object} plotSpec - Plot specification
 * @returns {Object} Heatmap chart configuration
 */
export function createHeatmapChartConfig(baseConfig, plotSpec) {
  const {
    data,
    min_value,
    max_value,
    opts
  } = plotSpec;

  if (!data || data.length < 1) {
    return baseConfig;
  }
  
  // Extract all unique CPU IDs and timestamp indices
  const yIndices = new Set();
  
  data.forEach(item => {
    yIndices.add(item[1]); // CPU ID
  });

  // Convert to array and sort numerically
  const cpuIds = Array.from(yIndices).sort((a, b) => a - b);

  // Ensure we have a continuous range of CPUs from 0 to max
  const maxCpuId = cpuIds.length > 0 ? Math.max(...cpuIds) : 0;
  const continuousCpuIds = Array.from({
    length: maxCpuId + 1
  }, (_, i) => i);

  // Calculate min/max values if not provided
  let minValue = min_value !== undefined ? min_value : Infinity;
  let maxValue = max_value !== undefined ? max_value : -Infinity;

  if (minValue === Infinity || maxValue === -Infinity) {
    data.forEach(item => {
      const value = item[2];
      minValue = Math.min(minValue, value);
      maxValue = Math.max(maxValue, value);
    });
  }

  // Ensure maxValue is always slightly higher than minValue for visualization
  if (maxValue === minValue) {
    maxValue = minValue + 0.001;
  }

  // Access format properties for unit formatting
  const format = opts.format || {};
  const unitSystem = format.unit_system;
  const yAxisLabel = format.y_axis_label || format.axis_label;
  
  // Configure visualMap with unit formatting
  let visualMapFormatter;
  let visualMapText = ['High', 'Low'];

  if (unitSystem) {
    visualMapFormatter = createAxisLabelFormatter(unitSystem);

    // Create descriptive labels for the color scale
    const valueLabel = format.value_label;
    if (valueLabel) {
      visualMapText = [`High ${valueLabel}`, `Low ${valueLabel}`];
    } else if (yAxisLabel) {
      visualMapText = [`High ${yAxisLabel}`, `Low ${yAxisLabel}`];
    }
  }
  
  // Override the Y axis for heatmap (showing CPU IDs)
  const heatmapYAxis = {
    type: 'category',
    name: yAxisLabel || 'CPU',
    nameLocation: 'middle',
    nameGap: 40,
    nameTextStyle: {
      color: '#E0E0E0',
      fontSize: 14,
      padding: [0, 0, 0, 20]
    },
    data: continuousCpuIds,
    splitArea: {
      show: true
    },
    axisLabel: {
      color: '#ABABAB'
    }
  };
  
  // Add visualMap for heatmap coloring
  const visualMap = {
    min: minValue,
    max: maxValue,
    calculable: false,
    show: false,
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
  };
  
  // Return heatmap configuration
  return {
    ...baseConfig,
    yAxis: heatmapYAxis,
    visualMap: visualMap,
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