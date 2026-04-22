
import {
    configureLineChart
} from './line.js';
import {
    configureScatterChart
} from './scatter.js';
import {
    configureHeatmap
} from './heatmap.js';
import {
    configureHistogramHeatmap
} from './histogram_heatmap.js';
import {
    configureMultiSeriesChart
} from './multi.js';
import globalColorMapper, { COLORS } from './util/colormap.js';
import { themeVersion } from '../theme.js';
import { resolveStyle, resolvedStyle } from './metric_types.js';


export class ChartsState {
    // Zoom state for synchronization across charts
    // {
    //     start?: number, // 0-100
    //     end?: number, // 0-100
    //     startValue?: number, // raw x axis data value (ms)
    //     endValue?: number,   // raw x axis data value (ms)
    // }
    zoomLevel = null;
    // 'global' (time bar) | 'local' (chart drag/scroll) | null
    zoomSource = null;
    // Global zoom — always percentage-based { start, end } (0-100).
    // Tracks what the time bar shows. Never updated by local chart zooms.
    globalZoom = null;
    // All `Chart` instances, mapped by id
    charts = new Map();
    // Global color mapper - for consistent cgroup colors
    colorMapper = globalColorMapper;

    // Resets charts state. It's assumed that each individual chart
    // will be disposed of when it is removed from the DOM.
    clear() {
        this.zoomLevel = null;
        this.zoomSource = null;
        this.globalZoom = null;
        this.charts.clear();
    }

    isDefaultZoom() {
        return !this.zoomLevel || (this.zoomLevel.start === 0 && this.zoomLevel.end === 100);
    }

    /** Returns true when any chart has a local zoom, frozen tooltip, or pinned series. */
    hasActiveSelection() {
        const hasLocalZoom = this.zoomSource === 'local' && !this.isDefaultZoom();
        return hasLocalZoom ||
            Array.from(this.charts.values()).some(
                c => c._tooltipFrozen || (c.pinnedSet && c.pinnedSet.size > 0));
    }

    // Reset zoom level on all charts
    resetZoom() {
        this.zoomLevel = { start: 0, end: 100 };
        this.zoomSource = null;
        this.globalZoom = { start: 0, end: 100 };
        this.charts.forEach(chart => {
            chart.dispatchAction({
                type: 'dataZoom',
                start: 0,
                end: 100,
            });
            chart._rescaleYAxis();
        });
    }

    // Reset zoom and clear all pin selections and frozen tooltips
    // (preserves heatmap/percentile toggle)
    resetAll() {
        this.resetZoom();
        this.charts.forEach(chart => {
            if (chart._tooltipFrozen) {
                chart._toggleTooltipFreeze(false);
            }
            // Hide any visible tooltips and axis pointer lines
            chart.dispatchAction({ type: 'hideTip' });
            chart.dispatchAction({ type: 'updateAxisPointer', currTrigger: 'leave' });
            if (chart.pinnedSet && chart.pinnedSet.size > 0) {
                chart.pinnedSet.clear();
                chart.configureChartByType();
            }
        });
    }
}

// Chart component - uses echarts to render a chart
export class Chart {
    constructor(vnode) {
        this.domNode = null; // not available until oncreate
        this.chartId = vnode.attrs.spec.opts.id;
        this.spec = vnode.attrs.spec;
        this.chartsState = vnode.attrs.chartsState;
        this.interval = vnode.attrs.interval; // sampling interval in seconds
        this.resizeObserver = null;
        this.observer = null;
        this.echart = null;
        this._themeVersion = themeVersion;
    }

    oncreate(vnode) {
        this.domNode = vnode.dom;

        // Set up the Intersection Observer to lazy load the chart
        const observer = new IntersectionObserver((entries) => {
            const hasIntersection = entries.some(entry => entry.isIntersecting);
            if (hasIntersection) {
                this.initEchart();
                // Once initialized, we can stop observing
                observer.unobserve(this.domNode);
            }
        }, {
            root: null, // Use viewport as root
            rootMargin: '1px', // Load when within 1px of viewport
            threshold: 0.01 // Trigger when at least 1% visible
        });

        // Start observing the chart element
        observer.observe(this.domNode);

        // Resize charts when their container size changes (window resize, zoom, etc.)
        this.resizeObserver = new ResizeObserver(() => {
            if (this.echart) {
                this.echart.resize();
            }
        });
        this.resizeObserver.observe(this.domNode);
        this.observer = observer;
    }

