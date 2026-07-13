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
    applyChartOption,
    CHART_GRID_TOP_WITH_LEGEND,
    COLORS,
    FONTS,
} from './base.js';
import globalColorMapper from './util/colormap.js';
import { buildBoxplotSeries } from './boxplot.js';

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
        applyNoData(chart);
        return;
    }

    const baseOption = getBaseOption();

    // First row is timestamps, subsequent rows are series data.
    const timeData = data[0];
    const lineCount = data.length - 1;

    let seriesNames = chart.spec.series_names;
    if (!seriesNames || seriesNames.length !== lineCount) {
        console.warn("series_names is missing or wrong length", seriesNames);
        seriesNames = Array.from(Array(lineCount).keys()).map(i => `Series ${i + 1}`);
    }

    // Access format properties using snake_case naming to match Rust serialization
    const format = opts.format || {};
    const unitSystem = format.unit_system;
    const logScale = format.log_scale;
    const range = format.range;

    const series = [];

    const cgroupColors = seriesNames.map(name => globalColorMapper.getColorByName(name));

    // For percentile charts, assign z-index so lower quantiles draw on top of higher ones.
    // This ensures p50 is visible when its value equals p99.99.
    const isPercentileChart = chart.spec.promql_query &&
        chart.spec.promql_query.includes('histogram_quantiles');

    for (let i = 1; i < data.length; i++) {
        const name = seriesNames[i - 1];
        const isOtherCategory = name === "Other";
        const color = cgroupColors[i - 1];

        let zippedData = timeData.map((t, j) => {
            const [v, raw] = clampToRange(data[i][j], range);
            return [t * 1000, v, raw];
        });
        zippedData = insertGapNulls(zippedData, chart.interval);

        // z controls draw order: higher z draws on top.
        // Percentile charts: reverse so lower quantiles (earlier in array) draw on top.
        // Other charts: "Other" category behind everything else.
        let zLevel;
        if (isPercentileChart) {
            zLevel = data.length - i;
        } else {
            zLevel = isOtherCategory ? 1 : 2;
        }

        series.push({
            name: name,
            type: 'line',
            data: zippedData,
            sampling: 'lttb',
            itemStyle: {
                color,
            },
            step: 'start',
            lineStyle: {
                color,
                width: isOtherCategory ? 1 : 1.5,
                opacity: isOtherCategory ? 0.6 : 1,
            },
            // Add symbol for "Other" category to make it more distinguishable
            showSymbol: isOtherCategory,
            symbolSize: isOtherCategory ? 3 : 0,
            z: zLevel,
            emphasis: {
                focus: 'series',
                lineStyle: {
                    width: 2,
                }
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

    // Display mode: draw the outer min/max envelope band behind each series so
    // decimated spikes aren't smoothed away by the median line (multi charts draw
    // their own lines above, so noMedian). Reuses the boxplot columns already
    // fetched (chart.spec.boxplot), clamped to range like the lines. z=0 keeps
    // the bands behind every line (which are z≥1). The band fills carry no name,
    // so they stay out of the legend.
    const boxcols = (Array.isArray(chart.spec.boxplot) && chart.spec.boxplot.length === lineCount)
        ? chart.spec.boxplot
        : null;
    if (boxcols && chart.spec.boxplotDecimated) {
        const clampCol = (arr) => {
            if (!range || (range.max == null && range.min == null)) return arr;
            const out = new Float64Array(arr.length);
            for (let i = 0; i < arr.length; i++) {
                let v = arr[i];
                if (range.max != null && v > range.max) v = range.max;
                else if (range.min != null && v < range.min) v = range.min;
                out[i] = v;
            }
            return out;
        };
        for (let k = 0; k < lineCount; k++) {
            const s = boxcols[k];
            if (!s || !s.t || s.t.length === 0) continue;
            const band = buildBoxplotSeries(
                { ...s, min: clampCol(s.min), max: clampCol(s.max) },
                {
                    stackId: `mb${k}`,
                    lineColor: cgroupColors[k],
                    outerOnly: true,
                    noMedian: true,
                },
            );
            for (const bs of band) bs.z = 0;
            series.push(...band);
        }
    }

    const option = {
        ...baseOption,
        grid: { ...baseOption.grid, top: String(CHART_GRID_TOP_WITH_LEGEND) },
        legend: {
            show: true,
            top: '42',
            right: '16',
            icon: 'roundRect',
            itemWidth: 10,
            itemHeight: 10,
            itemGap: 12,
            // Only the named line series belong in the legend — the band fills
            // added below carry no name.
            data: series.map(s => s.name).filter(Boolean),
            textStyle: {
                color: COLORS.fgSecondary,
                ...FONTS.legend,
            },
        },
        yAxis: getBaseYAxisOption(logScale, unitSystem),
        tooltip: {
            ...baseOption.tooltip,
            formatter: getTooltipFormatter(unitSystem ?
                createAxisLabelFormatter(unitSystem) :
                val => val, null, chart),
        },
        series: series,
        color: cgroupColors,
    };

    applyChartOption(chart, option);
}
