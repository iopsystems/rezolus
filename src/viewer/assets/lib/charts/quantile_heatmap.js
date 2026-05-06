// quantile_heatmap.js — Heatmap of histogram quantiles vs time. Each
// (time × quantile) cell is a colored rect; color reflects the value.
// Color scale is log10 across the visible value range, RdYlGn flipped
// (low value → green, high value → red).

import { formatDateTime } from './util/utils.js';
import { createAxisLabelFormatter } from './util/units.js';
import {
    getBaseOption,
    applyNoData,
    getTooltipFreezeFooter,
    calculateMinZoomSpan,
    getDataZoomConfig,
    applyChartOption,
    TIME_AXIS_FORMATTER,
    CHART_GRID_TOP_WITH_LEGEND,
    HISTOGRAM_CHART_GRID_LEFT,
    COLORS,
    FONTS,
} from './base.js';
import { rdYlGnColor } from './util/colormap.js';
import {
    buildGradientCanvas,
    ensureLegendBar,
    sigDigits,
    LEGEND_GRID_RIGHT,
} from './color_legend.js';

// Series names from the spectrum fetch are already formatted as `pXX`
// (see fetchQuantileSpectrumForPlot). For specs that come from elsewhere
// (e.g. raw fractions like "0.5"), coerce to the same `pXX` shape so
// the y-axis labels stay uniform.
const formatQuantileLabel = (raw) => {
    if (typeof raw === 'string' && raw.startsWith('p')) return raw;
    const q = parseFloat(raw);
    if (!Number.isFinite(q)) return `p${raw}`;
    const pct = q * 100;
    const fixed = pct.toFixed(2).replace(/\.?0+$/, '');
    return `p${fixed}`;
};

/**
 * Configures the Chart for quantile heatmap visualization.
 * Input shape matches the percentile scatter chart: data[0]=times,
 * data[1..N]=per-quantile value series, with chart.spec.series_names
 * carrying the raw quantile fractions (e.g. "0.5", "0.99").
 *
 * @param {import('./chart.js').Chart} chart
 */