    onupdate(vnode) {
        // Update the spec and interval references
        const oldSpec = this.spec;
        this.spec = vnode.attrs.spec;
        this.interval = vnode.attrs.interval;

        // Re-render if data changed, format changed, or theme was toggled
        const themeChanged = this._themeVersion !== themeVersion;
        const formatChanged = oldSpec.opts?.format !== this.spec.opts?.format;
        if (this.echart && (oldSpec.data !== this.spec.data || formatChanged || themeChanged)) {
            this._themeVersion = themeVersion;
            this.configureChartByType();

            // Restore zoom state after re-render (notMerge wipes the
            // dataZoom range). Applies to every reconfigure — not just
            // theme changes — because in compare mode each Mithril
            // redraw hands in a fresh spec object and therefore
            // triggers a full reconfigure, which would otherwise clear
            // the user's zoom on the non-source slot.
            if (this.chartsState.zoomLevel !== null) {
                const z = this.chartsState.zoomLevel;
                if (z.start !== 0 || z.end !== 100) {
                    this.echart.dispatchAction({
                        type: 'dataZoom',
                        start: z.start,
                        end: z.end,
                        startValue: z.startValue,
                        endValue: z.endValue,
                    });
                    this._rescaleYAxis();
                }
            }
            // Re-arm drag-to-zoom. applyChartOption inside
            // configureChartByType already dispatches takeGlobalCursor,
            // but the subsequent dataZoom restore above can leave
            // echarts' internal cursor state in a stale position
            // (noticeable on heatmaps, where the toolbox rectangle
            // select stops responding). Re-arm unconditionally.
            this.echart.dispatchAction({
                type: 'takeGlobalCursor',
                key: 'dataZoomSelect',
                dataZoomSelectActive: true,
            });
        }
    }

    onremove() {
        // Clean up chart instance and event handlers
        if (this.observer) {
            this.observer.disconnect();
        }

        if (this.resizeObserver) {
            this.resizeObserver.disconnect();
        }

        if (this._freezeKeyCleanup) this._freezeKeyCleanup();
        if (this._pinCleanup) this._pinCleanup();

        if (this.echart) {
            this.echart.dispose();
            this.echart = null;
        }
    }

    view() {
        return m('div.chart');
    }

    /**
     * Dispatch an action to the echart instance if it is initialized.
     */
    dispatchAction(action) {
        if (this.echart) {
            this.echart.dispatchAction(action);
        }
    }

    /**
     * Toggle tooltip freeze state. When frozen, the tooltip stays at its
     * current position until unfrozen.
     * @param {boolean} [force] - if provided, set frozen state explicitly
     */
    _toggleTooltipFreeze(force) {
        if (!this.echart) return;
        const freeze = force !== undefined ? force : !this._tooltipFrozen;
        this._tooltipFrozen = freeze;

        if (freeze) {
            // Stop tooltip from following the mouse
            this.echart.setOption({
                tooltip: { triggerOn: 'none' },
            });
        } else {
            // Restore normal tooltip behavior
            this.echart.setOption({
                tooltip: { triggerOn: 'mousemove|click' },
            });
        }

        // Directly patch the tooltip footer DOM since the formatter won't
        // re-run on an already-visible tooltip.
        const footer = this.domNode.querySelector('.tooltip-freeze-footer');
        if (footer) {
            footer.textContent = freeze ? 'FROZEN \u00b7 click to unfreeze' : 'click to freeze';
            footer.style.color = freeze ? COLORS.accent : COLORS.fgMuted;
        }
    }

