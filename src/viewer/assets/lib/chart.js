
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
    // Zoom state - for synchronization across charts
    zoomState = null;
    // Initialized charts - to prevent re-rendering
    initializedCharts = new Map();
    // Global color mapper - for consistent cgroup colors
    colorMapper = globalColorMapper;

    // Resets state, disposing of all initialized charts.
    clear() {
        this.zoomState = null;
        this.initializedCharts.forEach((chart) => {
            chart.dispose();
        });
        this.initializedCharts.clear();
    }
}

// Chart component - uses echarts to render a chart
export const Chart = {
    oncreate: function (vnode) {
        const domNode = vnode.dom;

        // Set up the Intersection Observer to lazy load the chart
        const observer = new IntersectionObserver((entries) => {
            const hasIntersection = entries.some(entry => entry.isIntersecting);
            if (hasIntersection) {
                this.init(vnode);
                // Once initialized, we can stop observing
                observer.unobserve(domNode);
            }
        }, {
            root: null, // Use viewport as root
            rootMargin: '100px', // Load when within 100px of viewport
            threshold: 0.01 // Trigger when at least 1% visible
        });

        // Start observing the chart element
        observer.observe(domNode);

        // Add window resize handler
        const resizeHandler = () => {
            if (vnode.state.chart) {
                vnode.state.chart.resize();
            }
        };
        window.addEventListener('resize', resizeHandler);
        vnode.state.resizeHandler = resizeHandler;
        vnode.state.observer = observer;
    },

    onremove: function (vnode) {
        // Clean up chart instance and event handlers
        if (vnode.state.observer) {
            vnode.state.observer.disconnect();
        }

        if (vnode.state.chart) {
            window.removeEventListener('resize', vnode.state.resizeHandler);
            // Don't dispose the chart since it's stored in initializedCharts
            // Only remove our reference to it
            vnode.state.chart = null;
        }
    },

    view: function () {
        return m('div.chart');
    },

    // Private methods
    init: function (vnode) {
        const domNode = vnode.dom;
        const { spec, chartsState } = vnode.attrs;
        const chartId = spec.opts.id;
        if (chartsState.initializedCharts.has(chartId)) {
            // Chart was already initialized, just reference it
            vnode.state.chart = chartsState.initializedCharts.get(chartId);
            console.log(`Chart ${chartId} already initialized`);
            return;
        }

        // Initialize the chart
        const chart = echarts.init(domNode);
        const startTime = new Date();
        chart.on('finished', function () {
            chart.off('finished');
            console.log(`Chart ${chartId} rendered in ${new Date() - startTime}ms`);
        })

        // Store original time data for human-friendly tick calculation
        if (spec.data && spec.data.length > 0) {
            if (spec.data[0] && Array.isArray(spec.data[0])) {
                // For line and scatter charts, time is in the first row
                chart.originalTimeData = spec.data[0];
            }
        } else if (spec.time_data) {
            // For heatmaps, time is in time_data property
            chart.originalTimeData = spec.time_data;
        }

        // Store chart instance for cleanup and to prevent re-initialization
        chartsState.initializedCharts.set(chartId, chart);

        // Configure the chart using the spec
        const option = createChartOption(spec, chartsState);
        chart.setOption(option);

        // Match existing zoom state.
        if (chartsState.zoomState !== null) {
            if (chartsState.zoomState.start !== 0 || chartsState.zoomState.end !== 100) {
                // Apply the zoom state to the new chart
                chart.dispatchAction({
                    type: 'dataZoom',
                    start: chartsState.zoomState.start,
                    end: chartsState.zoomState.end,
                    startValue: chartsState.zoomState.startValue,
                    endValue: chartsState.zoomState.endValue,
                });
            }
        }

        chart.on('datazoom', function (event) {
            // 'datazoom' events triggered by the user vs dispatched by us have different formats:
            // User-triggered events have a batch property with the details under it.
            // (We don't want to trigger on our own dispatched zoom actions, so this is convenient.)
            if (!event.batch) {
                return;
            }

            const details = event.batch[0];

            const { start, end, startValue, endValue } = details;
            chartsState.zoomState = {
                start,
                end,
                startValue,
                endValue,
            };
            chartsState.initializedCharts.forEach(chart => {
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
        chart.getZr().on('dblclick', function () {
            chartsState.zoomState = {
                start: 0,
                end: 100,
            };
            chartsState.initializedCharts.forEach(chart => {
                chart.dispatchAction({
                    type: 'dataZoom',
                    start: 0,
                    end: 100,
                });
            });
        })

        // Store chart in vnode state for updates and cleanup
        vnode.state.chart = chart;
    }
};

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