// scatter.js - Scatter chart configuration with fixed time axis handling

import {
    createAxisLabelFormatter,
} from './util/units.js';
import {
    insertGapNulls,
} from './util/utils.js';
import {
    getBaseOption,
    getBaseYAxisOption,
    getTooltipFormatter,
    getNoDataOption,
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
        chart.echart.setOption(getNoDataOption(opts.title, opts.description));
        return;
    }

    const baseOption = getBaseOption(opts.title, opts.description);

    // Access format properties using snake_case naming to match Rust serialization
    const format = opts.format || {};
    const unitSystem = format.unit_system;
    const logScale = format.log_scale;
    const minValue = format.min;
    const maxValue = format.max;

    // For percentile data, the format is [times, percentile1Values, percentile2Values, ...]
    const timeData = data[0];

    // Create series for each percentile
    const series = [];

    const percentileLabels = format.percentile_labels || ['p50', 'p90', 'p99', 'p99.9', 'p99.99'];

    const scatterColors = SCATTER_PALETTE;

    for (let i = 1; i < data.length; i++) {
        const percentileValues = data[i];
        const percentileData = [];

        // Create data points in the format [time, value, original_index]
        for (let j = 0; j < timeData.length; j++) {
            if (percentileValues[j] !== undefined && !isNaN(percentileValues[j])) {
                percentileData.push([timeData[j] * 1000, percentileValues[j], j]); // Store original index
            }
        }

        const color = scatterColors[(i - 1) % scatterColors.length];
        const name = percentileLabels[i - 1] || `Percentile ${i}`;

        // Line series underneath for visual continuity (with gap breaks)
        const lineData = insertGapNulls(percentileData, chart.interval);
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

    // Ensure pinnedSet exists early so the tooltip formatter can reference it
    if (!chart.pinnedSet) {
        chart.pinnedSet = new Set();
    }

    const minZoomSpan = calculateMinZoomSpan(timeData);

    const option = {
        ...baseOption,
        legend: (() => {
            const uniqueNames = [...new Set(series.map(s => s.name))];
            const row1 = uniqueNames.filter(n => !n.includes('.'));
            const row2 = uniqueNames.filter(n => n.includes('.'));
            // Fixed item width so swatches align across rows into a grid
            const itemW = 56;
            const shared = {
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
                formatter: (name) => {
                    // Pad names to equal width using monospace spaces
                    return name.padEnd(6);
                },
                textStyle: {
                    color: COLORS.fgSecondary,
                    ...FONTS.legend,
                    width: itemW,
                    borderColor: 'transparent',
                    borderWidth: 1,
                    borderRadius: 3,
                    padding: [2, 4],
                },
            };
            // Use rich data objects matching the exact shape applyPinState produces,
            // so the first click doesn't cause a layout recalculation.
            const initData = (names) => names.map(name => ({
                name,
                itemStyle: { borderColor: 'transparent', borderWidth: 2 },
                textStyle: {
                    color: COLORS.fgSecondary,
                    backgroundColor: 'transparent',
                    borderColor: 'transparent',
                    borderWidth: 1,
                    borderRadius: 3,
                    padding: [2, 4],
                    width: itemW,
                },
            }));
            if (row2.length === 0) {
                return { ...shared, top: '12', data: initData(row1) };
            }
            return [
                { ...shared, top: '4', data: initData(row1) },
                { ...shared, top: '20', data: initData(row2) },
            ];
        })(),
        dataZoom: getDataZoomConfig(minZoomSpan),
        yAxis: getBaseYAxisOption(logScale, minValue, maxValue, unitSystem),
        tooltip: {
            ...baseOption.tooltip,
            formatter: getTooltipFormatter(unitSystem ?
                createAxisLabelFormatter(unitSystem) :
                val => val, chart.pinnedSet, chart),
        },
        series: series,
        color: scatterColors,
    };

    applyChartOption(chart, option);

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
