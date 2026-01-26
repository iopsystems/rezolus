// histogram_heatmap.js - Histogram heatmap chart for latency distributions

import {
    formatDateTime
} from './util/utils.js';
import {
    getBaseOption,
    getNoDataOption,
    COLORS,
} from './base.js';

/**
 * Format a latency bucket boundary for display
 * @param {number} nanoseconds - The bucket boundary in nanoseconds
 * @returns {string} Formatted latency string
 */
function formatLatencyBucket(nanoseconds) {
    if (nanoseconds < 1000) {
        return `${nanoseconds}ns`;
    } else if (nanoseconds < 1000000) {
        return `${(nanoseconds / 1000).toFixed(1)}µs`;
    } else if (nanoseconds < 1000000000) {
        return `${(nanoseconds / 1000000).toFixed(1)}ms`;
    } else {
        return `${(nanoseconds / 1000000000).toFixed(2)}s`;
    }
}

/**
 * Inferno colormap - perceptually uniform, high contrast
 * Optimized for spotting hotspots in latency distributions
 * @param {number} t - Value from 0 to 1
 * @returns {string} RGB color string
 */
function heatmapColor(t) {
    // Inferno colormap: black -> purple -> red -> orange -> yellow
    const colors = [
        [0, 0, 4],        // black
        [27, 12, 65],     // dark purple
        [74, 12, 107],    // purple
        [120, 28, 109],   // magenta
        [165, 44, 96],    // pink-red
        [207, 68, 70],    // red
        [237, 105, 37],   // orange
        [251, 155, 6],    // yellow-orange
        [247, 209, 61],   // yellow
        [252, 255, 164],  // pale yellow
    ];

    const idx = t * (colors.length - 1);
    const i = Math.floor(idx);
    const f = idx - i;

    if (i >= colors.length - 1) {
        return `rgb(${colors[colors.length - 1].join(',')})`;
    }

    const c0 = colors[i];
    const c1 = colors[i + 1];
    const r = Math.round(c0[0] + f * (c1[0] - c0[0]));
    const g = Math.round(c0[1] + f * (c1[1] - c0[1]));
    const b = Math.round(c0[2] + f * (c1[2] - c0[2]));

    return `rgb(${r},${g},${b})`;
}

/**
 * Configures the Chart for histogram heatmap visualization
 * Uses a log-scale Y-axis with custom series for proper bucket sizing
 * @param {import('./chart.js').Chart} chart - the chart to configure
 */