export function configureQuantileHeatmap(chart) {
    const { data, opts } = chart.spec;

    if (!data || data.length < 2 || !data[0] || data[0].length === 0) {
        applyNoData(chart);
        return;
    }

    const baseOption = getBaseOption();
    const format = opts.format || {};
    const unitSystem = format.unit_system;

    const timeData = data[0];
    const tCount = timeData.length;
    const seriesCount = data.length - 1;

    const rawLabels = (chart.spec.series_names && chart.spec.series_names.length === seriesCount)
        ? chart.spec.series_names
        : Array.from({ length: seriesCount }, (_, i) => String((i + 1) / seriesCount));
    const displayLabels = rawLabels.map(formatQuantileLabel);

    // Color scale spans the non-null, positive value range across all cells.
    let colorMin = Infinity;
    let colorMax = -Infinity;
    for (let s = 1; s <= seriesCount; s++) {
        const col = data[s];
        for (let t = 0; t < tCount; t++) {
            const v = col[t];
            if (v != null && !Number.isNaN(v) && v > 0) {
                if (v < colorMin) colorMin = v;
                if (v > colorMax) colorMax = v;
            }
        }
    }
    // Override colorMin / colorMax with anchors when the spec carries
    // them. min anchor pins the lower bound across spectrum kinds
    // (full vs tail) so toggling between them keeps colors consistent;
    // max anchor lets the compare-mode pair share a unified ceiling so
    // both halves render with the same color scale.
    const minAnchor = chart.spec.color_min_anchor;
    if (minAnchor != null && Number.isFinite(minAnchor) && minAnchor > 0) {
        colorMin = minAnchor;
    }
    const maxAnchor = chart.spec.color_max_anchor;
    if (maxAnchor != null && Number.isFinite(maxAnchor) && maxAnchor > 0) {
        colorMax = maxAnchor;
    }
    if (!Number.isFinite(colorMin)) colorMin = 0;
    if (!Number.isFinite(colorMax)) colorMax = 1;
    if (colorMax <= colorMin) colorMax = colorMin > 0 ? colorMin * 10 : 1;

    // Log-scale color mapping. Guard against non-positive min by
    // clamping inside the log to a tiny epsilon (the renderer already
    // skips zero/negative cells, so this only matters when colorMin
    // collapsed to its initialization value above).
    const safeLog = (v) => Math.log10(v > 0 ? v : 1e-12);
    const logMin = safeLog(colorMin);
    const logMax = safeLog(colorMax);
    const logRange = logMax - logMin || 1;

    const timeIntervalMs = tCount > 1
        ? (timeData[1] - timeData[0]) * 1000
        : 1000;

    // Cells: [timestampMs, quantileIdx, value]. Skip non-positive
    // values — they'd be invisible anyway and break the log mapping.
    const cells = [];
    for (let t = 0; t < tCount; t++) {
        const tsMs = timeData[t] * 1000;
        for (let q = 0; q < seriesCount; q++) {
            const v = data[q + 1][t];
            if (v == null || Number.isNaN(v) || v <= 0) continue;
            cells.push([tsMs, q, v]);
        }
    }

    const renderItem = function (params, api) {
        const tsMs = api.value(0);
        const q = api.value(1);
        const v = api.value(2);

        const coordSys = params.coordSys;
        const gridX = coordSys.x;
        const gridY = coordSys.y;
        const gridWidth = coordSys.width;
        const gridHeight = coordSys.height;

        const c0 = api.coord([tsMs, q - 0.5]);
        const c1 = api.coord([tsMs + timeIntervalMs, q + 0.5]);
        if (!c0 || !c1) return;

        const overlap = 1;
        let x = c0[0];
        let y = c1[1] - overlap;
        let width = c1[0] - c0[0] + overlap;
        let height = c0[1] - c1[1] + overlap * 2;

        if (width <= 0 || height <= 0) return;

        if (x < gridX) { width -= (gridX - x); x = gridX; }
        if (x + width > gridX + gridWidth) width = gridX + gridWidth - x;
        if (y < gridY) { height -= (gridY - y); y = gridY; }
        if (y + height > gridY + gridHeight) height = gridY + gridHeight - y;
        if (width <= 0 || height <= 0) return;

        const norm = Math.min(1, Math.max(0, (safeLog(v) - logMin) / logRange));
        // Flip: low value → green (t=1), high value → red (t=0).
        const color = rdYlGnColor(1 - norm);

        return {
            type: 'rect',
            shape: { x, y, width, height },
            style: { fill: color, stroke: null, lineWidth: 0 },
        };
    };

    const valueFmt = unitSystem
        ? createAxisLabelFormatter(unitSystem)
        : (v) => String(v);

    const customXFormatterForTooltip = chart.spec.xAxisFormatter;
    const tooltipFormatter = function (params) {
        if (!params.data) return '';
        const [tsMs, q, v] = params.data;
        const formattedTime = customXFormatterForTooltip
            ? customXFormatterForTooltip(tsMs)
            : formatDateTime(tsMs);
        const label = displayLabels[q] || '';
        return `<div style="${FONTS.cssSans}">
            <div style="${FONTS.cssMono} font-size: ${FONTS.tooltipTimestamp.fontSize}px; color: ${COLORS.fgSecondary}; margin-bottom: 8px;">
                ${formattedTime}
            </div>
            <div style="display: flex; align-items: center; gap: 12px;">
                <span style="background: ${COLORS.accentSubtle}; padding: 3px 8px; border-radius: 4px; ${FONTS.cssMono} font-size: ${FONTS.tooltipTimestamp.fontSize}px; color: ${COLORS.accent};">
                    ${label}
                </span>
                <span style="${FONTS.cssMono} font-weight: ${FONTS.tooltipValue.fontWeight}; font-size: ${FONTS.tooltipValue.fontSize}px; color: ${COLORS.fg};">
                    ${valueFmt(v)}
                </span>
            </div>
            ${getTooltipFreezeFooter(chart)}
        </div>`;
    };

    const xAxisFormatter = chart.spec.xAxisFormatter || TIME_AXIS_FORMATTER;

    // Show ~10 evenly spaced quantile labels. Walk backward from the
    // top quantile so the highest row (most semantically meaningful)
    // is always labeled — gives p10, p20, …, p100 for the full
    // spectrum and p99.1, p99.2, …, p100 for the tail spectrum.
    const labelStride = Math.max(1, Math.ceil(seriesCount / 10));
    const tickIndices = new Set();
    for (let i = seriesCount - 1; i >= 0; i -= labelStride) {
        tickIndices.add(i);
    }

    const option = {
        ...baseOption,
        grid: {
            // Same fixed left gutter as the scatter percentile chart so
            // toggling Full ↔ Tail ↔ scatter keeps the y-axis position
            // anchored. containLabel is off so this value is the
            // actual rendered gutter, regardless of label width.
            left: HISTOGRAM_CHART_GRID_LEFT,
            right: String(LEGEND_GRID_RIGHT),
            top: String(CHART_GRID_TOP_WITH_LEGEND),
            bottom: '24',
            containLabel: false,
        },
        dataZoom: getDataZoomConfig(calculateMinZoomSpan(timeData)),
        xAxis: {
            type: 'time',
            min: 'dataMin',
            max: 'dataMax',
            splitNumber: 5,
            axisLine: { show: false },
            axisTick: { show: false },
            axisLabel: {
                color: COLORS.fgSecondary,
                ...FONTS.axisLabel,
                formatter: xAxisFormatter,
            },
            splitLine: {
                show: true,
                lineStyle: { color: COLORS.gridLine, type: 'dashed' },
            },
        },
        yAxis: {
            type: 'value',
            min: -0.5,
            max: seriesCount - 0.5,
            // Force a tick at every integer so the formatter sees one
            // call per quantile row; the formatter then only labels the
            // p10/p20/…/p100 indices and returns '' for everything else.
            interval: 1,
            axisLine: { show: false },
            axisTick: { show: false },
            axisLabel: {
                color: COLORS.fgSecondary,
                ...FONTS.axisLabel,
                showMinLabel: false,
                showMaxLabel: false,
                formatter: (val) => {
                    const i = Math.round(val);
                    if (i < 0 || i >= seriesCount) return '';
                    return tickIndices.has(i) ? displayLabels[i] : '';
                },
            },
            splitLine: { show: false },
        },
        tooltip: {
            ...baseOption.tooltip,
            trigger: 'item',
            // Position above the cursor by default. When the cell is
            // close to the top of the canvas (no room for the tooltip
            // above), flip below so the hovered cell stays visible
            // instead of being covered by the tooltip.
            position: function (point, _params, _dom, _rect, size) {
                const [mouseX, mouseY] = point;
                const [tipW, tipH] = size.contentSize;
                const [viewW] = size.viewSize;
                const gap = 12;
                let x = mouseX - tipW / 2;
                if (x < 0) x = 0;
                if (x + tipW > viewW) x = viewW - tipW;
                let y = mouseY - tipH - gap;
                if (y < 0) y = mouseY + gap;
                return [x, y];
            },
            formatter: tooltipFormatter,
        },
        series: [{
            name: opts.title,
            type: 'custom',
            renderItem,
            encode: { x: 0, y: 1 },
            data: cells,
            clip: true,
            progressive: 5000,
            progressiveThreshold: 3000,
            animation: false,
        }],
    };

    applyChartOption(chart, option);

    // Vertical legend bar: top=red=high value, bottom=green=low value.
    // The (1 - t) flip matches the cell color mapping above.
    const barCanvas = buildGradientCanvas((t) => rdYlGnColor(1 - t));

    // Bar gradient is linear in log10(value) space. Interpolate values
    // at evenly spaced positions for the tick labels.
    const TICK_COUNT = 5;
    const ticks = [];
    for (let i = 0; i < TICK_COUNT; i++) {
        const pos = i / (TICK_COUNT - 1);
        const value = colorMin > 0 && colorMax > 0
            ? Math.pow(10, safeLog(colorMin) + pos * (safeLog(colorMax) - safeLog(colorMin)))
            : colorMin + pos * (colorMax - colorMin);
        ticks.push({ pos, label: valueFmt(sigDigits(value)) });
    }
    // Re-run on every `finished` so the bar's height and top track the
    // grid rect (Y-axis). First call before layout uses defaults.
    const renderLegend = () => {
        const rect = chart.echart?.getModel()?.getComponent('grid')?.coordinateSystem?.getRect();
        ensureLegendBar(chart.domNode, barCanvas, {
            ticks,
            barTop: rect?.y,
            barHeight: rect?.height,
        });
    };
    renderLegend();
    if (chart._legendFinishedFn) chart.echart.off('finished', chart._legendFinishedFn);
    chart._legendFinishedFn = renderLegend;
    chart.echart.on('finished', renderLegend);
}
