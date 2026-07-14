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
import { buildBoxplotSeries } from './boxplot.js';
import { DEFAULT_PERCENTILES } from './metric_types.js';
import { configureQuantileHeatmap } from './quantile_heatmap.js';
import { fetchQuantileSpectrumForPlot } from '../data.js';
import { quantilesForKind } from './util/spectrum_quantiles.js';

// ── OOB (out-of-bounds / clamped) handling ───────────────────────────────────
// Percentile charts cap the y-axis at format.range.max and park values above it
// in a distinct "OOB" band so a spike doesn't blow the scale. Shared by both the
// scatter (dots) render and the decimated band render so neither drops clamped
// values.

// Geometry of the OOB band above range.max (where clamped values are parked).
function computeOobBand(range, logScale) {
    if (logScale) {
        const logMax = Math.log10(range.max);
        return { oobCenter: Math.pow(10, logMax + 0.15), oobMax: Math.pow(10, logMax + 0.3) };
    }
    const span = range.max - (range.min || 0);
    const band = Math.max(span * 0.05, 1e-9);
    return { oobCenter: range.max + band * 0.5, oobMax: range.max + band };
}

// yAxis config capped at the OOB band top, relabeling the range.max tick "OOB".
function oobYAxis(baseYAxis, oobMax, rangeMax) {
    const baseFormatter = baseYAxis.axisLabel.formatter;
    return {
        ...baseYAxis,
        max: oobMax,
        axisLabel: {
            ...baseYAxis.axisLabel,
            rich: { oob: { color: 'rgba(248, 81, 73, 0.65)' } },
            formatter: function (value) {
                if (rangeMax != null && Math.abs(value - rangeMax) / rangeMax < 0.01) return '{oob|OOB}';
                return typeof baseFormatter === 'function' ? baseFormatter(value) : value;
            },
        },
    };
}

// Dashed separator at range.max + shaded OOB band, to attach to a series.
function oobSeparator(rangeMax, oobMax) {
    return {
        markLine: {
            silent: true,
            symbol: 'none',
            data: [{ yAxis: rangeMax }],
            lineStyle: { color: COLORS.fgMuted, type: 'dashed', width: 1 },
            label: { show: false },
        },
        markArea: {
            silent: true,
            data: [[{ yAxis: rangeMax }, { yAxis: oobMax }]],
            itemStyle: { color: 'rgba(248, 81, 73, 0.06)' },
            label: { show: false },
        },
    };
}

