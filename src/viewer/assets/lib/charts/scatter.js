// scatter.js - Scatter chart configuration with fixed time axis handling

import {
    createAxisLabelFormatter,
} from './util/units.js';
import {
    getBaseOption,
    getBaseYAxisOption,
    getTooltipFormatter,
    getNoDataOption,
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
    // const yAxisLabel = format.y_axis_label || format.axis_label;
    // const valueLabel = format.value_label;
    const logScale = format.log_scale;
    const minValue = format.min;
    const maxValue = format.max;

    // For percentile data, the format is [times, percentile1Values, percentile2Values, ...]
    const timeData = data[0];

    // Create series for each percentile
    const series = [];

    const percentileLabels = format.percentile_labels || ['p50', 'p90', 'p99', 'p99.9', 'p99.99'];

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

    // Detect if this is a scheduler or time-based chart by looking at title or unit
    const isSchedulerChart =
        (chart.spec.opts.title && (chart.spec.opts.title.includes('Latency') || chart.spec.opts.title.includes('Time'))) ||
        unitSystem === 'time';
    // TODO: remove the above second-guessing and just use the unit system.

    // Return scatter chart configuration with reliable time axis
    const option = {
        ...baseOption,
        yAxis: getBaseYAxisOption(logScale, minValue, maxValue, unitSystem),
        tooltip: {
            ...baseOption.tooltip,
            formatter: getTooltipFormatter(unitSystem ?
                createAxisLabelFormatter(unitSystem) :
                val => val),
        },
        series: series
    };

    chart.echart.setOption(option);
}