// histogram_heatmap.js - Histogram heatmap chart for latency distributions

import {
    formatDateTime
} from './util/utils.js';
import {
    createAxisLabelFormatter,
} from './util/units.js';
import {
    getBaseOption,
    applyNoData,
    getTooltipFreezeFooter,
    calculateMinZoomSpan,
    getDataZoomConfig,
    applyChartOption,
    TIME_AXIS_FORMATTER,
    COLORS,
    FONTS,
} from './base.js';
import { infernoColor } from './util/colormap.js';

// Reuse the shared time formatter for latency bucket labels
const formatLatencyBucket = createAxisLabelFormatter('time');

/**
 * Build the gradient bar canvas for the heatmap legend (ECharts graphic).
 * Labels are rendered as DOM elements so they can update without triggering a canvas redraw.
 * @param {function} colorFn - maps 0..1 to an RGB color string
 * @returns {Object} echarts graphic config (bar image only)
 */
function buildHeatmapGradientBar(colorFn) {
    const barWidth = 120;
    const barHeight = 10;

    const canvas = document.createElement('canvas');
    canvas.width = barWidth;
    canvas.height = barHeight;
    const ctx = canvas.getContext('2d');
    for (let x = 0; x < barWidth; x++) {
        ctx.fillStyle = colorFn(x / (barWidth - 1));
        ctx.fillRect(x, 0, 1, barHeight);
    }

    return {
        elements: [{
            type: 'image',
            id: 'heatmap-gradient-bar',
            right: 24,
            top: 34,
            style: {
                image: canvas,
                width: barWidth,
                height: barHeight,
            },
        }],
    };
}

/**
 * Create or update a DOM label element positioned over the chart.
 * @param {HTMLElement} container - the chart DOM node
 * @param {string} className - CSS class for querySelector
 * @param {string} rightPx - CSS right position
 * @returns {HTMLElement}
 */