// Render decimated percentile columns (chart.spec.boxplot) as one median line
// + min/max band per percentile. Used when the percentile data is a downsample
// (zoomed out); the min/max band carries the second-to-second spread the
// scatter would otherwise show as noise, and `max` on the tail percentiles
// preserves spikes.
function renderPercentileBands(chart) {
    const { opts } = chart.spec;
    const format = opts.format || {};
    const unitSystem = format.unit_system;
    const logScale = format.log_scale;
    const range = format.range;
    const cols = chart.spec.boxplot;

    const labels = (chart.spec.series_names && chart.spec.series_names.length > 0)
        ? chart.spec.series_names.map((v) => `p${parseFloat(v) * 100}`)
        : (opts.percentiles || DEFAULT_PERCENTILES).map((v) => `p${v * 100}`);

    // OOB handling (mirrors the scatter dots render): park band values above
    // range.max in the OOB band so a spike shows there instead of blowing the
    // y-axis. Build clamped COPIES — the source columns may be shared tile-cache
    // views and must not be mutated in place.
    chart._oobAxisMax = null;
    let renderCols = cols;
    let oob = null;
    if (range && range.max != null) {
        const overMax = (arr) => {
            for (let i = 0; i < arr.length; i++) if (arr[i] > range.max) return true;
            return false;
        };
        const hasClamped = cols.some((s) =>
            ['min', 'lo', 'median', 'hi', 'max'].some((c) => overMax(s[c])));
        if (hasClamped) {
            oob = computeOobBand(range, logScale);
            const place = (v) => {
                if (v == null || Number.isNaN(v)) return v;
                if (v > range.max) return oob.oobCenter;
                if (range.min != null && v < range.min) return range.min;
                return v;
            };
            renderCols = cols.map((s) => {
                const out = { ...s };
                for (const c of ['min', 'lo', 'median', 'hi', 'max']) {
                    const src = s[c];
                    const dst = new Float64Array(src.length);
                    for (let i = 0; i < src.length; i++) dst[i] = place(src[i]);
                    out[c] = dst;
                }
                return out;
            });
            chart._oobAxisMax = oob.oobMax;
        }
    }

    // Stack lower quantiles on top (matching the scatter-dots convention) so a
    // low percentile stays visible where it meets a higher one: cols are
    // ascending, so index 0 gets the highest zBase. Stride by 4 (a series spans
    // zBase+1..+3).
    const echartsSeries = renderCols.flatMap((s, i) => buildBoxplotSeries(s, {
        name: labels[i] || `p${i + 1}`,
        stackId: `pct${i}`,
        lineColor: SCATTER_PALETTE[i % SCATTER_PALETTE.length],
        outerOnly: true,
        zBase: (renderCols.length - 1 - i) * 4,
    }));

    // OOB separator + shaded band, attached to the first band series.
    if (oob && echartsSeries.length > 0) {
        const marks = oobSeparator(range.max, oob.oobMax);
        echartsSeries[0].markLine = marks.markLine;
        echartsSeries[0].markArea = marks.markArea;
    }

    const widest = cols.reduce((a, s) => (s.t.length > a.length ? s.t : a), cols[0].t);
    const baseOption = getBaseOption();
    const option = {
        ...baseOption,
        dataZoom: getDataZoomConfig(calculateMinZoomSpan(widest)),
        yAxis: oob
            ? oobYAxis(getBaseYAxisOption(logScale, unitSystem), oob.oobMax, range.max)
            : getBaseYAxisOption(logScale, unitSystem),
        tooltip: {
            ...baseOption.tooltip,
            formatter: getTooltipFormatter(
                unitSystem ? createAxisLabelFormatter(unitSystem) : (val) => val,
                null,
                chart,
            ),
        },
        legend: buildOverlayLegendOption(labels),
        grid: {
            ...(baseOption.grid || {}),
            top: String(CHART_GRID_TOP_WITH_LEGEND),
            left: HISTOGRAM_CHART_GRID_LEFT,
        },
        series: echartsSeries,
    };
    applyChartOption(chart, option);
}

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

    // Display mode: when the percentile data is decimated (zoomed out), render
    // each percentile as a median line + min/max band instead of the noisy
    // wall of scatter dots. At native resolution (drilled in, not decimated)
    // fall through to the scatter render below, where second-to-second noise
    // reads better as dots.
    if (Array.isArray(chart.spec.boxplot) && chart.spec.boxplotDecimated) {
        renderPercentileBands(chart);
        // The Full/Tail spectrum controls belong on the decimated band view too,
        // not just the drilled-in dots render — otherwise they only appear after
        // zooming in far enough to leave display mode. (The spectrum fetch is
        // budget-strided, so Full/Tail works from the zoomed-out view.)
        ensureSpectrumCheckboxes(chart);
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
        const { oobCenter, oobMax } = computeOobBand(range, logScale);
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

    // Build yAxis config — when OOB is active, cap at the band top + relabel range.max "OOB"
    const baseYAxis = getBaseYAxisOption(logScale, unitSystem);
    const yAxisConfig = chart._oobAxisMax
        ? oobYAxis(baseYAxis, chart._oobAxisMax, range.max)
        : baseYAxis;

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
    if (chart._oobAxisMax && range && range.max != null) {
        const firstScatter = option.series.find(s => s.type === 'scatter');
        if (firstScatter) {
            const marks = oobSeparator(range.max, chart._oobAxisMax);
            firstScatter.markLine = marks.markLine;
            firstScatter.markArea = marks.markArea;
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
    // Flex centers the 16px glyph against the 13px label, then a 2px
    // upward translate compensates for the ☐/☑ glyph's built-in
    // bottom whitespace so the visual centers actually line up.
    el.style.display = 'inline-flex';
    el.style.alignItems = 'center';
    el.style.gap = '4px';
    el.innerHTML =
        `<span style="font-size: 16px; line-height: 1; transform: translateY(-2px);">${glyph}</span><span>${label}</span>`;
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
// Threshold picked to cover phones (full-width), phones in compare
// mode (half-width), tablet portrait, and narrow desktop windows.
const SPECTRUM_CONTROLS_NARROW_WIDTH = 700;
function positionControlsAtGridLeft(chart, container) {
    if (!chart.echart) return;
    try {
        const grid = chart.echart.getModel().getComponent('grid');
        const rect = grid?.coordinateSystem?.getRect();
        const dom = chart.echart.getDom?.();
        const chartWidth = dom?.clientWidth ?? 0;
        // Selection cards put notes alongside the chart, so the
        // histogram is structurally narrow regardless of viewport width.
        const inSelectionCard = !!dom?.closest?.('.selection-card-chart');
        const narrow = inSelectionCard
            || (chartWidth > 0 && chartWidth < SPECTRUM_CONTROLS_NARROW_WIDTH);
        if (narrow) {
            // Narrow chart: stack above the legend on its own line and
            // align to the right edge so it visually anchors to the
            // legend's column rather than floating in the left gutter.
            // top:24 keeps a ~6px gap above the legend (at top:42) so
            // the 13px control font doesn't touch the legend baseline.
            // right:28 = 12px chart inset + ~1em breathing room so the
            // checkboxes don't crowd the legend's rightmost chip.
            container.style.top = '24px';
            container.style.left = 'auto';
            container.style.right = '28px';
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

