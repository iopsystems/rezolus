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
    COLORS,
    FONTS,
} from './base.js';
import { VIRIDIS_COLORS, viridisColor } from './util/colormap.js';
import { buildGradientCanvas, ensureLegendBar } from './color_legend.js';

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

    // Extract all unique CPU IDs
    const yIndices = new Set();
    data.forEach(item => {
        yIndices.add(item[1]); // CPU ID
    });

    // Convert to array and sort numerically
    const cpuIds = Array.from(yIndices).sort((a, b) => a - b);

    // Ensure we have a continuous range of CPUs from 0 to max
    const maxCpuId = cpuIds.length > 0 ? Math.max(...cpuIds) : 0;
    const continuousCpuIds = Array.from({
        length: maxCpuId + 1
    }, (_, i) => i);

    if (continuousCpuIds.length !== cpuIds.length) {
        console.error('CPU IDs are not continuous', cpuIds);
    }

    // First, transform data into a simple 2d matrix of values.
    // dataMatrix[cpuId][timeIndex] = value
    const xCount = timeData.length;
    const yCount = continuousCpuIds.length;
    const dataMatrix = new Array(yCount).fill(null).map(() => new Array(xCount).fill(null));
    for (let i = 0; i < data.length; i++) {
        const [timeIndex, y, value] = data[i];
        dataMatrix[y][timeIndex] = value;
    }

    // Build data ordered by time (columns) first, then CPU (rows),
    // so that ECharts' progressive rendering fills left-to-right. When
    // the spec supplies a `nullCellColor`, emit null cells too so that
    // renderItem can paint them distinctly.
    const emitNullCells = !!chart.spec.nullCellColor;
    const processedData = [];
    for (let t = 0; t < xCount; t++) {
        for (let y = 0; y < yCount; y++) {
            const v = dataMatrix[y][t];
            if (v !== null) {
                processedData.push([timeData[t] * 1000, y, t, null, v]);
            } else if (emitNullCells) {
                processedData.push([timeData[t] * 1000, y, t, null, null]);
            }
        }
    }

    const MAX_DATA_POINT_DISPLAY = 50000;
    // Create a list of options for data to display at different levels of downsampling.
    // These are ordered from highest to lowest resolution. So, usage is to iterate through
    // them until one is low enough resolution.
    const nullColor = chart.spec.nullCellColor || null;
    chart.downsampleCache = [];
    chart.downsampleCache.push({
        factor: 1,
        data: processedData,
        renderItem: createRenderItemFunc(timeData, 1, nullColor),
    });
    const originalRatioOfDataPointsToMax = xCount * yCount / MAX_DATA_POINT_DISPLAY;

    if (originalRatioOfDataPointsToMax > 1) {
        const factor = Math.ceil(originalRatioOfDataPointsToMax);
        const downsampledData = downsample(dataMatrix, factor);
        const downsampledXCount = downsampledData[0].length;
        const processedDownsampledData = [];
        // Iterate time-first so progressive rendering fills left-to-right
        for (let x = 0; x < downsampledXCount; x++) {
            for (let y = 0; y < yCount; y++) {
                const minAndMax = downsampledData[y][x];
                if (minAndMax !== null) {
                    processedDownsampledData.push([timeData[x * factor] * 1000, y, x * factor, minAndMax[0], minAndMax[1]]);
                } else if (emitNullCells) {
                    processedDownsampledData.push([timeData[x * factor] * 1000, y, x * factor, null, null]);
                }
            }
        }
        chart.downsampleCache.push({
            factor,
            data: processedDownsampledData,
            renderItem: createRenderItemFunc(timeData, factor, nullColor),
        });
    }

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

    // Compare-mode diff heatmaps force symmetric bounds around 0 so the
    // diverging colormap maps baseline-heavy cells to one end and
    // experiment-heavy cells to the other.
    const symmetric = chart.spec.symmetricBounds === true;
    const visualMapMin = symmetric ? -Math.max(Math.abs(minValue), Math.abs(effectiveMax)) : minValue;
    const visualMapMax = symmetric ? Math.max(Math.abs(minValue), Math.abs(effectiveMax)) : effectiveMax;
    const visualMapColor = chart.spec.colormap || VIRIDIS_COLORS;

    // Compare-mode renderers override the x-axis formatter so labels
    // read as relative offsets (`+Xs`, `+XmYs`) instead of absolute
    // wall-clock times. When the spec sets `xAxisFormatter`, build a
    // merged xAxis that swaps only the `axisLabel.formatter` field.
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

    const option = {
        ...baseOption,
        ...(xAxisOverride ? { xAxis: xAxisOverride } : {}),
        grid: { ...baseOption.grid, top: '71' },
        yAxis,
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
            renderItem: chart.downsampleCache[chart.downsampleCache.length - 1].renderItem,
            clip: true,
            data: chart.downsampleCache[chart.downsampleCache.length - 1].data,
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

    // DOM legend bar: [minLabel] [colorBar] [maxLabel] in a flex row.
    // Suppressed in compare mode for the right-hand slot so the shared
    // color scale isn't rendered twice.
    if (!chart.spec.suppressLegendBar) {
        const formatter = createAxisLabelFormatter(unitSystem || 'count');
        const wrapper = chart.domNode.parentNode;
        // If the spec overrides the colormap (e.g. compare-mode diff heatmap),
        // build a gradient from that palette; otherwise use viridis.
        const legendColorFn = chart.spec.colormap
            ? buildRampColorFn(chart.spec.colormap)
            : viridisColor;
        // Invalidate any retained legend bar whose gradient doesn't match
        // the current colormap. Without this, toggling side-by-side → diff
        // reuses the stale viridis canvas in the existing bar and the
        // diverging palette never renders.
        const paletteSig = Array.isArray(chart.spec.colormap)
            ? chart.spec.colormap.join(',')
            : 'viridis';
        const existing = wrapper.querySelector('.heatmap-legend-bar');
        if (existing && existing.dataset.palette !== paletteSig) {
            existing.remove();
        }
        const barCanvas = buildGradientCanvas(legendColorFn);
        const { minLabel, maxLabel } = ensureLegendBar(wrapper, barCanvas);
        const bar = wrapper.querySelector('.heatmap-legend-bar');
        if (bar) bar.dataset.palette = paletteSig;
        minLabel.textContent = formatter(visualMapMin);
        maxLabel.textContent = formatter(visualMapMax);
    }

    // When this echart's zoom level changes, pick which set of potentially downsampled data to use.
    chart.echart.on('datazoom', (event) => {
        // 'datazoom' events triggered by the user vs dispatched by us have different formats:
        // User-triggered events have a batch property with the details under it.
        const zoomLevel = event.batch ? event.batch[0] : event;
        const factor = zoomLevelToFactor(zoomLevel, originalRatioOfDataPointsToMax, 1000 * (timeData[timeData.length - 1] - timeData[0]));
        for (let i = 0; i < chart.downsampleCache.length; i++) {
            const downsampleCacheItem = chart.downsampleCache[i];
            if (downsampleCacheItem.factor >= factor) {
                const data = downsampleCacheItem.data;
                const renderItem = downsampleCacheItem.renderItem;
                // Only update the echarts object if the data has changed.
                if (chart.echart.getOption().series[0].data.length !== data.length) {
                    chart.echart.setOption({
                        series: [{
                            data: data,
                            renderItem: renderItem
                        }]
                    });
                }
                break;
            }
        }
    });
}

