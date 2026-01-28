
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
import globalColorMapper from './util/colormap.js';


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
        this.resizeHandler = null;
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

        // Add window resize handler
        const resizeHandler = () => {
            if (this.echart) {
                this.echart.resize();
            }
        };
        window.addEventListener('resize', resizeHandler);
        this.resizeHandler = resizeHandler;
        this.observer = observer;
    }

    onupdate(vnode) {
        // Update the spec reference
        const oldSpec = this.spec;
        this.spec = vnode.attrs.spec;

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

        if (this.echart) {
            window.removeEventListener('resize', this.resizeHandler);
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
            });
        });

        // Enable drag-to-zoom
        // This requires the toolbox to be enabled. See the comment for the toolbox configuration in base.js for more details.
        this.echart.dispatchAction({
            type: 'takeGlobalCursor',
            key: 'dataZoomSelect',
            dataZoomSelectActive: true,
        });

        // Double click on a chart -> reset zoom level
        // https://github.com/apache/echarts/issues/18195#issuecomment-1399583619
        // TODO: Add a visible interface element to reset zoom, too.
        this.echart.getZr().on('dblclick', () => {
            this.chartsState.zoomLevel = {
                start: 0,
                end: 100,
            };
            this.chartsState.charts.forEach(chart => {
                chart.dispatchAction({
                    type: 'dataZoom',
                    start: 0,
                    end: 100,
                });
            });
        })
    }

    configureChartByType() {
        const {
            opts
        } = this.spec;

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