
import {
    createLineChartOption
} from './line.js';
import {
    createScatterChartOption
} from './scatter.js';
import {
    createHeatmapOption
} from './heatmap.js';
import {
    createMultiSeriesChartOption
} from './multi.js';
import globalColorMapper from './colormap.js';


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
        const chart = echarts.init(this.domNode);
        const startTime = new Date();
        chart.on('finished', () => {
            chart.off('finished');
            console.log(`Chart ${this.chartId} rendered in ${new Date() - startTime}ms`);
        })

        // Store original time data for human-friendly tick calculation
        if (this.spec.data && this.spec.data.length > 0) {
            if (this.spec.data[0] && Array.isArray(this.spec.data[0])) {
                // For line and scatter charts, time is in the first row
                chart.originalTimeData = this.spec.data[0];
            }
        } else if (this.spec.time_data) {
            // For heatmaps, time is in time_data property
            chart.originalTimeData = this.spec.time_data;
        }

        // Store chart instance for cleanup and to prevent re-initialization
        this.chartsState.initializedCharts.set(this.chartId, chart);

        // Configure the chart using the spec
        const option = createChartOption(this.spec, this.chartsState);
        chart.setOption(option);

        // Match existing zoom state.
        if (this.chartsState.zoomLevel !== null) {
            if (this.chartsState.zoomLevel.start !== 0 || this.chartsState.zoomLevel.end !== 100) {
                // Apply the zoom state to the new chart
                chart.dispatchAction({
                    type: 'dataZoom',
                    start: this.chartsState.zoomLevel.start,
                    end: this.chartsState.zoomLevel.end,
                    startValue: this.chartsState.zoomLevel.startValue,
                    endValue: this.chartsState.zoomLevel.endValue,
                });
            }
        }

        chart.on('datazoom', (event) => {
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
        // This requires the toolbox to be enabled. See the comment there for more details.
        chart.dispatchAction({
            type: 'takeGlobalCursor',
            key: 'dataZoomSelect',
            dataZoomSelectActive: true,
        });

        // Double click on a chart -> reset zoom level
        // https://github.com/apache/echarts/issues/18195#issuecomment-1399583619
        // TODO: Add a visible interface element to reset zoom, too.
        chart.getZr().on('dblclick', () => {
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

        // Store chart in vnode state for updates and cleanup
        this.echart = chart;
    }
}

function createChartOption(spec, chartsState) {
    const {
        opts
    } = spec;

    // Basic option template
    const baseOption = {
        grid: {
            left: '14%',
            right: '5%',
            // Subtracting from the element height, these give 384px height for the chart itself.
            top: '35',
            bottom: '35',
            containLabel: false,
        },
        xAxis: {
            type: 'time',
            min: 'dataMin',
            max: 'dataMax',
            // splitNumber appears to control the MINIMUM number of ticks. The max number is much higher.
            // This value is lowered from the default of 5 in order to reduce the max number of ticks,
            // which cause visual overlap of labels. It feels like this shouldn't be necessary.
            // Testing showed that their "automatic" determination of how many ticks fit is independent
            // of the size of the chart. So this value is trying to be empirically correct for charts of
            // a reasonable size (which is dependent on the size of the window).
            // TODO: should we adjust split number based on the size of the window? Or take x axis labels
            // into our own hands?
            splitNumber: 4,
            axisLine: {
                lineStyle: {
                    color: '#ABABAB'
                }
            },
            axisLabel: {
                color: '#ABABAB',
            },
        },
        tooltip: {
            trigger: 'axis',
            axisPointer: {
                type: 'line',
                snap: true,
                animation: false,
                label: {
                    backgroundColor: '#505765'
                }
            },
            textStyle: {
                color: '#E0E0E0'
            },
            backgroundColor: 'rgba(50, 50, 50, 0.8)',
            borderColor: 'rgba(70, 70, 70, 0.8)',
        },
        // This invisible toolbox is a workaround to have drag-to-zoom as the default behavior.
        // We programmatically activate the zoom tool and hide the interface.
        // https://github.com/apache/echarts/issues/13397#issuecomment-814864873
        toolbox: {
            orient: 'vertical',
            itemSize: 13,
            top: 15,
            right: -6,
            feature: {
                dataZoom: {
                    yAxisIndex: 'none',
                    icon: {
                        zoom: 'path://', // hack to remove zoom button
                        back: 'path://', // hack to remove restore button
                    },
                },
            },
        },
        title: {
            text: opts.title,
            left: 'center',
            textStyle: {
                color: '#E0E0E0'
            }
        },
        textStyle: {
            color: '#E0E0E0'
        },
        darkMode: true,
        backgroundColor: 'transparent'
    };

    // Handle different chart types by delegating to specialized modules
    if (opts.style === 'line') {
        return createLineChartOption(baseOption, spec, chartsState);
    } else if (opts.style === 'heatmap') {
        return createHeatmapOption(baseOption, spec, chartsState);
    } else if (opts.style === 'scatter') {
        return createScatterChartOption(baseOption, spec, chartsState);
    } else if (opts.style === 'multi') {
        // Multi-series chart type with consistent cgroup colors
        return createMultiSeriesChartOption(baseOption, spec, chartsState);
    }

    return baseOption;
}