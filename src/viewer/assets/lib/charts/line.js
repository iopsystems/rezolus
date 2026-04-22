import {
    createAxisLabelFormatter,
} from './util/units.js';
import {
    insertGapNulls,
    clampToRange,
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
        multiSeries,
        opts
    } = chart.spec;

    // Normalize to a list of {name, color, timeData, valueData, fill}.
    // Single-series callers pass `data: [timeData, valueData]` (unchanged).
    // Compare-mode callers pass `multiSeries: [{name,color,timeData,valueData}, ...]`.
    const seriesList = (Array.isArray(multiSeries) && multiSeries.length > 0)
        ? multiSeries
        : (data && data.length >= 2 && data[0] && data[1] && data[0].length > 0
            ? [{
                name: opts.title,
                color: COLORS.accent,
                timeData: data[0],
                valueData: data[1],
                fill: true,
            }]
            : []);

    if (seriesList.length === 0) {
        applyNoData(chart);
        return;
    }

    const baseOption = getBaseOption();

    // Access format properties using snake_case naming to match Rust serialization
    const format = opts.format || {};
    const unitSystem = format.unit_system;
    const logScale = format.log_scale;
    const range = format.range;

    // Pick the widest timeData across all series for zoom-span + formatter purposes.
    const widestTimeData = seriesList.reduce(
        (a, s) => (s.timeData.length > a.length ? s.timeData : a),
        seriesList[0].timeData,
    );

    const echartsSeries = seriesList.map((s) => {
        let zipped = s.timeData.map((t, i) => {
            const [v, raw] = clampToRange(s.valueData[i], range);
            return [t * 1000, v, raw];
        });
        zipped = insertGapNulls(zipped, chart.interval);

        const base = {
            data: zipped,
            type: 'line',
            name: s.name,
            showSymbol: false,
            sampling: 'lttb',
            emphasis: { focus: 'series' },
            step: 'start',
            lineStyle: { width: 1.5, color: s.color },
            itemStyle: { color: s.color },
            connectNulls: false,
            animationDuration: 0,
        };
        if (s.fill) {
            base.areaStyle = {
                color: {
                    type: 'linear',
                    x: 0, y: 0, x2: 0, y2: 1,
                    colorStops: [
                        { offset: 0, color: COLORS.accentAreaTop },
                        { offset: 0.5, color: COLORS.accentAreaMid },
                        { offset: 1, color: COLORS.accentAreaBottom },
                    ],
                },
            };
        }
        return base;
    });

    // Compare-mode line overlays want relative-time labels (+Xs) on
    // the x-axis. Honor `spec.xAxisFormatter` if set; otherwise use
    // the base time formatter.
    const customXFormatter = chart.spec.xAxisFormatter;
    const xAxisOverride = customXFormatter
        ? {
            ...baseOption.xAxis,
            axisLabel: {
                ...(baseOption.xAxis.axisLabel || {}),
                formatter: customXFormatter,
            },
        }
        : null;

    // TODO(compare): when `xAxisFormatter` is set we should prepend the
    // relative offset to the tooltip timestamp too. Today the tooltip
    // still formats `paramsArray[0].value[0]` as an absolute clock;
    // changing that means threading the formatter through
    // `getTooltipFormatter` in `base.js`, which is a wider refactor.
    // Axis labels carry the relative time, which is the must-have.
    const option = {
        ...baseOption,
        ...(xAxisOverride ? { xAxis: xAxisOverride } : {}),
        dataZoom: getDataZoomConfig(calculateMinZoomSpan(widestTimeData)),
        yAxis: getBaseYAxisOption(logScale, unitSystem),
        tooltip: {
            ...baseOption.tooltip,
            formatter: getTooltipFormatter(unitSystem ?
                createAxisLabelFormatter(unitSystem) :
                val => val, null, chart),
        },
        series: echartsSeries,
    };

    // Multi-series charts want a legend so traces are distinguishable.
    if (seriesList.length > 1) {
        option.legend = {
            ...(baseOption.legend || {}),
            data: seriesList.map((s) => s.name),
            show: true,
        };
    }

    applyChartOption(chart, option);
}