export function configureHistogramHeatmap(chart) {
    const {
        time_data: timeData,
        bucket_bounds: bucketBounds,
        data,
        min_value: minValue,
        max_value: maxValue,
        opts
    } = chart.spec;

    if (!data || data.length === 0 || !timeData || timeData.length === 0) {
        chart.echart.setOption(getNoDataOption(opts.title));
        return;
    }

    const baseOption = getBaseOption(opts.title);

    // Find the range of buckets that actually have data
    let minBucketIdx = Infinity;
    let maxBucketIdx = -Infinity;
    for (const [_, bucketIdx, count] of data) {
        if (count > 0) {
            minBucketIdx = Math.min(minBucketIdx, bucketIdx);
            maxBucketIdx = Math.max(maxBucketIdx, bucketIdx);
        }
    }

    // Add some padding around the range
    minBucketIdx = Math.max(0, minBucketIdx - 1);
    maxBucketIdx = Math.min(bucketBounds.length - 1, maxBucketIdx + 1);

    // Get the visible bucket bounds for log scale
    const minBucketValue = minBucketIdx > 0 ? Math.max(1, bucketBounds[minBucketIdx - 1]) : 1;
    const maxBucketValue = bucketBounds[maxBucketIdx];

    // Calculate the time interval for cell width (in milliseconds)
    const timeIntervalMs = timeData.length > 1
        ? (timeData[1] - timeData[0]) * 1000
        : 1000;

    // Create a lookup map for data points
    const dataMap = new Map();
    for (const [timeIdx, bucketIdx, count] of data) {
        if (bucketIdx >= minBucketIdx && bucketIdx <= maxBucketIdx) {
            dataMap.set(`${timeIdx}-${bucketIdx}`, count);
        }
    }

    // Only generate data for cells with actual data (non-zero counts)
    // This is much faster than rendering all cells - empty cells show as background
    const allCellsData = [];
    for (const [timeIdx, bucketIdx, count] of data) {
        if (bucketIdx < minBucketIdx || bucketIdx > maxBucketIdx || count === 0) {
            continue;
        }
        const timestampMs = timeData[timeIdx] * 1000;
        const lowerBound = bucketIdx > 0 ? Math.max(1, bucketBounds[bucketIdx - 1]) : 1;
        const upperBound = bucketBounds[bucketIdx];

        // Normalize count by bucket width in log space to get density
        const logWidth = Math.log(upperBound) - Math.log(lowerBound);
        const normalizedCount = logWidth > 0 ? count / logWidth : count;

        allCellsData.push([timestampMs, lowerBound, upperBound, normalizedCount, timeIdx, bucketIdx]);
    }

    // Calculate min/max of normalized values for color scaling
    let normalizedMin = Infinity;
    let normalizedMax = -Infinity;
    for (const cellData of allCellsData) {
        const normalizedCount = cellData[3];
        if (normalizedCount > 0) {
            normalizedMin = Math.min(normalizedMin, normalizedCount);
            normalizedMax = Math.max(normalizedMax, normalizedCount);
        }
    }
    if (normalizedMin === Infinity) normalizedMin = 0;
    if (normalizedMax === -Infinity) normalizedMax = 1;

    // Use log scale for color mapping
    const logMin = Math.log1p(normalizedMin);
    const logMax = Math.log1p(normalizedMax);
    const logRange = logMax - logMin || 1;

    // Configure tooltip with new styling
    const tooltipFormatter = function (params) {
        if (!params.data) {
            return '';
        }
        const [timestampMs, lowerBound, upperBound, count] = params.data;
        const formattedTime = formatDateTime(timestampMs);
        const bucketLabel = formatLatencyBucket(lowerBound);

        return `<div style="font-family: 'Inter', -apple-system, sans-serif;">
            <div style="font-family: 'JetBrains Mono', monospace; font-size: 11px; color: ${COLORS.fgSecondary}; margin-bottom: 8px;">
                ${formattedTime}
            </div>
            <div style="display: flex; align-items: center; gap: 12px;">
                <span style="background: ${COLORS.accentSubtle}; padding: 3px 8px; border-radius: 4px; font-family: 'JetBrains Mono', monospace; font-size: 11px; color: ${COLORS.accent};">
                    ${bucketLabel}
                </span>
                <span style="font-family: 'JetBrains Mono', monospace; font-weight: 600; font-size: 12px; color: ${COLORS.fg};">
                    Count: ${Math.round(count)}
                </span>
            </div>
        </div>`;
    };

    // Custom render function for heatmap cells with proper log-scale sizing
    const renderItem = function (params, api) {
        const timestampMs = api.value(0);
        const lowerBound = api.value(1);
        const upperBound = api.value(2);
        const count = api.value(3);

        const coordSys = params.coordSys;
        const gridX = coordSys.x;
        const gridY = coordSys.y;
        const gridWidth = coordSys.width;
        const gridHeight = coordSys.height;

        const cellStartMs = timestampMs - timeIntervalMs;
        const cellEndMs = timestampMs;

        const xStartCoord = api.coord([cellStartMs, lowerBound]);
        const xEndCoord = api.coord([cellEndMs, upperBound]);
        if (!xStartCoord || !xEndCoord) return;

        const overlap = 1;
        let x = xStartCoord[0];
        let y = xEndCoord[1] - overlap;
        let width = xEndCoord[0] - xStartCoord[0] + overlap;
        let height = xStartCoord[1] - xEndCoord[1] + overlap * 2;

        if (width <= 0 || height <= 0) return;

        // Clip to grid boundaries
        if (x < gridX) {
            width -= (gridX - x);
            x = gridX;
        }
        if (x + width > gridX + gridWidth) {
            width = gridX + gridWidth - x;
        }
        if (y < gridY) {
            height -= (gridY - y);
            y = gridY;
        }
        if (y + height > gridY + gridHeight) {
            height = gridY + gridHeight - y;
        }
        if (width <= 0 || height <= 0) return;

        // Map count to color using log scale
        const logCount = Math.log1p(count);
        const normalizedValue = Math.min(1, Math.max(0, (logCount - logMin) / logRange));
        const color = heatmapColor(normalizedValue);

        return {
            type: 'rect',
            shape: {
                x: x,
                y: y,
                width: width,
                height: height
            },
            style: {
                fill: color,
                stroke: null,
                lineWidth: 0
            }
        };
    };

    // Calculate minimum zoom span
    const sampleInterval = timeData.length > 1 ? timeData[1] - timeData[0] : 1;
    const totalDuration = timeData[timeData.length - 1] - timeData[0];
    const minZoomSpan = Math.max(0.1, (sampleInterval * 5 / totalDuration) * 100);

    const option = {
        ...baseOption,
        grid: {
            left: '80',
            right: '24',
            top: '50',
            bottom: '35',
            containLabel: false,
        },
        dataZoom: [{
            type: 'inside',
            xAxisIndex: 0,
            minSpan: minZoomSpan,
            filterMode: 'none',
        }, {
            type: 'slider',
            show: false,
            xAxisIndex: 0,
            minSpan: minZoomSpan,
            filterMode: 'none',
        }],
        xAxis: {
            type: 'time',
            min: 'dataMin',
            max: 'dataMax',
            splitNumber: 5,
            axisLine: { show: false },
            axisTick: { show: false },
            axisLabel: {
                color: COLORS.fgSecondary,
                fontSize: 10,
                fontFamily: '"JetBrains Mono", "SF Mono", monospace',
                formatter: {
                    year: '{yyyy}',
                    month: '{MMM}',
                    day: '{d}',
                    hour: '{HH}:{mm}',
                    minute: '{HH}:{mm}',
                    second: '{HH}:{mm}:{ss}',
                    millisecond: '{hh}:{mm}:{ss}.{SSS}',
                    none: '{hh}:{mm}:{ss}.{SSS}'
                }
            },
            splitLine: {
                show: true,
                lineStyle: {
                    color: COLORS.gridLine,
                    type: 'dashed',
                }
            },
        },
        yAxis: {
            type: 'log',
            min: minBucketValue,
            max: maxBucketValue,
            axisLine: { show: false },
            axisTick: { show: false },
            axisLabel: {
                color: COLORS.fgSecondary,
                fontSize: 10,
                fontFamily: '"JetBrains Mono", "SF Mono", monospace',
                formatter: (value) => formatLatencyBucket(value),
            },
            splitLine: {
                show: false,
            },
        },
        tooltip: {
            ...baseOption.tooltip,
            trigger: 'item',
            position: 'top',
            formatter: tooltipFormatter,
        },
        series: [{
            name: opts.title,
            type: 'custom',
            renderItem: renderItem,
            encode: {
                x: 0,
                y: [1, 2],
            },
            data: allCellsData,
            clip: true,
            animation: false,
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
}
