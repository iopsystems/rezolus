import {
    createAxisLabelFormatter
} from './util/units.js';
import {
    formatDateTime,
    clampToRange,
} from './util/utils.js';
import {
    getBaseOption,
    applyNoData,
    getTooltipFreezeFooter,
    applyChartOption,
    overrideXAxisFormatter,
    calculateMinZoomSpan,
    getDataZoomConfig,
    CHART_GRID_TOP_WITH_LEGEND,
    COLORS,
    FONTS,
} from './base.js';
import { VIRIDIS_COLORS, viridisColor } from './util/colormap.js';
import {
    createHeatmapResolutionStore,
    ensureHeatmapResolution,
} from './util/heatmap_data.js';
import {
    buildGradientCanvas,
    ensureLegendBar,
    linearTicks,
    sigDigits,
    LEGEND_GRID_RIGHT,
} from './color_legend.js';

/**
 * Build an `(t: 0..1) => cssColor` function that interpolates through a
 * palette array (CSS color strings). Used when the caller supplies a
 * custom colormap via `chart.spec.colormap`.
 */
const buildRampColorFn = (palette) => (t) => {
    if (!palette || palette.length === 0) return 'rgb(0,0,0)';
    if (palette.length === 1) return palette[0];
    const clamped = Math.max(0, Math.min(1, t));
    const idx = clamped * (palette.length - 1);
    const i = Math.floor(idx);
    if (i >= palette.length - 1) return palette[palette.length - 1];
    // Nearest-neighbor is sufficient for a legend gradient canvas; the
    // visualMap itself does the real interpolation server-side of echarts.
    const f = idx - i;
    return f < 0.5 ? palette[i] : palette[i + 1];
};

/**
 * Configures the Chart based on Chart.spec
 * Responsible for calling setOption on the echart instance, and for setting up any
 * chart-specific dynamic behavior.
 *
 * Heatmaps have both the worst built-in support in echarts and have additional complications.
 *
 * In particular, we are concerned about perf when there are many data points, so we downsample as needed.
 *
 * @param {import('./chart.js').Chart} chart - the chart to configure
 */
