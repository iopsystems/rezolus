// Refactored script.js - Main application logic with modular chart components and consistent cgroup colors

// Import our modular components
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
// Import the global color mapper for consistent cgroup colors
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

// Plot component that renders ECharts visualizations with proper time axis
const Plot = {
    oncreate: function (vnode) {
        const {
            attrs
        } = vnode;
        const chartDom = vnode.dom;

        // Store the attributes for later reference
        chartDom._attrs = attrs;

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
                        chart.group = 'connected_charts';

                        // Enable brush select for zooming
                        chart.dispatchAction({
                            type: 'takeGlobalCursor',
                            key: 'brush',
                            brushOption: {
                                brushType: 'lineX',
                                brushMode: 'multiple'
                            }
                        });

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

// Create ECharts options based on plot type with human-friendly time axis
function createChartOption(plotSpec) {
    const {
        opts
    } = plotSpec;

    // Basic option template
    const baseOption = {
        grid: {
            left: '14%',
            right: '5%',
            top: '40',
            bottom: '40',
            containLabel: false,
        },
        tooltip: {
            trigger: 'axis',
            axisPointer: {
                type: 'cross',
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
        title: {
            text: opts.title,
            left: 'center',
            textStyle: {
                color: '#E0E0E0'
            }
        },
        dataZoom: [{
            // Inside zoom (mouse wheel and pinch zoom)
            type: 'inside',
            filterMode: 'none', // Don't filter data points outside zoom range
            xAxisIndex: 0,
            yAxisIndex: 'none',
            start: 0,
            end: 100,
            zoomLock: false
        }],
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
    // for tracking current visualization state
    current: null,
    // Store initialized charts to prevent re-rendering
    initializedCharts: new Map(),
    // Make the color mapper available in the state for potential future use
    colorMapper: globalColorMapper
};

echarts.connect('connected_charts');

// Main application entry point
m.route.prefix = ""; // use regular paths for navigation, eg. /overview
m.route(document.body, "/overview", {
    "/:section": {
        onmatch(params, requestedPath) {
            // Prevent a route change if we're already on this route
            if (m.route.get() === requestedPath) {
                return new Promise(function () { });
            }

            // Clear initialized charts when changing sections
            if (requestedPath !== m.route.get()) {
                state.initializedCharts.clear();
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