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
    buildOverlayLegendOption,
    CHART_GRID_TOP_WITH_LEGEND,
    HISTOGRAM_CHART_GRID_LEFT,
    COLORS,
    FONTS,
} from './base.js';
import { SCATTER_PALETTE } from './util/colormap.js';
import { DEFAULT_PERCENTILES } from './metric_types.js';
import { configureQuantileHeatmap } from './quantile_heatmap.js';
import { fetchQuantileSpectrumForPlot } from '../data.js';
import { quantilesForKind } from './util/spectrum_quantiles.js';

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

    // Invalidate the cached spectrum data when the underlying scatter
    // data swaps to a different reference (e.g. parent refetch). The
    // checkbox state itself is preserved so the user's choice persists
    // across refreshes; the next render in spectrum mode will refetch.
    if (chart._spectrumDataSourceRef !== data) {
        chart._spectrumDataByKind = null;
        chart._spectrumDataSourceRef = data;
    }

    // Spectrum mode (`full` or `tail`): render as a quantile heatmap
    // using the cached result for that kind. If we don't have the data
    // yet, fall through and render the normal scatter while we kick
    // off the fetch in the background; once it lands, re-trigger.
    if (chart.spectrumKind) {
        const cache = chart._spectrumDataByKind?.[chart.spectrumKind];
        if (cache) {
            const originalSpec = chart.spec;
            chart.spec = {
                ...originalSpec,
                data: cache.data,
                series_names: cache.seriesNames,
                // Forward the p0 anchor so the quantile-heatmap color
                // scale starts at p0 (full and tail share the bound).
                color_min_anchor: cache.colorMinAnchor,
            };
            try {
                configureQuantileHeatmap(chart);
            } finally {
                chart.spec = originalSpec;
            }
            ensureSpectrumCheckboxes(chart);
            return;
        }
        kickOffSpectrumFetch(chart, chart.spectrumKind);
        // fall through to normal scatter render while we wait
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

    // Derive labels from query result series names (set by the PromQL engine's
    // percentile label), falling back to opts.percentiles for pre-data render.
    const percentileLabels = (chart.spec.series_names && chart.spec.series_names.length > 0)
        ? chart.spec.series_names.map(v => `p${parseFloat(v) * 100}`)
        : (opts.percentiles || DEFAULT_PERCENTILES).map(v => `p${v * 100}`);

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
        // Lower quantiles (earlier in array) get higher z so they draw on top
        // when values overlap with higher quantiles.
        const lineZ = (data.length - i) * 2;
        const scatterZ = lineZ + 1;
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
            z: lineZ,
            animation: false,
        });

        // Scatter series on top of its own line
        series.push({
            name: name,
            type: 'scatter',
            data: percentileData,
            symbolSize: 3,
            itemStyle: {
                color: color,
            },
            z: scatterZ,
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

    // `chart.pinnedSet` is the per-chart Set<string> of pinned series
    // labels. Pins are intentionally local: clicking p99 on one scatter
    // does not affect p99 on a sibling scatter. `resetAll` clears this
    // via chart._clearPins below.
    if (!chart.pinnedSet) {
        chart.pinnedSet = new Set();
    }

    // Store labels so _rescaleYAxis can map data indices to pinned names
    chart._seriesLabels = percentileLabels;

    const minZoomSpan = calculateMinZoomSpan(timeData);

    const uniqueNamesForLayout = [...new Set(series.map(s => s.name))];

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

    const option = {
        ...baseOption,
        grid: {
            ...baseOption.grid,
            top: String(CHART_GRID_TOP_WITH_LEGEND),
            // Pin the left gutter so toggling between scatter and the
            // quantile-heatmap variants doesn't shift the y-axis.
            left: HISTOGRAM_CHART_GRID_LEFT,
            containLabel: false,
        },
        legend: buildOverlayLegendOption(uniqueNamesForLayout, {
            tooltipFormatter: () => 'Click to pin, ⌘/Ctrl+click to multi-select',
            // Nudge the percentile legend ~2 character widths rightward
            // so it sits closer to the chart's right edge.
            right: '0',
        }),
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

    // Drop pins that no longer correspond to a rendered series (e.g.
    // after a reconfigure with a different percentile set).
    for (const name of chart.pinnedSet) {
        if (!uniqueNames.includes(name)) chart.pinnedSet.delete(name);
    }

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

        const legendUpdate = { data: makeLegendData(uniqueNames) };

        chart.echart.setOption({ series: updatedSeries, legend: legendUpdate });
    };

    // Remove any previously stacked handler before adding a new one
    chart.echart.off('legendselectchanged');
    chart.echart.on('legendselectchanged', (params) => {
        // Undo the default toggle — re-select all series
        const selected = {};
        uniqueNames.forEach(name => { selected[name] = true; });
        chart.echart.setOption({ legend: { selected } });

        // Local-only pin update: mutate chart.pinnedSet directly and
        // repaint. No fan-out to sibling charts.
        const name = params.name;
        if (ctrlHeld) {
            // Ctrl/Cmd+click: toggle this series in the multi-select set
            if (chart.pinnedSet.has(name)) chart.pinnedSet.delete(name);
            else chart.pinnedSet.add(name);
        } else {
            // Plain click: solo toggle (clear others and pin this one,
            // or unpin if it was already the lone pinned label).
            if (chart.pinnedSet.size === 1 && chart.pinnedSet.has(name)) {
                chart.pinnedSet.clear();
            } else {
                chart.pinnedSet.clear();
                chart.pinnedSet.add(name);
            }
        }
        applyPinState();
        chart._rescaleYAxis();
    });

    // resetAll() hook: chartsState.resetAll iterates charts and calls
    // this to clear per-chart pin state. We repaint here so the legend
    // + series styling drop back to the unpinned look immediately.
    chart._clearPins = () => {
        if (chart.pinnedSet.size === 0) return;
        chart.pinnedSet.clear();
        applyPinState();
        chart._rescaleYAxis();
    };

    // Redraw if any pins carried over from a previous configure.
    if (chart.pinnedSet.size > 0) {
        applyPinState();
    }

    ensureSpectrumCheckboxes(chart);
}

// ── Spectrum toggles ─────────────────────────────────────────────────
// Two mutually-exclusive checkboxes ("Full" and "Tail") let the user
// promote a percentile scatter into a quantile-heatmap view. State:
//   chart.spectrumKind: null | 'full' | 'tail'
//   chart._spectrumPending: null | 'full' | 'tail' (in-flight fetch)
//   chart._spectrumDataByKind: { full?, tail? } — cached fetch results,
//     invalidated on the next configure when the source data ref swaps.
// The DOM lives in chart.domNode (.chart already has position:relative)
// so it survives reconfigures and rides along with the chart container.

const SPECTRUM_CONTROLS_CLASS = 'spectrum-controls';
const SPECTRUM_CHECKBOX_CLASS = 'spectrum-toggle';
const SPECTRUM_TAIL_CHECKBOX_CLASS = 'spectrum-tail-toggle';

const SPECTRUM_LABELS = { full: 'Full', tail: 'Tail' };

function renderSpectrumCheckbox(el, chart, kind) {
    const on = chart.spectrumKind === kind;
    const pending = chart._spectrumPending === kind;
    // fgSecondary (--fg-secondary) reads brighter than fgMuted in dark
    // mode; the checkbox + label otherwise get lost against the chart bg.
    const color = on ? COLORS.fg : COLORS.fgSecondary;
    const glyph = on ? '☑' : '☐';
    const label = pending ? `${SPECTRUM_LABELS[kind]}…` : SPECTRUM_LABELS[kind];
    el.innerHTML =
        `<span style="font-size: 16px; vertical-align: bottom; position: relative; top: 2px;">${glyph}</span> ${label}`;
    el.style.color = color;
}

function refreshSpectrumCheckboxes(chart) {
    const container = chart.domNode.querySelector('.' + SPECTRUM_CONTROLS_CLASS);
    if (!container) return;
    const fullEl = container.querySelector('.' + SPECTRUM_CHECKBOX_CLASS);
    const tailEl = container.querySelector('.' + SPECTRUM_TAIL_CHECKBOX_CLASS);
    if (fullEl) renderSpectrumCheckbox(fullEl, chart, 'full');
    if (tailEl) renderSpectrumCheckbox(tailEl, chart, 'tail');
}

function toggleSpectrum(chart, kind) {
    chart.spectrumKind = (chart.spectrumKind === kind) ? null : kind;
    refreshSpectrumCheckboxes(chart);
    chart.configureChartByType();
}

// Position the spectrum controls inside chart.domNode so they:
//   - hug the chart grid's left edge (so they line up with the inner
//     plot canvas, not the y-axis label gutter)
//   - sit on the same vertical row as the right-anchored percentile
//     legend on wide charts, OR move up to the empty area above the
//     legend on narrow charts so they don't crowd the legend on
//     mobile / narrow viewport widths.
// The grid rect is only available after echarts has laid out the
// chart, so we query it via the `finished` event and reposition.
const SPECTRUM_CONTROLS_NARROW_WIDTH = 500;
function positionControlsAtGridLeft(chart, container) {
    if (!chart.echart) return;
    try {
        const grid = chart.echart.getModel().getComponent('grid');
        const rect = grid?.coordinateSystem?.getRect();
        const chartWidth = chart.echart.getDom?.()?.clientWidth ?? 0;
        const narrow = chartWidth > 0 && chartWidth < SPECTRUM_CONTROLS_NARROW_WIDTH;
        if (narrow) {
            // Narrow chart: stack above the legend on its own line and
            // align to the right edge so it visually anchors to the
            // legend's column rather than floating in the left gutter.
            container.style.top = '28px';
            container.style.left = 'auto';
            container.style.right = '12px';
        } else {
            container.style.top = '42px';
            container.style.right = 'auto';
            if (rect && Number.isFinite(rect.x)) {
                container.style.left = Math.round(rect.x) + 'px';
            }
        }
    } catch (_e) {
        // Layout not ready yet — next 'finished' event will retry.
    }
}

function ensureSpectrumCheckboxes(chart) {
    let container = chart.domNode.querySelector('.' + SPECTRUM_CONTROLS_CLASS);
    let fullEl, tailEl;
    if (!container) {
        container = document.createElement('span');
        container.className = SPECTRUM_CONTROLS_CLASS;
        // Sit on the same vertical row as the percentile legend
        // (top:42 matches buildOverlayLegendOption's default). The
        // left offset is filled in by positionControlsAtGridLeft once
        // the grid coordinate system is ready.
        container.style.cssText = `
            position: absolute;
            top: 42px;
            left: 0px;
            z-index: 10;
            display: inline-flex;
            gap: 12px;
            ${FONTS.cssControl}
            user-select: none;
        `;
        fullEl = document.createElement('span');
        fullEl.className = SPECTRUM_CHECKBOX_CLASS;
        fullEl.style.cursor = 'pointer';
        tailEl = document.createElement('span');
        tailEl.className = SPECTRUM_TAIL_CHECKBOX_CLASS;
        tailEl.style.cursor = 'pointer';
        container.appendChild(fullEl);
        container.appendChild(tailEl);
        chart.domNode.appendChild(container);
    } else {
        fullEl = container.querySelector('.' + SPECTRUM_CHECKBOX_CLASS);
        tailEl = container.querySelector('.' + SPECTRUM_TAIL_CHECKBOX_CLASS);
    }

    fullEl.onclick = () => toggleSpectrum(chart, 'full');
    tailEl.onclick = () => toggleSpectrum(chart, 'tail');
    renderSpectrumCheckbox(fullEl, chart, 'full');
    renderSpectrumCheckbox(tailEl, chart, 'tail');

    // Reposition on every render (initial layout, theme swap, resize,
    // zoom-driven re-layout). Replace any previous listener bound to a
    // stale closure so we don't stack handlers across reconfigures.
    if (chart.echart) {
        if (chart._spectrumFinishedFn) {
            chart.echart.off('finished', chart._spectrumFinishedFn);
        }
        chart._spectrumFinishedFn = () => positionControlsAtGridLeft(chart, container);
        chart.echart.on('finished', chart._spectrumFinishedFn);
        positionControlsAtGridLeft(chart, container);
    }
}

function kickOffSpectrumFetch(chart, kind) {
    if (chart._spectrumPending === kind) return;
    const plotForFetch = {
        promql_query: chart.spec.promql_query,
        opts: chart.spec.opts,
    };
    if (!plotForFetch.promql_query) return;

    chart._spectrumPending = kind;
    refreshSpectrumCheckboxes(chart);

    fetchQuantileSpectrumForPlot(plotForFetch, quantilesForKind(kind))
        .then((res) => {
            if (chart._spectrumPending === kind) chart._spectrumPending = null;
            if (!res) {
                // Fetch returned no usable data — bail back to scatter
                // (only if the user hasn't switched to a different kind
                // in the meantime).
                if (chart.spectrumKind === kind) chart.spectrumKind = null;
                refreshSpectrumCheckboxes(chart);
                return;
            }
            chart._spectrumDataByKind = chart._spectrumDataByKind || {};
            chart._spectrumDataByKind[kind] = {
                data: res.data,
                seriesNames: res.series_names,
                colorMinAnchor: res.color_min_anchor,
            };
            if (chart.spectrumKind === kind) chart.configureChartByType();
            else refreshSpectrumCheckboxes(chart);
        })
        .catch((err) => {
            if (chart._spectrumPending === kind) chart._spectrumPending = null;
            if (chart.spectrumKind === kind) chart.spectrumKind = null;
            refreshSpectrumCheckboxes(chart);
            console.error('Failed to fetch quantile spectrum:', err);
        });
}