export function configureHeatmap(chart) {
    const {
        time_data: timeData,
        data,
        min_value: minValue,
        max_value: maxValue,
        opts
    } = chart.spec;

    if (!data || data.length < 1 || !timeData || timeData.length === 0) {
        applyNoData(chart);
        return;
    }

    const baseOption = getBaseOption();

    const xCount = timeData.length;
    const emitNullCells = !!chart.spec.nullCellColor;
    const resolutionStore = createHeatmapResolutionStore(data, timeData, emitNullCells);
    chart.heatmapResolutionStore = resolutionStore;
    const yCount = resolutionStore.yCount;
    const continuousCpuIds = Array.from({ length: yCount }, (_, i) => i);
    if (continuousCpuIds.length !== resolutionStore.cpuIds.length) {
        console.error('CPU IDs are not continuous', resolutionStore.cpuIds);
    }

    const MAX_DATA_POINT_DISPLAY = 50000;
    const nullColor = chart.spec.nullCellColor || null;
    const originalRatioOfDataPointsToMax = xCount * yCount / MAX_DATA_POINT_DISPLAY;
    const initialFactor = Math.max(1, Math.ceil(originalRatioOfDataPointsToMax));
    const initialResolution = ensureHeatmapResolution(resolutionStore, initialFactor);
    chart._heatmapRenderedFactor = initialResolution.factor;

    // Y axis labels: if more than Y_MAX_LABELS, show every 2nd, 4th, 8th, 16th, or etc.
    const Y_MAX_LABELS = 16;
    // What's the smallest power of 2 that's greater than or equal to yCount / Y_MAX_LABELS?
    const yLabelMultiple = Math.pow(2, Math.ceil(Math.log2(Math.ceil(yCount / Y_MAX_LABELS))));
    // This tells echarts how many labels to skip. E.g. show 1, skip 7, show 1, skip 7, etc.
    const yAxisLabelInterval = yLabelMultiple - 1;
    // We have space to show more ticks than labels.
    const Y_MAX_TICKS_PER_LABEL = 4;
    const yTickMultiple = Math.ceil(yLabelMultiple / Y_MAX_TICKS_PER_LABEL);
    const yAxisTickInterval = yTickMultiple - 1;


    // Access format properties using snake_case naming to match Rust serialization
    const format = opts.format || {};
    const unitSystem = format.unit_system;
    const range = format.range;
    const yAxisLabel = format.y_axis_label || format.axis_label;
    const valueLabel = format.value_label;

    // Configure tooltip with unit formatting if specified
    const customXFormatterForTooltip = chart.spec.xAxisFormatter;
    const diffMatrices = chart.spec.diffMatrices;
    const diffCaptureLabels = chart.spec.diffCaptureLabels;
    let tooltipFormatter = function (params) {
        if (params.data === undefined) {
            return '';
        }
        // If this is a downsampled data point, `value` is the max value.
        // Otherwise, it's just the value, with `minValue` being null.
        const [time, cpu, timeIndex, minVal, value] = params.data;

        // In compare mode, time is already the post-anchor relative ms
        // value (the rebase happens before the chart is fed). Use the
        // custom formatter (e.g. `+Xs`) instead of the absolute clock.
        const formattedTime = customXFormatterForTooltip
            ? customXFormatterForTooltip(time)
            : formatDateTime(time);

        // Null cells (compare-mode diff heatmaps where one side is missing)
        // render with a short "no data" tooltip instead of number formatting.
        if (value === null || value === undefined) {
            return `<div style="${FONTS.cssSans}">
                        <div style="${FONTS.cssMono} font-size: ${FONTS.tooltipTimestamp.fontSize}px; color: ${COLORS.fgSecondary}; margin-bottom: 8px;">
                            ${formattedTime}
                        </div>
                        <div style="display: flex; align-items: center; gap: 12px;">
                            <span style="background: ${COLORS.accentSubtle}; padding: 3px 8px; border-radius: 4px; ${FONTS.cssMono} font-size: ${FONTS.tooltipTimestamp.fontSize}px; color: ${COLORS.accent};">
                                CPU ${cpu}
                            </span>
                            <span style="${FONTS.cssMono} font-weight: ${FONTS.tooltipValue.fontWeight}; font-size: ${FONTS.tooltipValue.fontSize}px; color: ${COLORS.fgMuted};">
                                no data
                            </span>
                        </div>
                        ${getTooltipFreezeFooter(chart)}
                    </div>`;
        }

        const fmt = unitSystem
            ? createAxisLabelFormatter(unitSystem)
            : (v) => v.toFixed(6);

        // Diff heatmap: pull baseline + experiment absolute values from
        // the side-channel matrices and render both instead of the
        // computed delta. The delta itself is one subtraction away and
        // the user can read it off the color already; the absolute
        // values tell them where on the scale each capture actually sat.
        if (diffMatrices) {
            const bv = diffMatrices.baseline?.[cpu]?.[timeIndex];
            const ev = diffMatrices.experiment?.[cpu]?.[timeIndex];
            const fmtCell = (v) => (v == null || Number.isNaN(v)) ? '—' : fmt(v);
            return `<div style="${FONTS.cssSans}">
                        <div style="${FONTS.cssMono} font-size: ${FONTS.tooltipTimestamp.fontSize}px; color: ${COLORS.fgSecondary}; margin-bottom: 8px;">
                            ${formattedTime}
                        </div>
                        <div style="display: flex; align-items: center; gap: 12px; margin-bottom: 4px;">
                            <span style="background: ${COLORS.accentSubtle}; padding: 3px 8px; border-radius: 4px; ${FONTS.cssMono} font-size: ${FONTS.tooltipTimestamp.fontSize}px; color: ${COLORS.accent};">
                                CPU ${cpu}
                            </span>
                        </div>
                        <div style="display: grid; grid-template-columns: max-content max-content; gap: 2px 12px; ${FONTS.cssMono} font-size: ${FONTS.tooltipValue.fontSize}px;">
                            <span style="color: var(--compare-baseline, ${COLORS.fgSecondary});">${diffCaptureLabels?.baseline || 'baseline'}</span>
                            <span style="color: ${COLORS.fg}; font-weight: ${FONTS.tooltipValue.fontWeight};">${fmtCell(bv)}</span>
                            <span style="color: var(--compare-experiment, ${COLORS.fgSecondary});">${diffCaptureLabels?.experiment || 'experiment'}</span>
                            <span style="color: ${COLORS.fg}; font-weight: ${FONTS.tooltipValue.fontWeight};">${fmtCell(ev)}</span>
                        </div>
                        ${getTooltipFreezeFooter(chart)}
                    </div>`;
        }

        const label = valueLabel ? `<span style="margin-left: 10px;">${valueLabel}: </span>` : '';

        const [clampedVal, rawVal] = clampToRange(value, range);
        const isClamped = rawVal != null;

        let valueString;
        if (minVal === null) {
            valueString = fmt(clampedVal);
            if (isClamped) {
                valueString += ` <span style="color: ${COLORS.fgMuted};">(raw: ${fmt(rawVal)})</span>`;
            }
        } else {
            const [clampedMin, rawMin] = clampToRange(minVal, range);
            const isMinClamped = rawMin != null;
            valueString = `${fmt(clampedMin)} - ${fmt(clampedVal)}`;
            if (isClamped || isMinClamped) {
                valueString += ` <span style="color: ${COLORS.fgMuted};">(raw: ${fmt(isMinClamped ? rawMin : clampedMin)} - ${fmt(isClamped ? rawVal : clampedVal)})</span>`;
            }
        }

        return `<div style="${FONTS.cssSans}">
                    <div style="${FONTS.cssMono} font-size: ${FONTS.tooltipTimestamp.fontSize}px; color: ${COLORS.fgSecondary}; margin-bottom: 8px;">
                        ${formattedTime}
                    </div>
                    <div style="display: flex; align-items: center; gap: 12px;">
                        <span style="background: ${COLORS.accentSubtle}; padding: 3px 8px; border-radius: 4px; ${FONTS.cssMono} font-size: ${FONTS.tooltipTimestamp.fontSize}px; color: ${COLORS.accent};">
                            CPU ${cpu}
                        </span>
                        ${label}
                        <span style="${FONTS.cssMono} font-weight: ${FONTS.tooltipValue.fontWeight}; font-size: ${FONTS.tooltipValue.fontSize}px; color: ${COLORS.fg};">
                            ${valueString}
                        </span>
                    </div>
                    ${getTooltipFreezeFooter(chart)}
                </div>`;
    };

    const yAxis = {
        type: 'category',
        name: yAxisLabel || 'CPU',
        nameLocation: 'middle',
        nameGap: 40,
        nameTextStyle: {
            color: COLORS.fg,
            ...FONTS.legend,
            padding: [0, 0, 0, 20]
        },
        data: continuousCpuIds,
        axisLine: {
            show: false,
        },
        axisLabel: {
            interval: yAxisLabelInterval,
            color: COLORS.fgSecondary,
            ...FONTS.axisLabel,
        },
        axisTick: {
            show: false,
            interval: yAxisTickInterval,
        }
    };

    const effectiveMax = range?.max != null ? Math.min(maxValue, range.max) : maxValue;
    const visualMapMin = minValue;
    const visualMapMax = effectiveMax;
    const visualMapColor = chart.spec.colormap || VIRIDIS_COLORS;

    // Compare-mode renderers override the x-axis formatter so labels
    // read as relative offsets (`+Xs`, `+XmYs`) instead of absolute
    // wall-clock times.
    const xAxisOverride = overrideXAxisFormatter(baseOption.xAxis, chart.spec.xAxisFormatter);

    const option = {
        ...baseOption,
        ...(xAxisOverride ? { xAxis: xAxisOverride } : {}),
        grid: {
            ...baseOption.grid,
            top: String(CHART_GRID_TOP_WITH_LEGEND),
            right: String(LEGEND_GRID_RIGHT),
        },
        yAxis,
        // dataZoom is the component takeGlobalCursor's dataZoomSelect
        // attaches to when the user drags a selection on the canvas.
        // Without this, the drag does nothing on heatmaps — which was
        // the long-standing heatmap-drag-zoom bug.
        dataZoom: getDataZoomConfig(calculateMinZoomSpan(timeData)),
        // Echarts has two render modes for hover effects. When number of chart elements is
        // below this threshold, it just draws the hover effect onto the same canvas.
        // When above this threshold, it draws them onto a separate canvas element (zrender's
        // "hoverLayer", which has data-zr-dom-id="zr_100000").
        // Echarts has a bug that when you zoom in and thereby transition from one mode to the other,
        // the hover effect on the hoverLayer is not erased. It sticks around as a weird
        // graphical artifact.
        // Setting the hoverLayerThreshold to 0 means that it won't switch between modes. Drawing
        // onto the separate layer apparently has some drawbacks according to echarts, but I don't
        // see any detriment for us. https://echarts.apache.org/en/option.html#hoverLayerThreshold
        // (I haven't seen any artifacts on our other chart types, so only adding it to heatmaps.)
        hoverLayerThreshold: 0,
        tooltip: {
            ...baseOption.tooltip,
            trigger: 'item',
            axisPointer: {
                type: 'line',
                animation: false,
                lineStyle: {
                    color: COLORS.accent,
                    opacity: 0.5,
                },
                label: {
                    backgroundColor: COLORS.bgCard
                }
            },
            position: 'top',
            formatter: tooltipFormatter,
        },
        visualMap: {
            type: 'continuous',
            min: visualMapMin,
            max: visualMapMax,
            calculable: false,
            show: false,
            inRange: {
                color: visualMapColor,
            }
        },
        series: [{
            name: chart.spec.opts.title,
            type: 'custom',
            renderItem: createRenderItemFunc(timeData, initialResolution.factor, nullColor),
            clip: true,
            data: initialResolution.data,
            emphasis: {
                itemStyle: {
                    shadowBlur: 10,
                    shadowColor: COLORS.shadowStrong
                }
            },
            // https://echarts.apache.org/en/option.html#series-heatmap.progressive
            // Bigger numbers mean more data is rendered at once.
            // Rendering smaller pieces at a time has a bigger perf impact than you
            // might think as every progressive render also requires reevaluating the
            // existing rendered stuff, so it's a quadratic cost to some extent.
            progressive: 8000,
            progressiveThreshold: 3000,
            animation: false
        }]
    };

    applyChartOption(chart, option);

    // DOM legend bar: vertical gradient with tick marks and labels to
    // the right of the chart canvas. Mounted inside chart.domNode (the
    // Chart component's own Mithril-managed div) so the legend is
    // removed with the Chart on unmount/swap.
    const formatter = createAxisLabelFormatter(unitSystem || 'count');
    const legendColorFn = chart.spec.colormap
        ? buildRampColorFn(chart.spec.colormap)
        : viridisColor;
    const barCanvas = buildGradientCanvas(legendColorFn);

    // In diff mode the values are signed (negative=baseline higher);
    // the sign itself disambiguates direction across all five ticks, so
    // we don't strip it. Captions above/below reinforce the meaning.
    const ticks = linearTicks(visualMapMin, visualMapMax, 5).map((t) => ({
        pos: t.pos,
        label: formatter(sigDigits(t.value)),
    }));
    const diffLabels = chart.spec.diffLegendLabels;
    ensureLegendBar(chart.domNode, barCanvas, {
        ticks,
        // Top of bar = max = positive value = "experiment is higher"
        // (which matched the right end in the legacy horizontal layout).
        topCaption: diffLabels ? diffLabels.right : '',
        bottomCaption: diffLabels ? diffLabels.left : '',
    });

    // When this echart's zoom level changes, pick which set of potentially downsampled data to use.
    chart.echart.on('datazoom', (event) => {
        // 'datazoom' events triggered by the user vs dispatched by us have different formats:
        // User-triggered events have a batch property with the details under it.
        const zoomLevel = event.batch ? event.batch[0] : event;
        const factor = zoomLevelToFactor(zoomLevel, originalRatioOfDataPointsToMax, 1000 * (timeData[timeData.length - 1] - timeData[0]));
        const resolution = ensureHeatmapResolution(resolutionStore, factor);
        if (chart._heatmapRenderedFactor !== resolution.factor) {
            chart._heatmapRenderedFactor = resolution.factor;
            chart.echart.setOption({
                series: [{
                    data: resolution.data,
                    renderItem: createRenderItemFunc(timeData, resolution.factor, nullColor),
                }],
            });
        }
    });
}