function ensureDomLabel(container, className, rightPx) {
    let el = container.querySelector('.' + className);
    if (!el) {
        el = document.createElement('span');
        el.className = className;
        el.style.cssText = `
            position: absolute;
            top: 35px;
            right: ${rightPx};
            transform: translateX(50%);
            ${FONTS.cssFootnote}
            color: ${COLORS.fgLabel};
            z-index: 10;
            pointer-events: none;
        `;
        container.appendChild(el);
    }
    return el;
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
        opts
    } = chart.spec;

    if (!data || data.length === 0 || !timeData || timeData.length === 0) {
        applyNoData(chart);
        return;
    }

    const baseOption = getBaseOption();

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

    // Get the visible bucket bounds for log scale, snapped to powers of 10
    // so ECharts places ticks at clean decade boundaries (1ns, 10ns, 100ns, ...)
    const rawMinBucket = minBucketIdx > 0 ? Math.max(1, bucketBounds[minBucketIdx - 1]) : 1;
    const rawMaxBucket = bucketBounds[maxBucketIdx];
    const minBucketValue = Math.pow(10, Math.floor(Math.log10(rawMinBucket)));
    const maxBucketValue = Math.pow(10, Math.ceil(Math.log10(rawMaxBucket)));

    // Downsample along the time axis if there are too many data points.
    // Target roughly 500 time columns to keep rendering fast.
    const MAX_TIME_COLUMNS = 500;
    const bucketRange = maxBucketIdx - minBucketIdx + 1;
    const factor = timeData.length > MAX_TIME_COLUMNS
        ? Math.ceil(timeData.length / MAX_TIME_COLUMNS)
        : 1;

    // Build downsampled time array
    const dsTimeData = [];
    for (let t = 0; t < timeData.length; t += factor) {
        dsTimeData.push(timeData[t]);
    }

    // Calculate the time interval for cell width (in milliseconds)
    const timeIntervalMs = dsTimeData.length > 1
        ? (dsTimeData[1] - dsTimeData[0]) * 1000
        : (timeData.length > 1 ? (timeData[1] - timeData[0]) * 1000 : 1000);

    // Aggregate data: for each downsampled time group × bucket, take the max count
    // Use a flat array keyed by (dsTimeIdx * bucketRange + bucketOffset) for speed
    const dsTimeCount = dsTimeData.length;
    const aggMax = new Float64Array(dsTimeCount * bucketRange); // initialized to 0

    for (const [timeIdx, bucketIdx, count] of data) {
        if (bucketIdx < minBucketIdx || bucketIdx > maxBucketIdx || count === 0) {
            continue;
        }
        const dsTimeIdx = Math.floor(timeIdx / factor);
        const bucketOffset = bucketIdx - minBucketIdx;
        const key = dsTimeIdx * bucketRange + bucketOffset;
        if (count > aggMax[key]) {
            aggMax[key] = count;
        }
    }

    // Initialize display mode (default: percentage)
    if (chart.histogramDisplayMode === undefined) {
        chart.histogramDisplayMode = 'percentage';
    }
    // Compute total counts per downsampled time step (for percentage mode)
    const totalCountPerTime = new Float64Array(dsTimeCount);
    for (let dt = 0; dt < dsTimeCount; dt++) {
        for (let bo = 0; bo < bucketRange; bo++) {
            totalCountPerTime[dt] += aggMax[dt * bucketRange + bo];
        }
    }

    // Build the final cell data from the aggregated values
    const allCellsData = [];
    for (let dt = 0; dt < dsTimeCount; dt++) {
        const timestampMs = dsTimeData[dt] * 1000;
        for (let bo = 0; bo < bucketRange; bo++) {
            const count = aggMax[dt * bucketRange + bo];
            if (count === 0) continue;

            const bucketIdx = bo + minBucketIdx;
            const lowerBound = bucketIdx > 0 ? Math.max(1, bucketBounds[bucketIdx - 1]) : 1;
            const upperBound = bucketBounds[bucketIdx];

            allCellsData.push([timestampMs, Math.log10(lowerBound), Math.log10(upperBound), count, dt, bucketIdx]);
        }
    }

    // Calculate min/max values for color scaling
    let colorMin = Infinity;
    let colorMax = -Infinity;
    for (const cellData of allCellsData) {
        const val = cellData[3];
        if (val > 0) {
            colorMin = Math.min(colorMin, val);
            colorMax = Math.max(colorMax, val);
        }
    }
    if (colorMin === Infinity) colorMin = 0;
    if (colorMax === -Infinity) colorMax = 1;

    // Use log scale for color mapping
    const logMin = Math.log1p(colorMin);
    const logMax = Math.log1p(colorMax);
    const logRange = logMax - logMin || 1;

    // Tooltip reads display mode dynamically — no setOption needed on toggle
    const tooltipFormatter = function (params) {
        if (!params.data) return '';
        const [timestampMs, logLowerBound, , count, dt] = params.data;
        const formattedTime = formatDateTime(timestampMs);
        const bucketLabel = formatLatencyBucket(Math.pow(10, logLowerBound));
        let formattedValue;
        if (chart.histogramDisplayMode === 'raw') {
            formattedValue = createAxisLabelFormatter('count')(count);
        } else {
            const total = totalCountPerTime[dt];
            const pct = total > 0 ? (count / total) * 100 : 0;
            formattedValue = pct.toFixed(1) + '%';
        }

        return `<div style="${FONTS.cssSans}">
            <div style="${FONTS.cssMono} font-size: ${FONTS.tooltipTimestamp.fontSize}px; color: ${COLORS.fgSecondary}; margin-bottom: 8px;">
                ${formattedTime}
            </div>
            <div style="display: flex; align-items: center; gap: 12px;">
                <span style="background: ${COLORS.accentSubtle}; padding: 3px 8px; border-radius: 4px; ${FONTS.cssMono} font-size: ${FONTS.tooltipTimestamp.fontSize}px; color: ${COLORS.accent};">
                    ${bucketLabel}
                </span>
                <span style="${FONTS.cssMono} font-weight: ${FONTS.tooltipValue.fontWeight}; font-size: ${FONTS.tooltipValue.fontSize}px; color: ${COLORS.fg};">
                    ${formattedValue}
                </span>
            </div>
            ${getTooltipFreezeFooter(chart)}
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

        const cellStartMs = timestampMs;
        const cellEndMs = timestampMs + timeIntervalMs;

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
        const color = infernoColor(normalizedValue);

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

    const option = {
        ...baseOption,
        grid: {
            left: '12',
            right: '17',
            top: '76',
            bottom: '24',
            containLabel: true,
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
                formatter: TIME_AXIS_FORMATTER,
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
            type: 'value',
            min: Math.log10(minBucketValue),
            max: Math.log10(maxBucketValue),
            interval: 1, // guaranteed one tick per decade in log10 space
            axisLine: { show: false },
            axisTick: { show: false },
            axisLabel: {
                color: COLORS.fgSecondary,
                ...FONTS.axisLabel,
                formatter: (logVal) => formatLatencyBucket(Math.pow(10, logVal)),
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
            progressive: 5000,
            progressiveThreshold: 3000,
            animation: false,
        }],
        graphic: buildHeatmapGradientBar(infernoColor),
    };

    applyChartOption(chart, option);

    // Precompute percentage min/max for label display
    let pctMin = Infinity;
    let pctMax = -Infinity;
    for (const cellData of allCellsData) {
        const count = cellData[3];
        const dt = cellData[4];
        const total = totalCountPerTime[dt];
        if (total > 0 && count > 0) {
            const pct = (count / total) * 100;
            pctMin = Math.min(pctMin, pct);
            pctMax = Math.max(pctMax, pct);
        }
    }
    if (pctMin === Infinity) pctMin = 0;
    if (pctMax === -Infinity) pctMax = 100;

    const countFormatter = createAxisLabelFormatter('count');
    const rawMinLabel = countFormatter(colorMin);
    const rawMaxLabel = countFormatter(colorMax);
    const pctMinLabel = pctMin.toFixed(1) + '%';
    const pctMaxLabel = pctMax.toFixed(1) + '%';

    // DOM labels for gradient bar min/max — update without canvas redraw
    chart.domNode.style.position = 'relative';
    const minLabelEl = ensureDomLabel(chart.domNode, 'heatmap-label-min', '144px');
    const maxLabelEl = ensureDomLabel(chart.domNode, 'heatmap-label-max', '24px');
    if (isNarrow) {
        minLabelEl.style.top = '76px';
        maxLabelEl.style.top = '76px';
    }

    const updateLabels = () => {
        const isRaw = chart.histogramDisplayMode === 'raw';
        minLabelEl.textContent = isRaw ? rawMinLabel : pctMinLabel;
        maxLabelEl.textContent = isRaw ? rawMaxLabel : pctMaxLabel;
    };
    updateLabels();

    // DOM checkbox overlay for percentage/raw count toggle.
    // Lives in the chart-wrapper (parent of canvas) so it aligns with the DOM title row.
    const wrapper = chart.domNode.parentNode;
    let checkboxEl = wrapper.querySelector('.histogram-toggle');
    if (!checkboxEl) {
        checkboxEl = document.createElement('span');
        checkboxEl.className = 'histogram-toggle';
        wrapper.appendChild(checkboxEl);
    }
    checkboxEl.style.cssText = `
        position: absolute;
        top: ${isNarrow ? '52px' : '10px'};
        right: 180px;
        ${FONTS.cssControl}
        cursor: pointer;
        user-select: none;
        z-index: 3;
    `;

    const updateCheckbox = () => {
        const isRaw = chart.histogramDisplayMode === 'raw';
        const color = isRaw ? COLORS.fg : COLORS.fgLabel;
        checkboxEl.innerHTML =
            `<span style="font-size: 16px; vertical-align: bottom;">${isRaw ? '☑' : '☐'}</span> Raw count`;
        checkboxEl.style.color = color;
    };
    updateCheckbox();

    checkboxEl.onclick = () => {
        const isRaw = chart.histogramDisplayMode === 'raw';
        chart.histogramDisplayMode = isRaw ? 'percentage' : 'raw';
        updateCheckbox();
        updateLabels();
    };
}
