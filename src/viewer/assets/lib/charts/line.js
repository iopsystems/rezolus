import {
    createAxisLabelFormatter,
} from './util/units.js';
import {
    getBaseOption,
    getBaseYAxisOption,
} from './base.js';

/**
 * Configures the Chart based on Chart.spec
 * Responsible for calling setOption on the echart instance, and for setting up any
 * chart-specific dynamic behavior.
 * @param {import('./chart.js').Chart} chart - the chart to configure
 */
export function configureLineChart(chart) {
    const {
        data,
        opts
    } = chart.spec;

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

    const option = {
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

    chart.echart.setOption(option);
}