// multi.js - Multi-series chart configuration with deterministic cgroup colors

import {
    createAxisLabelFormatter,
    createTooltipFormatter
} from './units.js';
import {
    calculateHumanFriendlyTicks,
    formatTimeAxisLabel,
    formatDateTime
} from './utils.js';
import globalColorMapper from './colormap.js';

/**
 * Creates a multi-series line chart configuration for ECharts with reliable time axis
 * and consistent cgroup colors across charts and page refreshes
 * Enhanced to support an "Other" category that sums all cgroups not in top/bottom N
 * 
 * @param {Object} baseOption - Base chart options
 * @param {Object} plotSpec - Plot specification with data and options
 * @param {Object} state - Global state object for synchronization
 * @returns {Object} ECharts configuration object
 */
export function createMultiSeriesChartOption(baseOption, plotSpec, state) {
    const {
        data,
        opts,
        series_names
    } = plotSpec;

    if (!data || data.length < 2) {
        return baseOption;
    }

    // For multi-series charts, the first row contains timestamps, subsequent rows are series data
    const timeData = data[0];

    // Store original timestamps for calculations - critical for reliable zooming
    const originalTimeData = timeData.slice();

    // Format timestamps for display
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
                function (value) {
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

    // Create a reliable time axis configuration
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
            // Use human-friendly tick intervals
            interval: function (index) {
                return ticks.includes(index);
            }
        }
    };

    // Create series configurations for each data series
    const series = [];

    // Use provided series names or generate default ones
    const names = plotSpec.series_names || [];

    // Get deterministic colors for all cgroups in this chart
    const cgroupColors = globalColorMapper.getColors(names);

    // Special "Other" category styling - always make it a gray line for consistency
    const otherLineStyle = {
        width: 2,
        color: '#888888'  // medium gray
    };

    for (let i = 1; i < data.length; i++) {
        // Get the series name (use provided name or default to "Series N")
        const name = (i <= names.length && names[i - 1]) ? names[i - 1] : `Series ${i}`;

        // Check if this is the "Other" category
        const isOtherCategory = name === "Other";

        // Determine line style and color
        let lineStyle, itemColor;

        if (isOtherCategory) {
            // Use special styling for "Other" category
            lineStyle = otherLineStyle;
            itemColor = otherLineStyle.color;
        } else {
            // Use deterministic color from our global mapper for normal cgroups
            const color = (i <= cgroupColors.length) ? cgroupColors[i - 1] : undefined;
            lineStyle = {
                color: color,
                width: 2
            };
            itemColor = color;
        }

        series.push({
            name: name,
            type: 'line',
            data: data[i],
            // Apply appropriate styling
            itemStyle: {
                color: itemColor
            },
            lineStyle: lineStyle,
            // Add symbol for "Other" category to make it more distinguishable
            showSymbol: isOtherCategory,
            symbolSize: isOtherCategory ? 4 : 0,
            // Ensure "Other" appears behind other lines
            z: isOtherCategory ? 1 : 2,
            emphasis: {
                focus: 'series'
            },
            animationDuration: 0
        });
    }

    // Always ensure "Other" category is the last in the series array
    // This ensures it appears in the legend last and gets rendered first (behind other lines)
    const otherIndex = series.findIndex(s => s.name === "Other");
    if (otherIndex !== -1 && otherIndex !== series.length - 1) {
        const otherSeries = series.splice(otherIndex, 1)[0];
        series.push(otherSeries);
    }

    // Return the complete chart configuration
    return {
        ...baseOption,
        grid: updatedGrid,
        tooltip: tooltipFormatter ? {
            ...baseOption.tooltip,
            ...tooltipFormatter
        } : baseOption.tooltip,
        xAxis: xAxis,
        yAxis: yAxis,
        series: series,
        // Don't use the default color palette for normal cgroups
        color: cgroupColors,
    };
}