    initEchart() {
        if (this.echart) {
            return;
        }

        // Initialize the chart
        this.echart = echarts.init(this.domNode);
        // Only log initial chart creation in debug mode
        if (window.location.search.includes('debug')) {
            const startTime = new Date();
            this.echart.on('finished', () => {
                this.echart.off('finished');
                console.log(`Chart ${this.chartId} rendered in ${new Date() - startTime}ms`);
            });
        }

        let timeData = null;
        if (this.spec.data && this.spec.data.length > 0) {
            if (this.spec.data[0] && Array.isArray(this.spec.data[0])) {
                timeData = this.spec.data[0];
            }
        } else if (this.spec.time_data) {
            timeData = this.spec.time_data;
        }

        // Calculate sample interval and minimum zoom percentage
        // Minimum zoom is 5x the sample interval
        if (timeData && timeData.length >= 2) {
            const sampleInterval = timeData[1] - timeData[0]; // in seconds
            const totalDuration = timeData[timeData.length - 1] - timeData[0];
            const minVisibleDuration = sampleInterval * 5;
            // Convert to percentage of total duration
            this.minZoomPercent = Math.max(0.1, (minVisibleDuration / totalDuration) * 100);
        } else {
            this.minZoomPercent = 0.1; // fallback minimum
        }

        // Store chart instance for cleanup and to prevent re-initialization
        this.chartsState.charts.set(this.chartId, this);

        // Perform the main echarts configuration work, and set up any chart-specific dynamic behavior.
        this.configureChartByType();

        // Match existing zoom state.
        if (this.chartsState.zoomLevel !== null) {
            if (this.chartsState.zoomLevel.start !== 0 || this.chartsState.zoomLevel.end !== 100) {
                // Apply the zoom state to the new chart
                this.echart.dispatchAction({
                    type: 'dataZoom',
                    start: this.chartsState.zoomLevel.start,
                    end: this.chartsState.zoomLevel.end,
                    startValue: this.chartsState.zoomLevel.startValue,
                    endValue: this.chartsState.zoomLevel.endValue,
                });
                this._rescaleYAxis();
            }
        }

        this.echart.on('datazoom', (event) => {
            // 'datazoom' events triggered by the user vs dispatched by us have different formats:
            // User-triggered events have a batch property with the details under it.
            // (We don't want to trigger on our own dispatched zoom actions, so this is convenient.)
            if (!event.batch) {
                return;
            }

            const details = event.batch[0];

            let { start, end, startValue, endValue } = details;

            // Toolbox drag-to-zoom sometimes only emits startValue /
            // endValue (absolute axis coords) and omits the percentage
            // pair. Downstream code (TimeRangeBar's Match Selection
            // button, onupdate's zoom restore) relies on the percentage
            // form, and in compare mode the absolute-coord fallback is
            // broken because the chart axis is in relative ms but the
            // TimeRangeBar's start_time is wall-clock ms. Derive the
            // percentages from the chart's own time range when missing.
            if ((start === undefined || Number.isNaN(start))
                && startValue !== undefined
                && endValue !== undefined) {
                const td = this.spec.time_data
                    || (Array.isArray(this.spec.data) ? this.spec.data[0] : null);
                if (td && td.length >= 2) {
                    // Chart x-axis values are timeData[i] * 1000 (ms).
                    const axisMin = td[0] * 1000;
                    const axisMax = td[td.length - 1] * 1000;
                    const total = axisMax - axisMin;
                    if (total > 0) {
                        start = Math.max(0, Math.min(100, ((startValue - axisMin) / total) * 100));
                        end = Math.max(0, Math.min(100, ((endValue - axisMin) / total) * 100));
                    }
                }
            }

            // Enforce minimum zoom level (5x sample interval)
            const zoomRange = end - start;
            const minZoom = this.minZoomPercent || 0.1;

            if (zoomRange < minZoom) {
                // Zoom is too tight, clamp it
                const center = (start + end) / 2;
                start = Math.max(0, center - minZoom / 2);
                end = Math.min(100, center + minZoom / 2);

                // Adjust if we hit the boundaries
                if (start === 0) {
                    end = Math.min(100, minZoom);
                } else if (end === 100) {
                    start = Math.max(0, 100 - minZoom);
                }

                // Clear startValue/endValue since we're using percentages
                startValue = undefined;
                endValue = undefined;
            }

            this.chartsState.zoomLevel = {
                start,
                end,
                startValue,
                endValue,
            };
            this.chartsState.zoomSource = 'local';
            this.chartsState.charts.forEach(chart => {
                chart.dispatchAction({
                    type: 'dataZoom',
                    start,
                    end,
                    startValue,
                    endValue,
                });
                chart._rescaleYAxis();
            });
            m.redraw();
        });

        // Enable drag-to-zoom
        // This requires the toolbox to be enabled. See the comment for the toolbox configuration in base.js for more details.
        this.echart.dispatchAction({
            type: 'takeGlobalCursor',
            key: 'dataZoomSelect',
            dataZoomSelectActive: true,
        });

        // Tooltip freeze: click on chart to freeze, click again to unfreeze.
        // Escape also unfreezes.
        this._tooltipFrozen = false;

        this.echart.getZr().on('click', (e) => {
            // Only freeze when clicking within the plot area, not on legend/title
            if (this.echart.containPixel('grid', [e.offsetX, e.offsetY])) {
                this._toggleTooltipFreeze();
            }
        });

        const onEscKey = (e) => {
            if (e.key === 'Escape' && this._tooltipFrozen) {
                e.preventDefault();
                this._toggleTooltipFreeze(false);
            }
        };
        document.addEventListener('keydown', onEscKey, true);
        this._freezeKeyCleanup = () => {
            document.removeEventListener('keydown', onEscKey, true);
        };

        // Double click on a chart -> reset zoom level
        // https://github.com/apache/echarts/issues/18195#issuecomment-1399583619
        this.echart.getZr().on('dblclick', () => {
            this.chartsState.resetAll();
            m.redraw();
        })
    }

