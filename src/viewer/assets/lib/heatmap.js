import {
    createAxisLabelFormatter
} from './units.js';
import {
    formatDateTime
} from './utils.js';

/**
 * Creates a heatmap chart configuration for ECharts
 * 
 * @param {Object} baseOption - Base chart options
 * @param {Object} plotSpec - Plot specification with data and options
 * @returns {Object} ECharts configuration object
 */
export function createHeatmapOption(baseOption, plotSpec) {
    const {
        time_data: timeData,
        data,
        min_value: minValue,
        max_value: maxValue,
        opts
    } = plotSpec;

    if (!data || data.length < 1) {
        return baseOption;
    }

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

    const MAX_DATA_POINT_DISPLAY = 1000000; // 60000;
    let condensedDataMatrix = null;
    let processedCondensedData = null;
    let divisor = null;
    let condensedXCount = null;
    if (xCount * yCount > MAX_DATA_POINT_DISPLAY) {
        // Create a condensed data matrix by averaging the values of the original data matrix over several consecutive time steps.
        divisor = Math.ceil(xCount * yCount / MAX_DATA_POINT_DISPLAY);
        condensedXCount = Math.ceil(xCount / divisor);
        console.log("Condensing data of ", yCount, "CPUs from", xCount, "timesteps to", condensedXCount, "using a divisor of", divisor);
        condensedDataMatrix = new Array(yCount).fill(null).map(() => new Array(condensedXCount).fill(null));
        for (let y = 0; y < yCount; y++) {
            for (let x = 0; x < condensedXCount; x++) {
                let sum = 0;
                let count = 0;
                for (let origX = x * divisor; origX < (x + 1) * divisor && origX < xCount; origX++) {
                    if (dataMatrix[y][origX] !== null) {
                        sum += dataMatrix[y][origX];
                        count++;
                    }
                }
                if (count > 0) {
                    condensedDataMatrix[y][x] = sum / count;
                }
            }
        }
    }

    const processedData = [];
    for (let i = 0; i < data.length; i++) {
        const [timeIndex, y, value] = data[i];
        if (timeIndex >= 0 && timeIndex < timeData.length) {
            processedData.push([timeData[timeIndex] * 1000, y, timeIndex, value]);
        }
    }

    if (condensedDataMatrix) {
        processedCondensedData = [];
        for (let y = 0; y < yCount; y++) {
            for (let x = 0; x < condensedXCount; x++) {
                const value = condensedDataMatrix[y][x];
                if (value !== null) {
                    processedCondensedData.push([timeData[x * divisor] * 1000, y, x * divisor, value]);
                }
            }
        }
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
        const [time, cpu, timeIndex, value] = params.data;

        const formattedTime = formatDateTime(time);

        if (unitSystem) {
            const formatter = createAxisLabelFormatter(unitSystem);
            const label = valueLabel ? `${valueLabel}: ` : '';
            return `${formattedTime}<br>CPU: ${cpu}&nbsp;&nbsp;&nbsp; ${label}<span style="font-weight: bold; float: right;">${formatter(value)}</span>`;
        } else {
            return `${formattedTime}<br>CPU: ${cpu}&nbsp;&nbsp;&nbsp; ${value.toFixed(6)}`;
        }
    };

    const renderItem = function (params, api) {
        const x = api.value(0);
        const y = api.value(1);
        const timeIndex = api.value(2);
        const nextX = timeData[timeIndex + (divisor ?? 1)] * 1000 || Number.MAX_VALUE;
        const start = api.coord([x, y]);
        const end = api.coord([nextX, y]);
        const width = end[0] - start[0] + 1; // +1 pixel to avoid hairline cracks.
        const height = api.size([0, 1])[1];
        // if (x === timeData[0] * 1000) {
        //     console.log("start", start[0], "end", end[0], "width", width, "height", height);
        // }
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
    }

    const yAxis = {
        type: 'category',
        name: yAxisLabel || 'CPU',
        nameLocation: 'middle',
        nameGap: 40,
        nameTextStyle: {
            color: '#E0E0E0',
            fontSize: 14,
            padding: [0, 0, 0, 20]
        },
        data: continuousCpuIds,
        axisLabel: {
            interval: yAxisLabelInterval,
            color: '#ABABAB'
        },
        axisTick: {
            interval: yAxisTickInterval,
        }
    };

    return {
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
                    color: '#E0E0E0',
                },
                label: {
                    backgroundColor: '#505765'
                }
            },
            position: 'top',
            formatter: tooltipFormatter,
            textStyle: {
                color: '#E0E0E0'
            },
            backgroundColor: 'rgba(50, 50, 50, 0.8)',
            borderColor: 'rgba(70, 70, 70, 0.8)',
        },
        visualMap: {
            type: 'continuous',
            min: minValue,
            max: maxValue,
            calculable: false,
            show: false, // Show the color scale
            inRange: {
                color: [
                    '#440154', '#481a6c', '#472f7d', '#414487', '#39568c',
                    '#31688e', '#2a788e', '#23888e', '#1f988b', '#22a884',
                    '#35b779', '#54c568', '#7ad151', '#a5db36', '#d2e21b', '#fde725'
                ]
            }
        },
        series: [{
            name: plotSpec.opts.title,
            type: 'custom',
            renderItem,
            clip: true,
            data: processedCondensedData || processedData,
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
}