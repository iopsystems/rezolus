// scatter.js - Scatter chart configuration with fixed time axis handling

import {
    createAxisLabelFormatter,
} from './util/units.js';
import {
    getBaseOption,
    getBaseYAxisOption,
    getTooltipFormatter,
    getNoDataOption,
    CHART_PALETTE,
    COLORS,
} from './base.js';

/**
 * Configures the Chart based on Chart.spec
 * Responsible for calling setOption on the echart instance, and for setting up any
 * chart-specific dynamic behavior.
 * @param {import('./chart.js').Chart} chart - the chart to configure
 */
export function configureScatterChart(chart) {
    const {
        data,
        opts
    } = chart.spec;

    if (!data || data.length < 2 || !data[0] || data[0].length === 0) {
        chart.echart.setOption(getNoDataOption(opts.title));
        return;
    }

    const baseOption = getBaseOption(opts.title);

    // Access format properties using snake_case naming to match Rust serialization
    const format = opts.format || {};
    const unitSystem = format.unit_system;
    const logScale = format.log_scale;
    const minValue = format.min;
    const maxValue = format.max;

    // For percentile data, the format is [times, percentile1Values, percentile2Values, ...]
    const timeData = data[0];

    // Create series for each percentile
    const series = [];

    const percentileLabels = format.percentile_labels || ['p50', 'p90', 'p99', 'p99.9', 'p99.99'];

    // Use curated palette for scatter points
    const scatterColors = [
        COLORS.accent,      // Electric blue
        COLORS.chartCyan,   // Bright cyan
        COLORS.chartTeal,   // Teal
        COLORS.chartGreen,  // Green
        COLORS.chartPurple, // Purple
    ];

    for (let i = 1; i < data.length; i++) {
        const percentileValues = data[i];
        const percentileData = [];

        // Create data points in the format [time, value, original_index]
        for (let j = 0; j < timeData.length; j++) {
            if (percentileValues[j] !== undefined && !isNaN(percentileValues[j])) {
                percentileData.push([timeData[j] * 1000, percentileValues[j], j]); // Store original index
            }
        }

        const color = scatterColors[(i - 1) % scatterColors.length];
        series.push({
            name: percentileLabels[i - 1] || `Percentile ${i}`,
            type: 'scatter',
            data: percentileData,
            symbolSize: 5,
            itemStyle: {
                color: color,
            },
            emphasis: {
                focus: 'series',
                itemStyle: {
                    shadowBlur: 12,
                    shadowColor: `${color}66`, // 40% opacity
                }
            }
        });
    }

    // Calculate minimum zoom span (5x sample interval as percentage of total duration)
    const sampleInterval = timeData.length > 1 ? (timeData[1] - timeData[0]) : 1;
    const totalDuration = timeData[timeData.length - 1] - timeData[0];
    const minZoomSpan = Math.max(0.1, (sampleInterval * 5 / totalDuration) * 100);

    // Return scatter chart configuration with reliable time axis
    const option = {
        ...baseOption,
        // Add dataZoom component with minSpan to enforce minimum zoom level
        dataZoom: [{
            type: 'inside',
            xAxisIndex: 0,
            minSpan: minZoomSpan,
            filterMode: 'none',
        }, {
            type: 'slider',
            show: false,
            xAxisIndex: 0,
            minSpan: minZoomSpan,
            filterMode: 'none',
        }],
        yAxis: getBaseYAxisOption(logScale, minValue, maxValue, unitSystem),
        tooltip: {
            ...baseOption.tooltip,
            formatter: getTooltipFormatter(unitSystem ?
                createAxisLabelFormatter(unitSystem) :
                val => val),
        },
        series: series,
        color: scatterColors,
    };

    // Use notMerge: true to clear any previous chart configuration (e.g., heatmap custom series)
    chart.echart.setOption(option, { notMerge: true });

    // Re-enable drag-to-zoom after clearing the chart
    chart.echart.dispatchAction({
        type: 'takeGlobalCursor',
        key: 'dataZoomSelect',
        dataZoomSelectActive: true,
    });
}
