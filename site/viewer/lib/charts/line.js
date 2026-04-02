import {
    createAxisLabelFormatter,
} from './util/units.js';
import {
    insertGapNulls,
} from './util/utils.js';
import {
    getBaseOption,
    getBaseYAxisOption,
    getTooltipFormatter,
    applyNoData,
    calculateMinZoomSpan,
    getDataZoomConfig,
    applyChartOption,
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
        applyNoData(chart);
        return;
    }

    const baseOption = getBaseOption();

    const [timeData, valueData] = data;

    let zippedData = timeData.map((t, i) => [t * 1000, valueData[i]]);
    zippedData = insertGapNulls(zippedData, chart.interval);

    // Access format properties using snake_case naming to match Rust serialization
    const format = opts.format || {};
    const unitSystem = format.unit_system;
    const logScale = format.log_scale;
    const minValue = format.min;
    const maxValue = format.max;

    const option = {
        ...baseOption,
        dataZoom: getDataZoomConfig(calculateMinZoomSpan(timeData)),
        yAxis: getBaseYAxisOption(logScale, minValue, maxValue, unitSystem),
        tooltip: {
            ...baseOption.tooltip,
            formatter: getTooltipFormatter(unitSystem ?
                createAxisLabelFormatter(unitSystem) :
                val => val, null, chart),
        },
        series: [{
            data: zippedData,
            type: 'line',
            name: opts.title,
            showSymbol: false,
            sampling: 'lttb',
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
                        { offset: 0, color: COLORS.accentAreaTop },
                        { offset: 0.5, color: COLORS.accentAreaMid },
                        { offset: 1, color: COLORS.accentAreaBottom },
                    ],
                },
            },
            animationDuration: 0, // Don't animate the line in
        }]
    };

    applyChartOption(chart, option);
}
