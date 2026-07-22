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
import { buildBoxplotSeries, buildEnvelopeLines, buildDivergenceBand } from './boxplot.js';
import { chartSwatches, renderSwatchRow, SWATCH_ROW_HEIGHT } from './swatches.js';
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
                // Optional rate() uncertainty band, parallel to valueData.
                // Absent (undefined) for non-rate queries → no band drawn.
                intervals: chart.spec.intervals,
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

    // Display mode: when the plot carries decimated boxplot columns, render
    // median line + inner/outer bands instead of a plain line. (Multi-series
    // 'multi'-style plots route to multi.js and render median lines only for
    // now; bands there are a follow-up.)
    const boxplotCols = Array.isArray(chart.spec.boxplot) && chart.spec.boxplot.length
        ? chart.spec.boxplot
        : null;
    const echartsSeries = boxplotCols
        ? boxplotCols.flatMap((s, i) => buildBoxplotSeries(s, {
            name: seriesList[i]?.name ?? (s.metric?.__name__ || `series ${i + 1}`),
            stackId: `bp${i}`,
            lineColor: seriesList[i]?.color || COLORS.accent,
            zBase: (boxplotCols.length - 1 - i) * 4,
        }))
        : seriesList.flatMap((s, idx) => {
        // Compare-mode entry carrying a decimated boxplot (median + min/max):
        // render an envelope of LINES (median + faint min/max), per capture
        // color, so two captures' spreads overlay without muddy filled bands.
        // Stack the first entry (baseline) on top for a consistent order, the
        // same index → z convention the percentile bands use.
        if (s.boxplot) {
            return buildEnvelopeLines(s.boxplot, {
                name: s.name,
                color: s.color,
                zBase: (seriesList.length - 1 - idx) * 4,
            });
        }

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
        // Prepend a translucent uncertainty band behind the line when the
        // series carries rate() acquisition-window bounds. Implemented as a
        // two-series stack: an invisible `lo` baseline plus a `hi-lo` delta
        // whose filled area spans lo→hi. z:1 keeps it under the line (z:2).
        return [...buildBandSeries(s, idx, range, chart.interval), base];
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
        // Compare mode: shade the gap between the two overlaid medians (the
        // divergence band) BEHIND the lines, so agreement reads as a thin line
        // and divergence as a widening ribbon. Prepended so its low z draws first.
        series: chart.spec.divergenceBand
            ? [...buildDivergenceBand(chart.spec.divergenceBand), ...echartsSeries]
            : echartsSeries,
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

    // Shade-meaning swatch row: inputs mirror the two band render paths above
    // exactly (boxplotCols XOR per-series intervals), so a swatch appears iff
    // the corresponding fill was actually drawn. The row needs reserved space
    // under the tick labels, hence the grid.bottom bump before setOption.
    const swatches = chartSwatches(boxplotCols
        ? { boxplot: boxplotCols }
        : { intervals: seriesList.map((s) => s.intervals) });
    if (swatches.length) {
        const grid = option.grid || baseOption.grid || {};
        option.grid = {
            ...grid,
            bottom: String(Number(grid.bottom ?? 24) + SWATCH_ROW_HEIGHT),
        };
    }

    applyChartOption(chart, option);
    renderSwatchRow(chart.domNode, swatches);

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

// Build the echarts series pair that renders a series' uncertainty band,
// or `[]` when the series has no usable `intervals`. `intervals` is an
// array parallel to `timeData`, each entry `[lo, hi]` (or null for a gap).
//
// echarts has no first-class "band" mark, so we stack two zero-line
// series sharing a per-series stack id: a transparent `lo` baseline and a
// `hi-lo` delta whose translucent areaStyle fills the lo→hi gap. Both are
// `silent` with hidden lines and suppressed tooltips so only the nominal
// line is interactive; z:1 keeps them behind it. Bounds are clamped to the
// same display range as the line so a band can't escape the axis.
export function buildBandSeries(s, idx, range, interval) {
    const intervals = s.intervals;
    if (!Array.isArray(intervals) || intervals.length !== s.timeData.length) {
        return [];
    }
    const loRaw = [];
    const deltaRaw = [];
    let anyBand = false;
    for (let i = 0; i < s.timeData.length; i++) {
        const t = s.timeData[i] * 1000;
        const iv = intervals[i];
        if (!Array.isArray(iv)) {
            loRaw.push([t, null]);
            deltaRaw.push([t, null]);
            continue;
        }
        const [lo] = clampToRange(iv[0], range);
        const [hi] = clampToRange(iv[1], range);
        loRaw.push([t, lo]);
        deltaRaw.push([t, hi - lo]);
        if (hi > lo) anyBand = true;
    }
    // Every bound collapsed (hi===lo) or empty — nothing visible to draw.
    if (!anyBand) return [];

    const stack = 'band-' + idx;
    const common = {
        type: 'line',
        name: s.name,
        stack,
        showSymbol: false,
        symbol: 'none',
        lineStyle: { opacity: 0 },
        silent: true,
        tooltip: { show: false },
        connectNulls: false,
        animationDuration: 0,
        z: 1,
    };
    return [
        { ...common, data: insertGapNulls(loRaw, interval), areaStyle: { opacity: 0 } },
        {
            ...common,
            data: insertGapNulls(deltaRaw, interval),
            areaStyle: { color: s.color, opacity: 0.18 },
        },
    ];
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

// Sit below the title, left edge aligned with the echarts plot grid
// (i.e. just past the y-axis gutter).
function positionTotalToggle(chart, container) {
    if (!chart.echart) return;
    try {
        const rect = chart.echart.getModel()
            .getComponent('grid')?.coordinateSystem?.getRect();
        if (rect && Number.isFinite(rect.x)) {
            container.style.left = Math.round(rect.x) + 'px';
        }
    } catch (_e) {
        // echarts grid not ready yet; next 'finished' event will retry.
    }
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
        el.style.cssText = `
            position: absolute;
            top: 42px;
            left: 0px;
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

    // Reposition on each echarts layout (initial, theme swap, resize,
    // zoom). Replace any previously bound listener so we don't stack
    // handlers across reconfigures.
    if (chart.echart) {
        if (chart._totalToggleFinishedFn) {
            chart.echart.off('finished', chart._totalToggleFinishedFn);
        }
        chart._totalToggleFinishedFn = () => positionTotalToggle(chart, el);
        chart.echart.on('finished', chart._totalToggleFinishedFn);
        positionTotalToggle(chart, el);
    }
}
