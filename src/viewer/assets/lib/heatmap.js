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

    const processedData = [];
    for (let i = 0; i < data.length; i++) {
        const [timeIndex, y, value] = data[i];
        if (timeIndex >= 0 && timeIndex < timeData.length) {
            processedData.push([timeData[timeIndex] * 1000, y, value]);
        }
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

    // Access format properties using snake_case naming to match Rust serialization
    const format = opts.format || {};
    const unitSystem = format.unit_system;
    const yAxisLabel = format.y_axis_label || format.axis_label;
    const valueLabel = format.value_label;

    // Configure tooltip with unit formatting if specified
    let tooltipFormatter = function (params) {
        const [time, cpu, value] = params.data;

        const formattedTime = formatDateTime(time / 1000, 'full');

        if (unitSystem) {
            const formatter = createAxisLabelFormatter(unitSystem);
            const label = valueLabel ? `${valueLabel}: ` : '';
            return `${formattedTime}<br>CPU: ${cpu}&nbsp;&nbsp;&nbsp; ${label}<span style="font-weight: bold; float: right;">${formatter(value)}</span>`;
        } else {
            return `${formattedTime}<br>CPU: ${cpu}&nbsp;&nbsp;&nbsp; ${value.toFixed(6)}`;
        }
    };

    const renderItem = function (params, api) {
        var x = api.value(0);
        var y = api.value(1);
        var start = api.coord([x, y]);
        var end = api.coord([x + 1000, y]);
        var height = api.size([0, 1])[1];
        return (
            {
                type: 'rect',
                transition: [],
                shape: {
                    x: start[0],
                    y: start[1] - height / 2,
                    width: end[0] - start[0] + .5, // The .5 pixel extra helps avoid hairline cracks.
                    height: height
                },
                style: api.style()
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
            color: '#ABABAB'
        }
    };

    return {
        ...baseOption,
        yAxis,
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
            data: processedData,
            emphasis: {
                itemStyle: {
                    shadowBlur: 10,
                    shadowColor: 'rgba(0, 0, 0, 0.5)'
                }
            },
            progressive: 1000,
            progressiveThreshold: 3000,
            animation: false
        }]
    };
}