/**
 * Convert a zoom level to a factor.
 * @param {{start: number, end: number, startValue: number, endValue: number}} zoomLevel from echarts
 * @param {number} originalRatioOfDataPointsToMax e.g. if it's 2 then there are 2x as many data points as the max we want to draw.
 * @param {number} originalXDifference x[n] - x[1]. Needed because zoom level is sometimes just raw x values.
 * @returns {number}
 */
const zoomLevelToFactor = (zoomLevel, originalRatioOfDataPointsToMax, originalXDifference) => {
    const { start, end, startValue, endValue } = zoomLevel;
    if (start !== undefined && end !== undefined) {
        const fraction = (end - start) / 100;
        if (fraction <= 0) {
            return 1;
        }
        return Math.ceil(originalRatioOfDataPointsToMax * fraction);
    } else if (startValue !== undefined && endValue !== undefined) {
        const fraction = (endValue - startValue) / originalXDifference;
        if (fraction <= 0) {
            return 1;
        }
        return Math.ceil(originalRatioOfDataPointsToMax * fraction);
    }
    // No zoom level specified, so assume fully zoomed out.
    return Math.ceil(originalRatioOfDataPointsToMax);
}
/**
 * Custom-type echarts charts rely on renderItem.
 * This creates one, accounting for the downsampling factor.
 * @param {Array<number>} timeData the array of original x values
 * @param {number} factor the downsampling factor
 * @param {string|null} nullColor fill color for null cells (compare-mode
 *        diff heatmaps). When null, null cells are skipped entirely.
 * @returns {function} renderItem function for echarts
 */
const createRenderItemFunc = (timeData, factor, nullColor) => {
    return function (params, api) {
        const x = api.value(0);
        const y = api.value(1);
        const timeIndex = api.value(2);
        const value = api.value(4);
        const nextX = timeData[timeIndex + factor] * 1000 || Number.MAX_VALUE;
        const start = api.coord([x, y]);
        const end = api.coord([nextX, y]);
        const width = end[0] - start[0] + 1; // +1 pixel to avoid hairline cracks.
        const height = api.size([0, 1])[1];
        const isNull = value === null || value === undefined;
        if (isNull && !nullColor) {
            // No null color configured — don't paint (matches legacy behavior).
            return;
        }
        return (
            {
                type: 'rect',
                transition: [],
                shape: {
                    x: start[0],
                    y: start[1] - height / 2,
                    width: width,
                    height: height
                },
                // Do not use all of api.style() - this causes big performance issues.
                style: {
                    // Use the appropriate fill color from the color scale,
                    // or the configured null color for missing data.
                    fill: isNull ? nullColor : api.style().fill,
                }
            }
        );
    };
}
