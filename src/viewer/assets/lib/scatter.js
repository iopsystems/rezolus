// scatter.js - Scatter chart configuration with fixed time axis handling

import {
    createAxisLabelFormatter,
} from './units.js';

/**
 * Creates a scatter chart configuration for ECharts
 * 
 * @param {Object} baseOption - Base chart options
 * @param {Object} plotSpec - Plot specification with data and options
 * @returns {Object} ECharts configuration object
 */
export function createScatterChartOption(baseOption, plotSpec) {
    const {
        data,
        opts
    } = plotSpec;

    if (!data || data.length < 2) {
        return baseOption;
    }

    // For percentile data, the format is [times, percentile1Values, percentile2Values, ...]
    const timeData = data[0];

    // Create series for each percentile
    const series = [];

    // Determine percentiles based on the data structure
    // Assuming data format: [timestamps, p50values, p99values, ...]
    const percentileLabels = ['p50', 'p90', 'p99', 'p99.9', 'p99.99']; // Default labels, can be customized

    for (let i = 1; i < data.length; i++) {
        const percentileValues = data[i];
        const percentileData = [];

        // Create data points in the format [time, value, original_index]
        for (let j = 0; j < timeData.length; j++) {
            if (percentileValues[j] !== undefined && !isNaN(percentileValues[j])) {
                percentileData.push([timeData[j] * 1000, percentileValues[j], j]); // Store original index
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
    // const yAxisLabel = format.y_axis_label || format.axis_label;
    // const valueLabel = format.value_label;
    const logScale = format.log_scale;
    const minValue = format.min;
    const maxValue = format.max;

    // Detect if this is a scheduler or time-based chart by looking at title or unit
    const isSchedulerChart =
        (plotSpec.opts.title && (plotSpec.opts.title.includes('Latency') || plotSpec.opts.title.includes('Time'))) ||
        unitSystem === 'time';
    // TODO: remove the above second-guessing and just use the unit system.

    const yAxis = {
        type: logScale ? 'log' : 'value',
        logBase: 10,
        scale: true,
        min: minValue,
        max: maxValue,
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

    // Return scatter chart configuration with reliable time axis
    return {
        ...baseOption,
        yAxis,
        tooltip: {
            ...baseOption.tooltip,
            valueFormatter: unitSystem ?
                createAxisLabelFormatter(unitSystem) :
                undefined,
        },
        series: series
    };
}