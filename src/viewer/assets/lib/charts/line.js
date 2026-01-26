import {
    createAxisLabelFormatter,
} from './util/units.js';
import {
    getBaseOption,
    getBaseYAxisOption,
    getTooltipFormatter,
    getNoDataOption,
    COLORS,
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

    if (
        !data ||
        data.length < 2 ||
        !data[0] ||
        !data[1] ||
        data[0].length === 0
    ) {
        chart.echart.setOption(getNoDataOption(opts.title));
        return;
    }

    const baseOption = getBaseOption(opts.title, (val) => val);

    const [timeData, valueData] = data;

    const zippedData = timeData.map((t, i) => [t * 1000, valueData[i]]);

    // Access format properties using snake_case naming to match Rust serialization
    const format = opts.format || {};
    const unitSystem = format.unit_system;
    const logScale = format.log_scale;
    const minValue = format.min;
    const maxValue = format.max;

    // Calculate minimum zoom span (5x sample interval as percentage of total duration)
    const sampleInterval = timeData.length > 1 ? (timeData[1] - timeData[0]) : 1;
    const totalDuration = timeData[timeData.length - 1] - timeData[0];
    const minZoomSpan = Math.max(0.1, (sampleInterval * 5 / totalDuration) * 100);

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
                width: 1.5,
                color: COLORS.accent,
            },
            itemStyle: {
                color: COLORS.accent,
            },
            areaStyle: {
                color: {
                    type: 'linear',
                    x: 0,
                    y: 0,
                    x2: 0,
                    y2: 1,
                    colorStops: [
                        { offset: 0, color: 'rgba(88, 166, 255, 0.2)' },
                        { offset: 0.5, color: 'rgba(88, 166, 255, 0.08)' },
                        { offset: 1, color: 'rgba(88, 166, 255, 0.01)' },
                    ],
                },
            },
            animationDuration: 0, // Don't animate the line in
        }]
    };

    // Use notMerge: true to clear any previous chart configuration
    chart.echart.setOption(option, { notMerge: true });

    // Re-enable drag-to-zoom after clearing the chart
    chart.echart.dispatchAction({
        type: 'takeGlobalCursor',
        key: 'dataZoomSelect',
        dataZoomSelectActive: true,
    });
}
