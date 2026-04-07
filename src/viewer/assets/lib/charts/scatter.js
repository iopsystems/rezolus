// scatter.js - Scatter chart configuration with fixed time axis handling

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
    COLORS,
    FONTS,
} from './base.js';
import { SCATTER_PALETTE } from './util/colormap.js';

/**
 * Configures the Chart based on Chart.spec
 * Responsible for calling setOption on the echart instance, and for setting up any
 * chart-specific dynamic behavior.
 * @param {import('./chart.js').Chart} chart - the chart to configure
 */
export function configureScatterChart(chart) {
    const {
        data,
        opts
    } = chart.spec;

    if (!data || data.length < 2 || !data[0] || data[0].length === 0) {
        applyNoData(chart);
        return;
    }

    const baseOption = getBaseOption();

    // Access format properties using snake_case naming to match Rust serialization
    const format = opts.format || {};
    const unitSystem = format.unit_system;
    const logScale = format.log_scale;
    const range = format.range;

    // For percentile data, the format is [times, percentile1Values, percentile2Values, ...]
    const timeData = data[0];

    // Create series for each percentile
    const series = [];

    const percentileLabels = format.percentile_labels || ['p50', 'p90', 'p99', 'p99.9', 'p99.99'];

    const scatterColors = SCATTER_PALETTE;

    let hasClamped = false;

    for (let i = 1; i < data.length; i++) {
        const percentileValues = data[i];
        const percentileData = [];
        const lineOnlyData = [];  // line breaks at clamped points

        for (let j = 0; j < timeData.length; j++) {
            const rawInput = percentileValues[j];
            if (rawInput !== undefined && !isNaN(rawInput)) {
                const [v, raw] = clampToRange(rawInput, range);
                const isClamped = raw != null;
                if (isClamped) {
                    hasClamped = true;
                    // y-value will be repositioned to OOB band after the loop
                    percentileData.push({ value: [timeData[j] * 1000, v, raw], itemStyle: { color: COLORS.clamped } });
                    lineOnlyData.push([timeData[j] * 1000, null]);
                } else {
                    percentileData.push([timeData[j] * 1000, v, null]);
                    lineOnlyData.push([timeData[j] * 1000, v, null]);
                }
            }
        }

        const color = scatterColors[(i - 1) % scatterColors.length];
        const name = percentileLabels[i - 1] || `Percentile ${i}`;

        // Line series underneath for visual continuity (with gap breaks)
        // Uses lineOnlyData which has nulls at clamped points so the line breaks there
        const lineData = insertGapNulls(lineOnlyData, chart.interval);
        series.push({
            name: name,
            type: 'line',
            data: lineData,
            showSymbol: false,
            lineStyle: {
                color: color,
                width: 1.5,
                opacity: 0.4,
            },
            itemStyle: { color: color },
            tooltip: { show: false },
            z: 1,
            animation: false,
        });

        // Scatter series on top
        series.push({
            name: name,
            type: 'scatter',
            data: percentileData,
            symbolSize: 3,
            itemStyle: {
                color: color,
            },
            z: 2,
            emphasis: {
                focus: 'series',
                scale: false,
                itemStyle: {
                    shadowBlur: 0,
                    shadowColor: 'transparent',
                    borderWidth: 0,
                }
            }
        });
    }

    // OOB band: move clamped dots above the main chart area
    chart._oobAxisMax = null;
    if (hasClamped && range && range.max != null) {
        let oobCenter, oobMax;
        if (logScale) {
            const logMax = Math.log10(range.max);
            oobCenter = Math.pow(10, logMax + 0.15);
            oobMax = Math.pow(10, logMax + 0.3);
        } else {
            const span = range.max - (range.min || 0);
            const band = Math.max(span * 0.05, 1e-9);
            oobCenter = range.max + band * 0.5;
            oobMax = range.max + band;
        }

        // Reposition clamped dots to OOB center
        for (const s of series) {
            if (s.type !== 'scatter') continue;
            for (const item of s.data) {
                if (item && item.value && item.value[2] != null) {
                    item.value[1] = oobCenter;
                }
            }
        }
        chart._oobAxisMax = oobMax;
    }

    // Ensure pinnedSet exists early so the tooltip formatter can reference it
    if (!chart.pinnedSet) {
        chart.pinnedSet = new Set();
    }

    const minZoomSpan = calculateMinZoomSpan(timeData);

    const uniqueNamesForLayout = [...new Set(series.map(s => s.name))];
    const hasTwoRowLegend = uniqueNamesForLayout.some(n => n.includes('.'));

    // Build yAxis config — when OOB is active, skip the last label at range.max
    const baseYAxis = getBaseYAxisOption(logScale, unitSystem);
    let yAxisConfig;
    if (chart._oobAxisMax) {
        const baseFormatter = baseYAxis.axisLabel.formatter;
        const rangeMax = range.max;
        yAxisConfig = {
            ...baseYAxis,
            max: chart._oobAxisMax,
            axisLabel: {
                ...baseYAxis.axisLabel,
                rich: { oob: { color: 'rgba(248, 81, 73, 0.65)' } },
                formatter: function (value) {
                    // Replace the tick at range.max with "OOB" to label the band
                    if (rangeMax != null && Math.abs(value - rangeMax) / rangeMax < 0.01) return '{oob|OOB}';
                    return typeof baseFormatter === 'function' ? baseFormatter(value) : value;
                },
            },
        };
    } else {
        yAxisConfig = baseYAxis;
    }

    // Build legend config — will be adjusted for narrow charts after first render
    const legendItemW = 56;
    const legendShared = {
        show: true,
        right: '16',
        icon: 'circle',
        itemWidth: 8,
        itemHeight: 8,
        itemGap: 0,
        tooltip: {
            show: true,
            formatter: () => 'Click to pin, ⌘/Ctrl+click to multi-select',
        },
        formatter: (name) => name.padEnd(6),
        textStyle: {
            color: COLORS.fgSecondary,
            ...FONTS.legend,
            width: legendItemW,
            borderColor: 'transparent',
            borderWidth: 1,
            borderRadius: 3,
            padding: [2, 4],
        },
    };
    const legendInitData = (names) => names.map(name => ({
        name,
        itemStyle: { borderColor: 'transparent', borderWidth: 2 },
        textStyle: {
            color: COLORS.fgSecondary,
            backgroundColor: 'transparent',
            borderColor: 'transparent',
            borderWidth: 1,
            borderRadius: 3,
            padding: [2, 4],
            width: legendItemW,
        },
    }));
    const legendRow1 = uniqueNamesForLayout.filter(n => !n.includes('.'));
    const legendRow2 = uniqueNamesForLayout.filter(n => n.includes('.'));

    const option = {
        ...baseOption,
        grid: { ...baseOption.grid, top: hasTwoRowLegend ? '68' : '60' },
        legend: (() => {
            if (legendRow2.length === 0) {
                return { ...legendShared, top: '10', data: legendInitData(legendRow1) };
            }
            return [
                { ...legendShared, top: '4', data: legendInitData(legendRow1) },
                { ...legendShared, top: '20', data: legendInitData(legendRow2) },
            ];
        })(),
        dataZoom: getDataZoomConfig(minZoomSpan),
        yAxis: yAxisConfig,
        tooltip: {
            ...baseOption.tooltip,
            formatter: getTooltipFormatter(unitSystem ?
                createAxisLabelFormatter(unitSystem) :
                val => val, chart.pinnedSet, chart, 'scatter'),
        },
        series: series,
        color: scatterColors,
    };

    // Add OOB band visual separator and background
    if (hasClamped && range && range.max != null) {
        const firstScatter = option.series.find(s => s.type === 'scatter');
        if (firstScatter) {
            firstScatter.markLine = {
                silent: true,
                symbol: 'none',
                data: [{ yAxis: range.max }],
                lineStyle: { color: COLORS.fgMuted, type: 'dashed', width: 1 },
                label: { show: false },
            };
            firstScatter.markArea = {
                silent: true,
                data: [[{ yAxis: range.max }, { yAxis: chart._oobAxisMax }]],
                itemStyle: { color: 'rgba(248, 81, 73, 0.06)' },
                label: { show: false },
            };
        }
    }

    applyChartOption(chart, option);

    // Narrow charts: move legend below the title/description instead of beside it
    const NARROW_THRESHOLD = 480;
    const chartWidth = chart.echart.getWidth();
    if (chartWidth && chartWidth < NARROW_THRESHOLD) {
        // Legend drops below header; push grid down to make room
        const legendTop = hasTwoRowLegend ? '38' : '34';
        const legendTop2 = '54';
        const gridTop = hasTwoRowLegend ? '86' : '76';
        const narrowLegend = legendRow2.length === 0
            ? { ...legendShared, top: legendTop, right: '16', data: legendInitData(legendRow1) }
            : [
                { ...legendShared, top: legendTop, right: '16', data: legendInitData(legendRow1) },
                { ...legendShared, top: legendTop2, right: '16', data: legendInitData(legendRow2) },
            ];
        chart.echart.setOption({ legend: narrowLegend, grid: { top: gridTop } });
    }

    // Pin feature: click a legend item to keep it highlighted.
    // Ctrl/Cmd+click to multi-select. Click again to unpin.
    // Uses persistent series style changes (not transient highlight/downplay)
    // so that ECharts' legend hover doesn't override the pin state.
    const uniqueNames = [...new Set(series.map(s => s.name))];
    const colorByName = {};
    for (let i = 1; i < data.length; i++) {
        const name = percentileLabels[i - 1] || `Percentile ${i}`;
        colorByName[name] = scatterColors[(i - 1) % scatterColors.length];
    }

    // Track Ctrl/Cmd key state — legendselectchanged doesn't carry the mouse event
    let ctrlHeld = false;
    const onKeyDown = (e) => { if (e.ctrlKey || e.metaKey) ctrlHeld = true; };
    const onKeyUp = (e) => { if (!e.ctrlKey && !e.metaKey) ctrlHeld = false; };
    // Also detect modifier from mousedown on the chart container (more reliable
    // than keydown for cases where the chart canvas already has focus)
    const onMouseDown = (e) => { ctrlHeld = e.ctrlKey || e.metaKey; };

    // Clean up previous listeners if chart is being reconfigured
    if (chart._pinCleanup) chart._pinCleanup();
    document.addEventListener('keydown', onKeyDown);
    document.addEventListener('keyup', onKeyUp);
    chart.domNode.addEventListener('mousedown', onMouseDown, true);
    chart._pinCleanup = () => {
        document.removeEventListener('keydown', onKeyDown);
        document.removeEventListener('keyup', onKeyUp);
        chart.domNode.removeEventListener('mousedown', onMouseDown, true);
    };

    const applyPinState = () => {
        const pinned = chart.pinnedSet;
        const hasPins = pinned.size > 0;

        // Update series opacity and disable emphasis when pinned so that
        // ECharts' built-in legend hover doesn't override the pin styling.
        const updatedSeries = series.map(s => {
            const isFaded = hasPins && !pinned.has(s.name);
            const color = colorByName[s.name];
            if (s.type === 'line') {
                return {
                    lineStyle: { color, width: 1.5, opacity: isFaded ? 0.1 : 0.4 },
                    itemStyle: { color, opacity: isFaded ? 0.1 : 1 },
                    emphasis: { disabled: hasPins },
                };
            } else {
                return {
                    itemStyle: { color, opacity: isFaded ? 0.15 : 1 },
                    emphasis: hasPins
                        ? { disabled: true }
                        : { disabled: false, focus: 'series', scale: false, itemStyle: { shadowBlur: 0, shadowColor: 'transparent', borderWidth: 0 } },
                };
            }
        });

        // Update legend items to indicate pinned state:
        // bordered swatch + background highlight on text.
        // Padding is always the same so layout doesn't shift on pin/unpin.
        const makeLegendData = (names) => names.map(name => {
            const isPinned = pinned.has(name);
            const color = colorByName[name];
            return {
                name,
                itemStyle: {
                    borderColor: isPinned ? color : 'transparent',
                    borderWidth: 2,
                },
                textStyle: {
                    color: isPinned ? COLORS.fg : COLORS.fgSecondary,
                    backgroundColor: isPinned ? (color + '26') : 'transparent', // 15% opacity via hex alpha
                    borderColor: isPinned ? color : 'transparent',
                    borderWidth: 1,
                    borderRadius: 3,
                    padding: [2, 4],
                    width: 56,
                },
            };
        });

        const row1 = uniqueNames.filter(n => !n.includes('.'));
        const row2 = uniqueNames.filter(n => n.includes('.'));
        const legendUpdate = row2.length === 0
            ? { data: makeLegendData(row1) }
            : [{ data: makeLegendData(row1) }, { data: makeLegendData(row2) }];

        chart.echart.setOption({ series: updatedSeries, legend: legendUpdate });
    };

    // Remove any previously stacked handler before adding a new one
    chart.echart.off('legendselectchanged');
    chart.echart.on('legendselectchanged', (params) => {
        // Undo the default toggle — re-select all series
        const selected = {};
        uniqueNames.forEach(name => { selected[name] = true; });
        if (Array.isArray(option.legend)) {
            chart.echart.setOption({ legend: option.legend.map(() => ({ selected })) });
        } else {
            chart.echart.setOption({ legend: { selected } });
        }

        const name = params.name;
        if (ctrlHeld) {
            // Ctrl/Cmd+click: toggle this series in the multi-select set
            if (chart.pinnedSet.has(name)) {
                chart.pinnedSet.delete(name);
            } else {
                chart.pinnedSet.add(name);
            }
        } else {
            // Plain click: solo toggle (clear others)
            if (chart.pinnedSet.size === 1 && chart.pinnedSet.has(name)) {
                chart.pinnedSet.clear();
            } else {
                chart.pinnedSet.clear();
                chart.pinnedSet.add(name);
            }
        }
        applyPinState();
    });

    // Restore pin state if chart was reconfigured (e.g., data update)
    if (chart.pinnedSet.size > 0) {
        applyPinState();
    }
}
