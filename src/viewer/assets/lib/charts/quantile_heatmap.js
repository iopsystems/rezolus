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

// Build a (t: 0..1) → cssColor function from a palette array (CSS color
// strings). Used when the spec supplies a custom `colormap` (e.g. the
// diverging palette for compare-mode diff heatmaps). Nearest-neighbor
// interpolation between adjacent stops — adequate for the cell colors
// (the gradient legend uses the same function via buildGradientCanvas).
const buildRampColorFn = (palette) => (t) => {
    if (!palette || palette.length === 0) return 'rgb(0,0,0)';
    if (palette.length === 1) return palette[0];
    const clamped = Math.max(0, Math.min(1, t));
    const idx = clamped * (palette.length - 1);
    const i = Math.floor(idx);
    if (i >= palette.length - 1) return palette[palette.length - 1];
    const f = idx - i;
    return f < 0.5 ? palette[i] : palette[i + 1];
};

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

    // Color function. Default: flipped-RdYlGn so low values are green
    // and high values are red. When the spec supplies a custom palette
    // (e.g. compare-mode diff diverging palette), use that ramp directly
    // — palette index 0 maps to t=0 (the colorMin end), so the strategy
    // is responsible for ordering the palette accordingly.
    const customPalette = chart.spec.colormap;
    const rampFn = customPalette ? buildRampColorFn(customPalette) : null;
    const cellColorFor = (norm) => rampFn
        ? rampFn(norm)
        : rdYlGnColor(1 - norm);  // flipped: low value → green

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

    // Cells: [timestampMs, quantileIdx, value]. When `nullCellColor`
    // is set, emit null-valued cells too so they render with the
    // configured color (compare-mode diff heatmap uses this). Otherwise
    // skip them along with non-positive cells (which break log mapping).
    const nullCellColor = chart.spec.nullCellColor || null;
    const cells = [];
    for (let t = 0; t < tCount; t++) {
        const tsMs = timeData[t] * 1000;
        for (let q = 0; q < seriesCount; q++) {
            const v = data[q + 1][t];
            const isNull = v == null || Number.isNaN(v);
            if (isNull) {
                if (nullCellColor) cells.push([tsMs, q, null]);
                continue;
            }
            if (v <= 0) continue;
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

        // Null cells (compare-mode diff): paint with the configured
        // null color and exit early — no log-scale mapping needed.
        if (v === null || v === undefined || Number.isNaN(v)) {
            return {
                type: 'rect',
                shape: { x, y, width, height },
                style: { fill: nullCellColor, stroke: null, lineWidth: 0 },
            };
        }

        const norm = Math.min(1, Math.max(0, (safeLog(v) - logMin) / logRange));
        const color = cellColorFor(norm);

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

    // Diff side-channel: when present, the tooltip pulls the original
    // baseline + experiment values from these matrices and renders both
    // alongside the delta (matrices are keyed [qIdx][tIdx]).
    const diffMatrices = chart.spec.diffMatrices;
    const diffCaptureLabels = chart.spec.diffCaptureLabels;

    const tooltipFormatter = function (params) {
        if (!params.data) return '';
        const [tsMs, q, v] = params.data;
        const formattedTime = customXFormatterForTooltip
            ? customXFormatterForTooltip(tsMs)
            : formatDateTime(tsMs);
        const label = displayLabels[q] || '';

        // Diff variant: show baseline | experiment | delta.
        if (diffMatrices) {
            // Find the time index by scanning timeData for the matching ms.
            // O(N) but tooltips fire on hover — fine at 100s of timestamps.
            let tIdx = -1;
            for (let i = 0; i < tCount; i++) {
                if (Math.abs(timeData[i] * 1000 - tsMs) < 0.5) { tIdx = i; break; }
            }
            const bv = (tIdx >= 0) ? diffMatrices.baseline?.[q]?.[tIdx] : null;
            const ev = (tIdx >= 0) ? diffMatrices.experiment?.[q]?.[tIdx] : null;
            const fmtCell = (x) => (x == null || Number.isNaN(x)) ? '—' : valueFmt(x);
            const baselineLabel = diffCaptureLabels?.baseline || 'baseline';
            const experimentLabel = diffCaptureLabels?.experiment || 'experiment';
            const deltaStr = (v == null || Number.isNaN(v))
                ? '—'
                : `${v >= 0 ? '+' : ''}${valueFmt(v)}`;
            return `<div style="${FONTS.cssSans}">
                <div style="${FONTS.cssMono} font-size: ${FONTS.tooltipTimestamp.fontSize}px; color: ${COLORS.fgSecondary}; margin-bottom: 8px;">
                    ${formattedTime}
                </div>
                <div style="display: flex; align-items: center; gap: 12px; margin-bottom: 6px;">
                    <span style="background: ${COLORS.accentSubtle}; padding: 3px 8px; border-radius: 4px; ${FONTS.cssMono} font-size: ${FONTS.tooltipTimestamp.fontSize}px; color: ${COLORS.accent};">
                        ${label}
                    </span>
                </div>
                <div style="display: grid; grid-template-columns: max-content max-content; gap: 2px 12px; ${FONTS.cssMono} font-size: ${FONTS.tooltipValue.fontSize}px;">
                    <span style="color: var(--compare-baseline, ${COLORS.fgSecondary});">${baselineLabel}</span>
                    <span style="color: ${COLORS.fg}; font-weight: ${FONTS.tooltipValue.fontWeight};">${fmtCell(bv)}</span>
                    <span style="color: var(--compare-experiment, ${COLORS.fgSecondary});">${experimentLabel}</span>
                    <span style="color: ${COLORS.fg}; font-weight: ${FONTS.tooltipValue.fontWeight};">${fmtCell(ev)}</span>
                    <span style="color: ${COLORS.fgSecondary};">Δ</span>
                    <span style="color: ${COLORS.fg}; font-weight: ${FONTS.tooltipValue.fontWeight};">${deltaStr}</span>
                </div>
                ${getTooltipFreezeFooter(chart)}
            </div>`;
        }

        // Compare-pair variant: when the spec carries the counterpart's
        // spectrum data, render baseline | experiment | delta. This is
        // the side-by-side path's tooltip (the diff path used the
        // diffMatrices branch above).
        if (chart.spec.compareCounterpartData
            && chart.spec.compareCaptureLabels
            && chart.spec.compareSelfRole) {
            const counterpart = chart.spec.compareCounterpartData;
            const labels = chart.spec.compareCaptureLabels;
            const selfRole = chart.spec.compareSelfRole;  // 'baseline' | 'experiment'
            // Find counterpart cell value at the same (timeIdx, qIdx).
            let tIdx = -1;
            for (let i = 0; i < tCount; i++) {
                if (Math.abs(timeData[i] * 1000 - tsMs) < 0.5) { tIdx = i; break; }
            }
            const otherV = (tIdx >= 0 && Array.isArray(counterpart.data) && counterpart.data[q + 1])
                ? counterpart.data[q + 1][tIdx]
                : null;
            const fmtCell = (x) => (x == null || Number.isNaN(x)) ? '—' : valueFmt(x);
            const selfV = v;
            const baseV = selfRole === 'baseline' ? selfV : otherV;
            const expV  = selfRole === 'experiment' ? selfV : otherV;
            let deltaStr = '—';
            if (baseV != null && !Number.isNaN(baseV) && expV != null && !Number.isNaN(expV)) {
                const d = expV - baseV;
                const pct = baseV !== 0 ? `(${d >= 0 ? '+' : ''}${(d / baseV * 100).toFixed(1)}%)` : '';
                deltaStr = `${d >= 0 ? '+' : ''}${valueFmt(d)} ${pct}`.trim();
            }
            return `<div style="${FONTS.cssSans}">
                <div style="${FONTS.cssMono} font-size: ${FONTS.tooltipTimestamp.fontSize}px; color: ${COLORS.fgSecondary}; margin-bottom: 8px;">
                    ${formattedTime}
                </div>
                <div style="display: flex; align-items: center; gap: 12px; margin-bottom: 6px;">
                    <span style="background: ${COLORS.accentSubtle}; padding: 3px 8px; border-radius: 4px; ${FONTS.cssMono} font-size: ${FONTS.tooltipTimestamp.fontSize}px; color: ${COLORS.accent};">
                        ${label}
                    </span>
                </div>
                <div style="display: grid; grid-template-columns: max-content max-content; gap: 2px 12px; ${FONTS.cssMono} font-size: ${FONTS.tooltipValue.fontSize}px;">
                    <span style="color: var(--compare-baseline, ${COLORS.fgSecondary});">${labels.baseline || 'baseline'}</span>
                    <span style="color: ${COLORS.fg}; font-weight: ${FONTS.tooltipValue.fontWeight};">${fmtCell(baseV)}</span>
                    <span style="color: var(--compare-experiment, ${COLORS.fgSecondary});">${labels.experiment || 'experiment'}</span>
                    <span style="color: ${COLORS.fg}; font-weight: ${FONTS.tooltipValue.fontWeight};">${fmtCell(expV)}</span>
                    <span style="color: ${COLORS.fgSecondary};">Δ</span>
                    <span style="color: ${COLORS.fg}; font-weight: ${FONTS.tooltipValue.fontWeight};">${deltaStr}</span>
                </div>
                ${getTooltipFreezeFooter(chart)}
            </div>`;
        }

        // Default variant: single value tooltip.
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

    // Compare-mode cross-cursor publisher. When the spec carries a
    // compareGroupId, every showtip / hidetip event publishes the
    // cell coordinates to sibling subscribers (the other half of the
    // pair). Off completely outside compare-mode pair rendering.
    const compareGroupId = chart.spec.compareGroupId;
    const chartsState = chart.chartsState;
    if (compareGroupId && chartsState && typeof chartsState.publishCompareCursor === 'function') {
        // Tear down any prior listener (reconfigure path).
        if (chart._compareCursorOff) chart._compareCursorOff();
        const onShow = (params) => {
            if (!params || !params.data) return;
            chartsState.publishCompareCursor(compareGroupId, {
                timeMs: params.data[0],
                qIdx: params.data[1],
                sourceChartId: chart.chartId,
            });
        };
        const onHide = () => {
            chartsState.publishCompareCursor(compareGroupId, null);
        };
        chart.echart.on('showTip', onShow);
        chart.echart.on('hideTip', onHide);
        chart._compareCursorOff = () => {
            chart.echart.off('showTip', onShow);
            chart.echart.off('hideTip', onHide);
            chart._compareCursorOff = null;
        };
    }

    // Compare-mode cross-cursor subscriber. Receives sibling events
    // and renders a thin crosshair at the matching cell. The publisher
    // filters out self-sourced events (via fn._chartId), so on its own
    // hover this chart doesn't draw a redundant crosshair on itself.
    if (compareGroupId && chartsState && typeof chartsState.subscribeCompareCursor === 'function') {
        if (chart._compareCursorUnsub) chart._compareCursorUnsub();
        const onCursor = (payload) => drawCompareCrosshair(chart, payload, timeData, tCount, seriesCount);
        onCursor._chartId = chart.chartId;  // tag for self-filter in publishCompareCursor
        chart._compareCursorUnsub = chartsState.subscribeCompareCursor(compareGroupId, onCursor);
    }

    // Vertical legend bar. For the default (RdYlGn-flipped) palette we
    // feed `1 - t` so the bottom (low value) stays green; for a custom
    // palette (e.g. compare-mode diff diverging) use the ramp directly
    // — strategies that supply their own palette get exactly the colors
    // they configured (no implicit flip).
    const legendColorFn = customPalette ? rampFn : (t) => rdYlGnColor(1 - t);
    const barCanvas = buildGradientCanvas(legendColorFn);

    // Bar gradient is linear in log10(value) space. Interpolate values
    // at evenly spaced positions for the tick labels. In diff mode the
    // values are signed (negative=baseline higher); the sign itself
    // disambiguates direction across all five ticks, so we don't strip
    // it. Captions above/below the bar reinforce the meaning.
    const TICK_COUNT = 5;
    const ticks = [];
    for (let i = 0; i < TICK_COUNT; i++) {
        const pos = i / (TICK_COUNT - 1);
        const value = colorMin > 0 && colorMax > 0
            ? Math.pow(10, safeLog(colorMin) + pos * (safeLog(colorMax) - safeLog(colorMin)))
            : colorMin + pos * (colorMax - colorMin);
        ticks.push({ pos, label: valueFmt(sigDigits(value)) });
    }
    // Diff captions: top of bar = max = "experiment is higher" (matches
    // the right end of the legacy horizontal layout).
    const diffLegendLabels = chart.spec.diffLegendLabels;
    // Re-run on every `finished` so the bar's height and top track the
    // grid rect (Y-axis). First call before layout uses defaults.
    const renderLegend = () => {
        const rect = chart.echart?.getModel()?.getComponent('grid')?.coordinateSystem?.getRect();
        ensureLegendBar(chart.domNode, barCanvas, {
            ticks,
            topCaption: diffLegendLabels ? diffLegendLabels.right : '',
            bottomCaption: diffLegendLabels ? diffLegendLabels.left : '',
            barTop: rect?.y,
            barHeight: rect?.height,
        });
    };
    renderLegend();
    if (chart._legendFinishedFn) chart.echart.off('finished', chart._legendFinishedFn);
    chart._legendFinishedFn = renderLegend;
    chart.echart.on('finished', renderLegend);
}

// Render a crosshair on `chart` at the matching cell from a sibling's
// cursor event. Cleared when payload is null. Uses ECharts' graphic
// component overlaid on the chart canvas — no relayout cost.
function drawCompareCrosshair(chart, payload, timeData, tCount, seriesCount) {
    if (!chart || !chart.echart) return;
    if (!payload) {
        chart.echart.setOption({ graphic: [] });
        return;
    }
    const { timeMs, qIdx } = payload;
    // Convert (timeMs, qIdx) to pixel coords. convertToPixel returns
    // null when the chart hasn't laid out yet.
    const pix = chart.echart.convertToPixel({ gridIndex: 0 }, [timeMs, qIdx]);
    if (!pix) return;

    const grid = chart.echart.getModel().getComponent('grid');
    const rect = grid?.coordinateSystem?.getRect();
    if (!rect) return;

    // Cell width/height: try to sample neighbours via convertToPixel
    // first (matches actual axis spacing including any non-uniform
    // mapping). For the rightmost time column or the top quantile row
    // the neighbour is past the axis max and convertToPixel returns
    // null — fall back to dividing the grid rect by the cell counts.
    const intervalMs = tCount > 1 ? (timeData[1] - timeData[0]) * 1000 : 1000;
    const widthSample = chart.echart.convertToPixel({ gridIndex: 0 }, [timeMs + intervalMs, qIdx]);
    const heightSample = chart.echart.convertToPixel({ gridIndex: 0 }, [timeMs, qIdx + 1]);
    const fallbackCellWidth = (tCount > 0) ? rect.width / tCount : rect.width;
    const fallbackCellHeight = (seriesCount > 0) ? rect.height / seriesCount : rect.height;
    const cellWidthPx = widthSample ? (widthSample[0] - pix[0]) : fallbackCellWidth;
    const cellHeightPx = heightSample ? (heightSample[1] - pix[1]) : -fallbackCellHeight;

    chart.echart.setOption({
        graphic: [
            // Horizontal line across grid at qIdx
            {
                type: 'rect',
                z: 100,
                shape: { x: rect.x, y: pix[1] + cellHeightPx, width: rect.width, height: 1 },
                style: { fill: 'rgba(255,255,255,0.6)' },
                silent: true,
            },
            // Vertical line across grid at timeMs
            {
                type: 'rect',
                z: 100,
                shape: { x: pix[0], y: rect.y, width: Math.max(1, cellWidthPx), height: rect.height },
                style: { fill: 'rgba(255,255,255,0.15)' },
                silent: true,
            },
        ],
    });
}
