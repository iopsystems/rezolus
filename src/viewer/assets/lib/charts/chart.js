
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


export class ChartsState {
    // Zoom state for synchronization across charts
    // { 
    //     start?: number, // 0-100
    //     end?: number, // 0-100
    //     startValue?: number, // raw x axis data value
    //     endValue?: number,  // raw x axis data value
    // }
    zoomLevel = null;
    // All `Chart` instances, mapped by id
    charts = new Map();
    // Global color mapper - for consistent cgroup colors
    colorMapper = globalColorMapper;

    // Resets charts state. It's assumed that each individual chart
    // will be disposed of when it is removed from the DOM.
    clear() {
        this.zoomLevel = null;
        this.charts.clear();
    }

    isDefaultZoom() {
        return !this.zoomLevel || (this.zoomLevel.start === 0 && this.zoomLevel.end === 100);
    }

    // Reset zoom level on all charts
    resetZoom() {
        this.zoomLevel = {
            start: 0,
            end: 100,
        };
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

        // If the chart is already initialized and data has changed, update it
        if (this.echart && oldSpec.data !== this.spec.data) {
            // Instead of reinitializing, just update the chart configuration
            this.configureChartByType();
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

        if (this.echart) {
            this.echart.dispose();
            this.echart = null;
        }
    }

    view() {
        return m('div.chart');
    }

    isInitialized() {
        return this.echart !== null;
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

    /**
     * If the echart instance is already initialized, dispose and reinitialize it.
     */
    reinitialize() {
        if (this.isInitialized()) {
            this.echart.dispose();
            this.echart = null;
            this.initEchart();
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

        // Store original time data for human-friendly tick calculation
        let timeData = null;
        if (this.spec.data && this.spec.data.length > 0) {
            if (this.spec.data[0] && Array.isArray(this.spec.data[0])) {
                // For line and scatter charts, time is in the first row
                timeData = this.spec.data[0];
                this.echart.originalTimeData = timeData;
            }
        } else if (this.spec.time_data) {
            // For heatmaps, time is in time_data property
            timeData = this.spec.time_data;
            this.echart.originalTimeData = timeData;
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
        // TODO: Add a visible interface element to reset zoom, too.
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

        const style = this.spec.opts.style;
        // Only for chart types with a value/log Y-axis
        if (style === 'heatmap' || style === 'histogram_heatmap') return;

        const data = this.spec.data;
        if (!data || data.length < 2 || !data[0] || data[0].length === 0) return;

        const format = this.spec.opts.format || {};
        const option = this.echart.getOption();
        const isLog = option?.yAxis?.[0]?.type === 'log';

        // If at default zoom, restore original bounds
        if (this.chartsState.isDefaultZoom()) {
            this.echart.setOption({
                yAxis: { min: format.min ?? null, max: format.max ?? null }
            });
            return;
        }

        // Compute visible time range from zoom state
        const timeData = data[0]; // seconds
        const zoom = this.chartsState.zoomLevel;

        let visibleMinMs, visibleMaxMs;
        if (zoom.startValue !== undefined && zoom.endValue !== undefined) {
            visibleMinMs = zoom.startValue;
            visibleMaxMs = zoom.endValue;
        } else {
            const totalMinMs = timeData[0] * 1000;
            const totalMaxMs = timeData[timeData.length - 1] * 1000;
            const totalRange = totalMaxMs - totalMinMs;
            visibleMinMs = totalMinMs + (zoom.start / 100) * totalRange;
            visibleMaxMs = totalMinMs + (zoom.end / 100) * totalRange;
        }

        // Scan raw data for min/max Y in visible range
        let yMin = Infinity;
        let yMax = -Infinity;

        for (let seriesIdx = 1; seriesIdx < data.length; seriesIdx++) {
            const values = data[seriesIdx];
            for (let i = 0; i < timeData.length; i++) {
                const tMs = timeData[i] * 1000;
                if (tMs < visibleMinMs) continue;
                if (tMs > visibleMaxMs) break;
                const y = values[i];
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

        // Respect explicit format bounds if configured
        this.echart.setOption({
            yAxis: {
                min: format.min ?? newMin,
                max: format.max ?? newMax,
            }
        });
    }

    configureChartByType() {
        const {
            opts
        } = this.spec;

        // Clean up histogram heatmap DOM overlays when switching to a different chart type
        if (opts.style !== 'histogram_heatmap') {
            for (const cls of ['histogram-toggle', 'heatmap-label-min', 'heatmap-label-max']) {
                this.domNode?.querySelector('.' + cls)?.remove();
            }
        }

        // Handle different chart types by delegating to specialized modules
        if (opts.style === 'line') {
            configureLineChart(this, this.spec, this.chartsState);
        } else if (opts.style === 'heatmap') {
            configureHeatmap(this, this.spec, this.chartsState);
        } else if (opts.style === 'histogram_heatmap') {
            configureHistogramHeatmap(this, this.spec, this.chartsState);
        } else if (opts.style === 'scatter') {
            configureScatterChart(this, this.spec, this.chartsState);
        } else if (opts.style === 'multi') {
            configureMultiSeriesChart(this, this.spec, this.chartsState);
        } else {
            throw new Error(`Unknown chart style: ${opts.style}`);
        }
    }
}