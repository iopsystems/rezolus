import {
    createAxisLabelFormatter,
} from './units.js';

/**
 * Creates a line chart configuration for ECharts
 * 
 * @param {Object} baseOption - Base chart options
 * @param {Object} plotSpec - Plot specification with data and options
 * @returns {Object} ECharts configuration object
 */
export function createLineChartOption(baseOption, plotSpec) {
    const {
        data,
        opts
    } = plotSpec;

    if (!data || data.length < 2) {
        return baseOption;
    }

    const [timeData, valueData] = data;

    const zippedData = timeData.map((t, i) => [t * 1000, valueData[i]]);

    // Access format properties using snake_case naming to match Rust serialization
    const format = opts.format || {};
    const unitSystem = format.unit_system;
    // const yAxisLabel = format.y_axis_label || format.axis_label;
    // const valueLabel = format.value_label;
    const logScale = format.log_scale;
    const minValue = format.min;
    const maxValue = format.max;

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

    const xAxis = {
        type: 'time',
        min: 'dataMin',
        max: 'dataMax',
        axisLine: {
            lineStyle: {
                color: '#ABABAB'
            }
        },
        axisLabel: {
            color: '#ABABAB',
            formatter: '{hh}:{mm}:{ss}',
        },
    };

    return {
        ...baseOption,
        xAxis,
        yAxis,
        tooltip: {
            ...baseOption.tooltip,
            valueFormatter: unitSystem ?
                createAxisLabelFormatter(unitSystem) :
                undefined,
        },
        series: [{
            data: zippedData,
            type: 'line',
            name: opts.title,
            showSymbol: false,
            emphasis: {
                focus: 'series'
            },
            lineStyle: {
                width: 2
            },
            animationDuration: 0, // Don't animate the line in
        }]
    };
}