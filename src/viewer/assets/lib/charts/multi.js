// multi.js - Multi-series chart configuration with deterministic cgroup colors

import {
    createAxisLabelFormatter,
} from './util/units.js';
import {
    getBaseOption,
    getBaseYAxisOption,
    getTooltipFormatter,
    getNoDataOption,
} from './base.js';
import globalColorMapper from './util/colormap.js';

/**
 * Configures the Chart based on Chart.spec
 * Responsible for calling setOption on the echart instance, and for setting up any
 * chart-specific dynamic behavior.
 * @param {import('./chart.js').Chart} chart - the chart to configure
 */
export function configureMultiSeriesChart(chart) {
    const {
        data,
        opts,
    } = chart.spec;

    if (!data || data.length < 2 || !data[0] || data[0].length === 0) {
        chart.echart.setOption(getNoDataOption(opts.title));
        return;
    }

    const baseOption = getBaseOption(opts.title);

    // For multi-series charts, the first row contains timestamps, subsequent rows are series data
    const timeData = data[0];
    const lineCount = data.length - 1;

    let seriesNames = chart.spec.series_names;
    if (!seriesNames || seriesNames.length !== lineCount) {
        console.log("series_names is missing or wrong length", seriesNames);
        seriesNames = Array.from(Array(lineCount).keys()).map(i => `Series ${i + 1}`);
    }

    // Access format properties using snake_case naming to match Rust serialization
    const format = opts.format || {};
    const unitSystem = format.unit_system;
    // const yAxisLabel = format.y_axis_label || format.axis_label;
    // const valueLabel = format.value_label;
    const logScale = format.log_scale;
    const minValue = format.min;
    const maxValue = format.max;

    // Create series configurations for each data series
    const series = [];

    // Get colors only for selected cgroups (not for aggregate "Other")
    const cgroupColors = seriesNames.map((name) => {
        const color = globalColorMapper.getSelectedCgroupColor(name);
        // If no color assigned (shouldn't happen for selected cgroups), fall back to index-based color
        return (
            color ||
            globalColorMapper.getColorByIndex(seriesNames.indexOf(name))
        );
    });

    for (let i = 1; i < data.length; i++) {
        const name = seriesNames[i - 1];
        const isOtherCategory = name === "Other";
        const color = cgroupColors[i - 1];

        const zippedData = timeData.map((t, j) => [t * 1000, data[i][j]]);

        series.push({
            name: name,
            type: 'line',
            data: zippedData,
            itemStyle: {
                color,
            },
            step: 'start',
            lineStyle: {
                color,
                width: 2,
            },
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

    // Ensure "Other" category is the last in the series array so it appears in the legend last.
    const otherIndex = series.findIndex(s => s.name === "Other");
    if (otherIndex !== -1 && otherIndex !== series.length - 1) {
        const otherSeries = series.splice(otherIndex, 1)[0];
        series.push(otherSeries);
    }

    const option = {
        ...baseOption,
        yAxis: getBaseYAxisOption(logScale, minValue, maxValue, unitSystem),
        tooltip: {
            ...baseOption.tooltip,
            formatter: getTooltipFormatter(unitSystem ?
                createAxisLabelFormatter(unitSystem) :
                val => val),
        },
        series: series,
        // Don't use the default color palette for normal cgroups
        color: cgroupColors,
    };

    chart.echart.setOption(option);
}