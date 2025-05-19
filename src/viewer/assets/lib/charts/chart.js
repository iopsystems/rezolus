
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
    // Initialized charts by id
    initializedCharts = new Map();
    // Global color mapper - for consistent cgroup colors
    colorMapper = globalColorMapper;

    // Resets state, disposing of all initialized charts.
    clear() {
        this.zoomLevel = null;
        this.initializedCharts.forEach((chart) => {
            chart.dispose();
        });
        this.initializedCharts.clear();
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
            rootMargin: '100px', // Load when within 100px of viewport
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

    onremove() {
        // Clean up chart instance and event handlers
        if (this.observer) {
            this.observer.disconnect();
        }

        if (this.echart) {
            window.removeEventListener('resize', this.resizeHandler);
            // Don't dispose the chart since it's stored in initializedCharts
            // Only remove our reference to it
            this.echart = null;
        }
    }

    view() {
        return m('div.chart');
    }

    initEchart() {
        if (this.chartsState.initializedCharts.has(this.chartId)) {
            // Chart was already initialized, just reference it
            this.echart = this.chartsState.initializedCharts.get(this.chartId);
            console.log(`Chart ${this.chartId} already initialized`);
            return;
        }

        // Initialize the chart
        this.echart = echarts.init(this.domNode);
        const startTime = new Date();
        this.echart.on('finished', () => {
            this.echart.off('finished');
            console.log(`Chart ${this.chartId} rendered in ${new Date() - startTime}ms`);
        })

        // Store original time data for human-friendly tick calculation
        if (this.spec.data && this.spec.data.length > 0) {
            if (this.spec.data[0] && Array.isArray(this.spec.data[0])) {
                // For line and scatter charts, time is in the first row
                this.echart.originalTimeData = this.spec.data[0];
            }
        } else if (this.spec.time_data) {
            // For heatmaps, time is in time_data property
            this.echart.originalTimeData = this.spec.time_data;
        }

        // Store chart instance for cleanup and to prevent re-initialization
        this.chartsState.initializedCharts.set(this.chartId, this.echart);

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

            const { start, end, startValue, endValue } = details;
            this.chartsState.zoomLevel = {
                start,
                end,
                startValue,
                endValue,
            };
            this.chartsState.initializedCharts.forEach(chart => {
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
            this.chartsState.initializedCharts.forEach(chart => {
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
        } else if (opts.style === 'scatter') {
            configureScatterChart(this, this.spec, this.chartsState);
        } else if (opts.style === 'multi') {
            configureMultiSeriesChart(this, this.spec, this.chartsState);
        } else {
            throw new Error(`Unknown chart style: ${opts.style}`);
        }
    }
}