    /**
     * Rescale Y-axis to fit visible data when zoomed in on X-axis.
     * Called after every datazoom event and on zoom reset.
     */
    _rescaleYAxis() {
        if (!this.echart) return;

        const style = resolvedStyle(this.spec)
            || resolveStyle(this.spec.opts.type, this.spec.opts.subtype);
        // Only for chart types with a value/log Y-axis
        if (style === 'heatmap' || style === 'histogram_heatmap') return;

        // In compare-mode overlays, spec.data holds only the baseline's
        // [timeData, valueData]; the experiment values live in
        // spec.multiSeries[1]. Collect all series' (timeData, valueData)
        // pairs so the Y-rescale considers both captures and doesn't
        // clip the higher-valued trace.
        const multi = Array.isArray(this.spec.multiSeries) && this.spec.multiSeries.length > 0
            ? this.spec.multiSeries
            : null;
        const data = this.spec.data;
        const seriesPairs = multi
            ? multi.map((s) => ({ timeData: s.timeData, valueData: s.valueData }))
            : (data && data.length >= 2 && data[0] && data[0].length > 0
                ? (() => {
                    const timeData = data[0];
                    const out = [];
                    for (let i = 1; i < data.length; i++) out.push({ timeData, valueData: data[i] });
                    return out;
                })()
                : []);
        if (seriesPairs.length === 0) return;

        const format = this.spec.opts.format || {};
        const option = this.echart.getOption();
        const isLog = option?.yAxis?.[0]?.type === 'log';

        // When percentile series are pinned, rescale Y to pinned subset only.
        const hasPins = this.pinnedSet && this.pinnedSet.size > 0;
        const labels = this._seriesLabels; // set by scatter chart config

        // If at default zoom and no pins active, restore original bounds
        if (this.chartsState.isDefaultZoom() && !(hasPins && labels)) {
            this.echart.setOption({
                yAxis: { min: null, max: this._oobAxisMax ?? null }
            });
            return;
        }

        // Compute visible time range from zoom state. Use the widest
        // series' timeData as the reference (matches what line.js uses
        // for dataZoom).
        const refTimeData = seriesPairs.reduce(
            (a, s) => (s.timeData.length > a.length ? s.timeData : a),
            seriesPairs[0].timeData,
        );
        const zoom = this.chartsState.zoomLevel;

        let visibleMinMs, visibleMaxMs;
        if (!zoom) {
            visibleMinMs = refTimeData[0] * 1000;
            visibleMaxMs = refTimeData[refTimeData.length - 1] * 1000;
        } else if (zoom.startValue !== undefined && zoom.endValue !== undefined) {
            visibleMinMs = zoom.startValue;
            visibleMaxMs = zoom.endValue;
        } else {
            const totalMinMs = refTimeData[0] * 1000;
            const totalMaxMs = refTimeData[refTimeData.length - 1] * 1000;
            const totalRange = totalMaxMs - totalMinMs;
            visibleMinMs = totalMinMs + (zoom.start / 100) * totalRange;
            visibleMaxMs = totalMinMs + (zoom.end / 100) * totalRange;
        }

        // Scan each series' data for min/max Y in visible range.
        // When percentile series are pinned, only consider pinned series
        // (legacy spec.data path — multiSeries doesn't carry labels).
        let yMin = Infinity;
        let yMax = -Infinity;

        for (let pairIdx = 0; pairIdx < seriesPairs.length; pairIdx++) {
            if (hasPins && labels) {
                const name = labels[pairIdx];
                if (name && !this.pinnedSet.has(name)) continue;
            }
            const { timeData, valueData } = seriesPairs[pairIdx];
            for (let i = 0; i < timeData.length; i++) {
                const tMs = timeData[i] * 1000;
                if (tMs < visibleMinMs) continue;
                if (tMs > visibleMaxMs) break;
                const y = valueData[i];
                if (y !== null && y !== undefined && !isNaN(y)) {
                    if (y < yMin) yMin = y;
                    if (y > yMax) yMax = y;
                }
            }
        }

        if (yMin === Infinity || yMax === -Infinity) return;

        // Handle flat line (all same value)
        if (yMin === yMax) {
            const pad = yMin !== 0 ? Math.abs(yMin * 0.1) : 1;
            yMin -= pad;
            yMax += pad;
        }

        // Add padding — multiplicative for log scale, additive for linear
        let newMin, newMax;
        if (isLog) {
            newMin = yMin / 1.2;
            newMax = yMax * 1.2;
        } else {
            const padding = (yMax - yMin) * 0.05;
            newMin = yMin - padding;
            newMax = yMax + padding;
        }

        // Cap axis max: OOB band takes priority, then range ceiling, then padded max
        let yAxisMax;
        if (this._oobAxisMax) {
            yAxisMax = this._oobAxisMax;
        } else if (format.range?.max != null) {
            yAxisMax = Math.min(newMax, format.range.max);
        } else {
            yAxisMax = newMax;
        }
        this.echart.setOption({
            yAxis: {
                min: newMin,
                max: yAxisMax,
            }
        });
    }

