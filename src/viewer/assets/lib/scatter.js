// scatter.js - Scatter chart configuration with fixed time axis handling

import {
    createAxisLabelFormatter,
    createTooltipFormatter
} from './units.js';
import {
    calculateHumanFriendlyTicks,
    formatTimeAxisLabel,
    formatDateTime
} from './utils.js';

/**
 * Creates a scatter chart configuration for ECharts with reliable time axis
 * 
 * @param {Object} baseOption - Base chart options
 * @param {Object} plotSpec - Plot specification with data and options
 * @param {Object} state - Global state object for synchronization
 * @returns {Object} ECharts configuration object
 */
export function createScatterChartOption(baseOption, plotSpec, state) {
    const {
        data,
        opts
    } = plotSpec;

    if (!data || data.length < 2) {
        return baseOption;
    }

    // For percentile data, the format is [times, percentile1Values, percentile2Values, ...]
    const timeData = data[0];

    // Store original timestamps for calculations - critical for reliable zooming
    const originalTimeData = timeData.slice();

    // Format timestamps using our enhanced formatter
    const formattedTimeData = originalTimeData.map(timestamp =>
        formatDateTime(timestamp, 'time')
    );

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

    // Create series for each percentile
    const series = [];

    // Determine percentiles based on the data structure
    // Assuming data format: [timestamps, p50values, p99values, ...]
    const percentileLabels = ['p50', 'p90', 'p99', 'p99.9', 'p99.99']; // Default labels, can be customized

    for (let i = 1; i < data.length; i++) {
        const percentileData = [];
        const percentileValues = data[i];

        // Create data points in the format [time, value, original_index]
        for (let j = 0; j < timeData.length; j++) {
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
            formatter: function (params) {
                if (!Array.isArray(params)) params = [params];

                // Get the original timestamp using the stored index
                // This is the key improvement for reliable time display during zoom/pan
                const originalIndex = params[0].data[2]; // Third value in data array
                const fullTimestamp = (originalIndex >= 0 && originalIndex < originalTimeData.length) ?
                    formatDateTime(originalTimeData[originalIndex], 'full') :
                    formatDateTime(Date.now() / 1000, 'full');

                // Start with the timestamp
                let result = `<div>${fullTimestamp}</div>`;

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

    // Detect if this is a scheduler or time-based chart by looking at title or unit
    const isSchedulerChart =
        (plotSpec.opts.title && (plotSpec.opts.title.includes('Latency') || plotSpec.opts.title.includes('Time'))) ||
        unitSystem === 'time';

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
            margin: 12, // Fixed consistent margin for all charts
            formatter: unitSystem ?
                createAxisLabelFormatter(unitSystem) :
                function (value) {
                    // Format log scale labels more compactly if needed
                    if (logScale && Math.abs(value) >= 1000) {
                        return value.toExponential(0);
                    }
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
            formatter: function (value, index) {
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
            interval: function (index) {
                return ticks.includes(index);
            }
        }
    };

    // Return scatter chart configuration with reliable time axis
    return {
        ...baseOption,
        grid: updatedGrid,
        tooltip: tooltipFormatter ? {
            ...baseOption.tooltip,
            ...tooltipFormatter
        } : baseOption.tooltip,
        xAxis: xAxis,
        yAxis: yAxis,
        series: series
    };
}