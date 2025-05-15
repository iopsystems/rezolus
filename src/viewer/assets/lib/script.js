
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

// Sidebar component
const Sidebar = {
    view({
        attrs
    }) {
        return m("div#sidebar", [
            attrs.sections.map((section) => m(m.route.Link, {
                class: attrs.activeSection === section ? 'selected' : '',
                href: section.route,
            }, section.name))
        ]);
    }
};

// Main component
const Main = {
    view({
        attrs: {
            activeSection,
            groups,
            sections
        }
    }) {
        return m("div",
            m("header", [
                m('h1', 'Rezolus', m('span.div', ' » '), activeSection.name),
            ]),
            m("main", [
                m(Sidebar, {
                    activeSection,
                    sections
                }),
                m('div#groups',
                    groups.map((group) => m(Group, group))
                )
            ]));
    }
};

// Group component
const Group = {
    view({
        attrs
    }) {
        return m("div.group", {
            id: attrs.id
        }, [
            m("h2", `${attrs.name}`),
            m("div.plots", attrs.plots.map(spec => m(Plot, spec))),
        ]);
    }
};

// Plot component - renders an echarts chart
const Plot = {
    oncreate: function (vnode) {
        const {
            attrs
        } = vnode;
        const chartDom = vnode.dom;

        // Set up the Intersection Observer to lazy load the chart
        const observer = new IntersectionObserver((entries) => {
            entries.forEach(entry => {
                if (entry.isIntersecting) {
                    // Check if we already initialized this chart
                    const chartId = attrs.opts.id;
                    if (!state.initializedCharts.has(chartId)) {
                        // Initialize the chart
                        const chart = echarts.init(chartDom);

                        // Store original time data for human-friendly tick calculation
                        if (attrs.data && attrs.data.length > 0) {
                            if (attrs.data[0] && Array.isArray(attrs.data[0])) {
                                // For line and scatter charts, time is in the first row
                                chart.originalTimeData = attrs.data[0];
                            }
                        } else if (attrs.time_data) {
                            // For heatmaps, time is in time_data property
                            chart.originalTimeData = attrs.time_data;
                        }

                        // Store chart instance for cleanup and to prevent re-initialization
                        state.initializedCharts.set(chartId, chart);

                        // Configure and render the chart based on plot style
                        const option = createChartOption(attrs);
                        chart.setOption(option);

                        // Match existing zoom state.
                        if (state.zoomState !== null) {

                            if (state.zoomState.start !== 0 || state.zoomState.end !== 100) {
                                // Apply the zoom state to the new chart
                                chart.dispatchAction({
                                    type: 'dataZoom',
                                    start: state.zoomState.start,
                                    end: state.zoomState.end,
                                    startValue: state.zoomState.startValue,
                                    endValue: state.zoomState.endValue,
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
                            state.zoomState = {
                                start,
                                end,
                                startValue,
                                endValue,
                            };
                            state.initializedCharts.forEach(chart => {
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
                            state.zoomState = {
                                start: 0,
                                end: 100,
                            };
                            state.initializedCharts.forEach(chart => {
                                chart.dispatchAction({
                                    type: 'dataZoom',
                                    start: 0,
                                    end: 100,
                                });
                            });
                        })

                        // Store chart in vnode state for updates and cleanup
                        vnode.state.chart = chart;
                    } else {
                        // Chart was already initialized, just reference it
                        vnode.state.chart = state.initializedCharts.get(chartId);
                    }

                    // Once initialized, we can stop observing
                    observer.unobserve(chartDom);
                }
            });
        }, {
            root: null, // Use viewport as root
            rootMargin: '100px', // Load when within 100px of viewport
            threshold: 0.1 // Trigger when at least 10% visible
        });

        // Start observing the chart element
        observer.observe(chartDom);

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
        return m('div.plot');
    }
};

function createChartOption(plotSpec) {
    const {
        opts
    } = plotSpec;

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

    // Handle different plot types by delegating to specialized modules
    if (opts.style === 'line') {
        return createLineChartOption(baseOption, plotSpec, state);
    } else if (opts.style === 'heatmap') {
        return createHeatmapOption(baseOption, plotSpec, state);
    } else if (opts.style === 'scatter') {
        return createScatterChartOption(baseOption, plotSpec, state);
    } else if (opts.style === 'multi') {
        // Multi-series chart type with consistent cgroup colors
        return createMultiSeriesChartOption(baseOption, plotSpec, state);
    }

    return baseOption;
}

// Application state management
const state = {
    // Zoom state - for synchronization across charts
    zoomState: null,
    // Initialized charts - to prevent re-rendering
    initializedCharts: new Map(),
    // Global color mapper - for consistent cgroup colors
    colorMapper: globalColorMapper,
};

// Main application entry point
m.route.prefix = ""; // use regular paths for navigation, eg. /overview
m.route(document.body, "/overview", {
    "/:section": {
        onmatch(params, requestedPath) {
            // Prevent a route change if we're already on this route
            if (m.route.get() === requestedPath) {
                return new Promise(function () { });
            }

            if (requestedPath !== m.route.get()) {
                // Clear initialized charts and zoom state.
                state.zoomState = null;
                state.initializedCharts.forEach((chart) => {
                    chart.dispose();
                });
                state.initializedCharts.clear();

                // Reset scroll position.
                window.scrollTo(0, 0);
            }

            const url = `/data/${params.section}.json`;
            console.time(`Load ${url}`);
            return m.request({
                method: "GET",
                url,
                withCredentials: true,
            }).then(data => {
                console.timeEnd(`Load ${url}`);
                const activeSection = data.sections.find(section => section.route === requestedPath);
                return ({
                    view() {
                        return m(Main, {
                            ...data,
                            activeSection
                        });
                    },
                    oncreate(vnode) {
                    },
                    onremove(vnode) {
                    }
                });
            });
        }
    }
});