/**
 * Create a downsampled version of the data matrix.
 * Combines every `factor` data points along the x axis into a single data point with a min and max value.
 * @param {Array<Array<number>>} dataMatrix
 * @param {number} factor
 * @returns {Array<Array<number>>}
 */
const downsample = (dataMatrix, factor) => {
    const yCount = dataMatrix.length;
    const xCount = dataMatrix[0].length;
    const downsampledXCount = Math.ceil(xCount / factor);
    const downsampledDataMatrix = new Array(yCount).fill(null).map(() => new Array(downsampledXCount).fill(null));
    for (let y = 0; y < yCount; y++) {
        for (let x = 0; x < Math.ceil(xCount / factor); x++) {
            let max = null;
            let min = null;
            for (let origX = x * factor; origX < (x + 1) * factor && origX < xCount; origX++) {
                if (dataMatrix[y][origX] !== null) {
                    if (max === null) {
                        max = dataMatrix[y][origX];
                        min = dataMatrix[y][origX];
                    } else {
                        max = Math.max(max, dataMatrix[y][origX]);
                        min = Math.min(min, dataMatrix[y][origX]);
                    }
                }
            }
            if (max !== null) {
                downsampledDataMatrix[y][x] = [min, max];
            }
        }
    }
    return downsampledDataMatrix;
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
