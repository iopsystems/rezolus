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
    buildOverlayLegendOption,
    overrideXAxisFormatter,
    CHART_GRID_TOP_WITH_LEGEND,
    COLORS,
} from './base.js';
import { FONTS } from './util/fonts.js';
import { executePromQLRangeQuery, applyResultToPlot } from '../data.js';

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

    // Tooltip series row reflects current toggle state; card header keeps the full suffix.
    const seriesTitle = chart.spec.promql_query_total
        ? opts.title.replace(/Mean\/Total\b/, chart._showTotal ? 'Total' : 'Mean')
        : opts.title;

    // Normalize to a list of {name, color, timeData, valueData, fill}.
    // Single-series callers pass `data: [timeData, valueData]` (unchanged).
    // Compare-mode callers pass `multiSeries: [{name,color,timeData,valueData}, ...]`.
    const seriesList = (Array.isArray(multiSeries) && multiSeries.length > 0)
        ? multiSeries
        : (data && data.length >= 2 && data[0] && data[1] && data[0].length > 0
            ? [{
                name: seriesTitle,
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

    const echartsSeries = seriesList.flatMap((s) => {
        const zippedRaw = s.timeData.map((t, i) => {
            const [v, raw] = clampToRange(s.valueData[i], range);
            return [t * 1000, v, raw];
        });

        // Scatter-mode series: faded connecting line underneath + crisp
        // dots on top, matching single-capture scatter.js's treatment of
        // percentile series. The line is a visual guide (opacity 0.4,
        // tooltip suppressed) so the eye follows the trend; hovering
        // triggers only the scatter series.
        if (s.scatter) {
            const guideLine = insertGapNulls(zippedRaw, chart.interval);
            return [
                {
                    name: s.name,
                    type: 'line',
                    data: guideLine,
                    showSymbol: false,
                    lineStyle: { color: s.color, width: 1.5, opacity: 0.4 },
                    itemStyle: { color: s.color },
                    tooltip: { show: false },
                    connectNulls: false,
                    animationDuration: 0,
                },
                {
                    name: s.name,
                    type: 'scatter',
                    data: zippedRaw,
                    symbol: 'circle',
                    symbolSize: 3,
                    itemStyle: { color: s.color },
                    emphasis: { focus: 'series' },
                    animationDuration: 0,
                },
            ];
        }

        const zipped = insertGapNulls(zippedRaw, chart.interval);
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
        return [base];
    });

    // Compare-mode line overlays want relative-time labels (+Xs) on
    // the x-axis. Honor `spec.xAxisFormatter` if set; otherwise use
    // the base time formatter.
    const xAxisOverride = overrideXAxisFormatter(baseOption.xAxis, chart.spec.xAxisFormatter);

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

    // Multi-series charts get the same legend treatment scatter uses
    // (right-aligned circle swatches, padded names, grid pushed down).
    if (seriesList.length > 1) {
        option.legend = buildOverlayLegendOption(seriesList.map((s) => s.name));
        // Preserve user's show/hide toggles across re-renders.
        // applyChartOption uses setOption(..., {notMerge: true}), which
        // otherwise wipes echarts' internal legend.selected state on
        // every redraw — the net effect was that toggling a series off
        // appeared to hide both series until the user interacted again.
        if (chart._legendSelected) {
            option.legend.selected = { ...chart._legendSelected };
        }
        // Push the plot grid down so the legend has room above it,
        // matching scatter's layout.
        option.grid = { ...(baseOption.grid || {}), top: String(CHART_GRID_TOP_WITH_LEGEND) };
    }

    applyChartOption(chart, option);

    // Track future legend toggles so subsequent re-renders preserve them.
    if (seriesList.length > 1) {
        chart.echart.off('legendselectchanged', chart._legendSelectHandler);
        chart._legendSelectHandler = (params) => {
            chart._legendSelected = { ...params.selected };
        };
        chart.echart.on('legendselectchanged', chart._legendSelectHandler);
    }

    ensureTotalToggle(chart);
}

// Mean/Total toggle. State on the chart instance (no persistence);
// pattern mirrors scatter.js's spectrum-controls.

const TOTAL_TOGGLE_CLASS = 'total-toggle';

function renderTotalCheckbox(el, on, pending) {
    const glyph = on ? '☑' : '☐';
    const label = pending ? 'Total…' : 'Total';
    el.innerHTML =
        `<span style="font-size: 16px; line-height: 1; transform: translateY(-2px);">${glyph}</span>`
        + `<span>${label}</span>`;
    el.style.color = on ? COLORS.fg : COLORS.fgSecondary;
}

function refreshTotalCheckbox(chart) {
    const el = chart.domNode?.querySelector('.' + TOTAL_TOGGLE_CLASS);
    if (el) renderTotalCheckbox(el, !!chart._showTotal, !!chart._totalPending);
}

async function toggleTotal(chart) {
    if (chart._totalPending) return;
    const totalQuery = chart.spec.promql_query_total;
    if (!totalQuery) return;

    if (chart._showTotal) {
        if (chart._meanDataCache) chart.spec.data = chart._meanDataCache;
        chart._showTotal = false;
        refreshTotalCheckbox(chart);
        chart.configureChartByType();
        return;
    }

    chart._meanDataCache = chart.spec.data;
    if (chart._totalDataCache) {
        chart.spec.data = chart._totalDataCache;
        chart._showTotal = true;
        refreshTotalCheckbox(chart);
        chart.configureChartByType();
        return;
    }

    chart._totalPending = true;
    refreshTotalCheckbox(chart);
    try {
        const result = await executePromQLRangeQuery(totalQuery);
        chart._totalPending = false;
        const ok = result?.status === 'success'
            && (result.data?.result?.length ?? 0) > 0;
        if (!ok) {
            chart._meanDataCache = null;
            refreshTotalCheckbox(chart);
            return;
        }
        applyResultToPlot(chart.spec, result);
        chart._totalDataCache = chart.spec.data;
        chart._showTotal = true;
    } catch (e) {
        console.warn('[total-toggle] fetch failed:', e);
        chart._totalPending = false;
        chart.spec.data = chart._meanDataCache;
        chart._meanDataCache = null;
    }
    refreshTotalCheckbox(chart);
    chart.configureChartByType();
}

function ensureTotalToggle(chart) {
    if (!chart.spec.promql_query_total) {
        chart.domNode?.querySelector('.' + TOTAL_TOGGLE_CLASS)?.remove();
        return;
    }
    if (!chart.domNode) return;
    let el = chart.domNode.querySelector('.' + TOTAL_TOGGLE_CLASS);
    if (!el) {
        el = document.createElement('span');
        el.className = TOTAL_TOGGLE_CLASS;
        // right:60 clears the expand (right:8) + select-pin (right:32) icons.
        el.style.cssText = `
            position: absolute;
            top: 8px;
            right: 60px;
            z-index: 10;
            display: inline-flex;
            align-items: center;
            gap: 4px;
            cursor: pointer;
            ${FONTS.cssControl}
            user-select: none;
        `;
        chart.domNode.appendChild(el);
    }
    el.onclick = () => toggleTotal(chart);
    renderTotalCheckbox(el, !!chart._showTotal, !!chart._totalPending);
}