    configureChartByType() {
        // Use explicit style (query explorer), resolved style (from query result),
        // or infer from metric type/subtype before data arrives.
        const style = resolvedStyle(this.spec)
            || resolveStyle(this.spec.opts.type, this.spec.opts.subtype);

        // Clean up heatmap DOM legend bar when switching to a different chart type
        if (style !== 'histogram_heatmap' && style !== 'heatmap') {
            this.domNode?.parentNode?.querySelector('.heatmap-legend-bar')?.remove();
        }

        // Handle different chart types by delegating to specialized modules
        if (style === 'line') {
            configureLineChart(this, this.spec, this.chartsState);
        } else if (style === 'heatmap') {
            configureHeatmap(this, this.spec, this.chartsState);
        } else if (style === 'histogram_heatmap') {
            configureHistogramHeatmap(this, this.spec, this.chartsState);
        } else if (style === 'scatter') {
            configureScatterChart(this, this.spec, this.chartsState);
        } else if (style === 'multi') {
            configureMultiSeriesChart(this, this.spec, this.chartsState);
        } else {
            throw new Error(`Unknown chart style: ${style}`);
        }

        // Compact layout for cgroup paired charts: same grid for both, hide echarts legend
        if (this.spec.compactGrid && this.echart) {
            this.echart.setOption({
                grid: { top: '20' },
                legend: { show: false },
            });
        }
    }
}