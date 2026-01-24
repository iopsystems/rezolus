import {
    createAxisLabelFormatter
} from './util/units.js';
import {
    formatDateTime
} from './util/utils.js';
import {
    getBaseOption,
    getNoDataOption,
    COLORS,
} from './base.js';

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
        chart.echart.setOption(getNoDataOption(opts.title));
        return;
    }

    const baseOption = getBaseOption(opts.title);

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

    const processedData = [];
    for (let i = 0; i < data.length; i++) {
        const [timeIndex, y, value] = data[i];
        if (timeIndex >= 0 && timeIndex < timeData.length) {
            processedData.push([timeData[timeIndex] * 1000, y, timeIndex, null, value]);
        }
    }

    const MAX_DATA_POINT_DISPLAY = 50000;
    // Create a list of options for data to display at different levels of downsampling.
    // These are ordered from highest to lowest resolution. So, usage is to iterate through
    // them until one is low enough resolution.
    chart.downsampleCache = [];
    chart.downsampleCache.push({ factor: 1, data: processedData, renderItem: createRenderItemFunc(timeData, 1) });
    const originalRatioOfDataPointsToMax = xCount * yCount / MAX_DATA_POINT_DISPLAY;

    if (originalRatioOfDataPointsToMax > 1) {
        const factor = Math.ceil(originalRatioOfDataPointsToMax);
        const downsampledData = downsample(dataMatrix, factor);
        const downsampledXCount = downsampledData[0].length;
        const processedDownsampledData = [];
        for (let y = 0; y < yCount; y++) {
            for (let x = 0; x < downsampledXCount; x++) {
                const minAndMax = downsampledData[y][x];
                if (minAndMax !== null) {
                    processedDownsampledData.push([timeData[x * factor] * 1000, y, x * factor, minAndMax[0], minAndMax[1]]);
                }
            }
        }
        chart.downsampleCache.push({ factor, data: processedDownsampledData, renderItem: createRenderItemFunc(timeData, factor) });
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
    const yAxisLabel = format.y_axis_label || format.axis_label;
    const valueLabel = format.value_label;

    // Configure tooltip with unit formatting if specified
    let tooltipFormatter = function (params) {
        if (params.data === undefined) {
            return '';
        }
        // If this is a downsampled data point, `value` is the max value.
        // Otherwise, it's just the value, with `minValue` being null.
        const [time, cpu, timeIndex, minVal, value] = params.data;

        const formattedTime = formatDateTime(time);

        let label, formattedValue, formattedMinValue;
        if (unitSystem) {
            const formatter = createAxisLabelFormatter(unitSystem);
            label = valueLabel ? `<span style="margin-left: 10px;">${valueLabel}: </span>` : '';
            formattedValue = formatter(value);
            formattedMinValue = minVal === null ? '' : formatter(minVal);
        } else {
            label = '';
            formattedValue = value.toFixed(6);
            formattedMinValue = minVal === null ? '' : minVal.toFixed(6);
        }
        const valueString = minVal === null ? formattedValue : `${formattedMinValue} - ${formattedValue}`;
        return `<div style="font-family: 'Inter', -apple-system, sans-serif;">
                    <div style="font-family: 'JetBrains Mono', monospace; font-size: 11px; color: ${COLORS.fgSecondary}; margin-bottom: 8px;">
                        ${formattedTime}
                    </div>
                    <div style="display: flex; align-items: center; gap: 12px;">
                        <span style="background: ${COLORS.accentSubtle}; padding: 3px 8px; border-radius: 4px; font-family: 'JetBrains Mono', monospace; font-size: 11px; color: ${COLORS.accent};">
                            CPU ${cpu}
                        </span>
                        ${label}
                        <span style="font-family: 'JetBrains Mono', monospace; font-weight: 600; font-size: 12px; color: ${COLORS.fg};">
                            ${valueString}
                        </span>
                    </div>
                </div>`;
    };

    const yAxis = {
        type: 'category',
        name: yAxisLabel || 'CPU',
        nameLocation: 'middle',
        nameGap: 40,
        nameTextStyle: {
            color: COLORS.fg,
            fontSize: 11,
            fontFamily: '"JetBrains Mono", "SF Mono", monospace',
            padding: [0, 0, 0, 20]
        },
        data: continuousCpuIds,
        axisLine: {
            show: false,
        },
        axisLabel: {
            interval: yAxisLabelInterval,
            color: COLORS.fgSecondary,
            fontSize: 10,
            fontFamily: '"JetBrains Mono", "SF Mono", monospace',
        },
        axisTick: {
            show: false,
            interval: yAxisTickInterval,
        }
    };

    const option = {
        ...baseOption,
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
            min: minValue,
            max: maxValue,
            calculable: false,
            show: false,
            inRange: {
                // Inferno colormap - perceptually uniform, high contrast
                color: [
                    '#000004',  // black
                    '#1b0c41',  // dark purple
                    '#4a0c6b',  // purple
                    '#781c6d',  // magenta
                    '#a52c60',  // pink-red
                    '#cf4446',  // red
                    '#ed6925',  // orange
                    '#fb9b06',  // yellow-orange
                    '#f7d13d',  // yellow
                    '#fcffa4',  // pale yellow
                ]
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
                    shadowColor: 'rgba(0, 0, 0, 0.5)'
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

    // Use notMerge: true to clear any previous chart configuration
    chart.echart.setOption(option, { notMerge: true });

    // Re-enable drag-to-zoom after clearing the chart
    chart.echart.dispatchAction({
        type: 'takeGlobalCursor',
        key: 'dataZoomSelect',
        dataZoomSelectActive: true,
    });

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
 * @returns {function} renderItem function for echarts
 */
const createRenderItemFunc = (timeData, factor) => {
    return function (params, api) {
        const x = api.value(0);
        const y = api.value(1);
        const timeIndex = api.value(2);
        const nextX = timeData[timeIndex + factor] * 1000 || Number.MAX_VALUE;
        const start = api.coord([x, y]);
        const end = api.coord([nextX, y]);
        const width = end[0] - start[0] + 1; // +1 pixel to avoid hairline cracks.
        const height = api.size([0, 1])[1];
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
                    // Use the appropriate fill color from the color scale.
                    fill: api.style().fill
                }
            }
        );
    };
}
