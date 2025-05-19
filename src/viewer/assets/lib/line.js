import {
    createAxisLabelFormatter,
} from './units.js';
import {
    getBaseOption,
    getBaseYAxisOption,
} from './charts/base.js';

/**
 * Creates a line chart configuration for ECharts
 * 
 * @param {Object} baseOption - Base chart options
 * @param {Object} plotSpec - Plot specification with data and options
 * @returns {Object} ECharts configuration object
 */
export function createLineChartOption(plotSpec) {
    const {
        data,
        opts
    } = plotSpec;

    const baseOption = getBaseOption(opts.title);

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

    return {
        ...baseOption,
        yAxis: getBaseYAxisOption(logScale, minValue, maxValue, unitSystem),
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
            step: 'start',
            lineStyle: {
                width: 2
            },
            animationDuration: 0, // Don't animate the line in
        }]
    };
}