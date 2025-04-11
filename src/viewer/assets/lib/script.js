// Refactored script.js - Main application logic with modular chart components

// Import our modular components
import { PlotOpts } from './plot.js';
import { createLineChartOption } from './line.js';
import { createScatterChartOption } from './scatter.js';
import { createHeatmapOption } from './heatmap.js';
import { 
  formatDateTime, 
  isChartVisible, 
  updateChartsAfterZoom,
  setupChartSync
} from './utils.js';

// Application state management
const state = {
  // for tracking current visualization state
  current: null,
  // Store initialized charts to prevent re-rendering
  initializedCharts: new Map(),
  // Global zoom state to apply to all charts
  globalZoom: {
    start: 0,
    end: 100,
    isZoomed: false
  },
  // Flag to prevent recursive synchronization
  isZoomSyncing: false,
  // Flag to prevent recursive cursor updates
  isCursorSyncing: false,
  // Track which charts need zoom update (for lazy updating)
  chartsNeedingZoomUpdate: new Set(),
  // Store shared axis tick settings for consistency across charts
  sharedAxisConfig: {
    // Track visible tick indices for consistent tick spacing
    visibleTicks: [],
    // Store last update timestamp to avoid too frequent recalculations
    lastUpdate: 0
  }
};

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

// Plot component that renders ECharts visualizations with lazy loading
const Plot = {
  oncreate: function(vnode) {
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
            // Store chart instance for cleanup and to prevent re-initialization
            state.initializedCharts.set(chartId, chart);

            // Configure and render the chart based on plot style
            const option = createChartOption(attrs);
            chart.setOption(option);

            // Apply global zoom state if it exists
            if (state.globalZoom.isZoomed) {
              chart.dispatchAction({
                type: 'dataZoom',
                start: state.globalZoom.start,
                end: state.globalZoom.end
              });
            }

            // Enable brush select for zooming
            chart.dispatchAction({
              type: 'takeGlobalCursor',
              key: 'dataZoomSelect',
              dataZoomSelectActive: true
            });

            // Add this chart to the chart sync system
            setupChartSync([chart], state);

            // Store chart in vnode state for updates and cleanup
            vnode.state.chart = chart;
          } else {
            // Chart was already initialized, just reference it
            vnode.state.chart = state.initializedCharts.get(chartId);

            // Check if this chart needs a zoom update
            if (state.chartsNeedingZoomUpdate.has(chartId)) {
              vnode.state.chart.dispatchAction({
                type: 'dataZoom',
                start: state.globalZoom.start,
                end: state.globalZoom.end
              });

              // Remove from charts needing update
              state.chartsNeedingZoomUpdate.delete(chartId);
            }
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

  onupdate: function(vnode) {
    // Update chart if data changed and chart is initialized
    if (vnode.state.chart) {
      const option = createChartOption(vnode.attrs);
      vnode.state.chart.setOption(option);

      // Check if this chart needs a zoom update
      const chartId = vnode.attrs.opts.id;
      if (state.chartsNeedingZoomUpdate.has(chartId)) {
        vnode.state.chart.dispatchAction({
          type: 'dataZoom',
          start: state.globalZoom.start,
          end: state.globalZoom.end
        });

        // Remove from charts needing update
        state.chartsNeedingZoomUpdate.delete(chartId);
      }
    }
  },

  onremove: function(vnode) {
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

  view: function() {
    return m('div.plot');
  }
};

// Create ECharts options based on plot type
function createChartOption(plotSpec) {
  const {
    opts
  } = plotSpec;

  // Basic option template
  const baseOption = {
    grid: {
      left: '5%',
      right: '5%',
      top: '40',
      bottom: '40',
      containLabel: true
    },
    tooltip: {
      trigger: 'axis',
      axisPointer: {
        type: 'cross',
        animation: false,
        label: {
          backgroundColor: '#505765'
        }
      }
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
      start: state.globalZoom.start,
      end: state.globalZoom.end,
      zoomLock: false
    }, {
      // Brush select zoom
      type: 'slider',
      show: false,
      xAxisIndex: 0,
      filterMode: 'none',
      start: state.globalZoom.start,
      end: state.globalZoom.end
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
  }

  return baseOption;
}

// Main application entry point
m.route.prefix = ""; // use regular paths for navigation, eg. /overview
m.route(document.body, "/overview", {
  "/:section": {
    onmatch(params, requestedPath) {
      // Prevent a route change if we're already on this route
      if (m.route.get() === requestedPath) {
        return new Promise(function() {});
      }

      // Clear initialized charts when changing sections
      if (requestedPath !== m.route.get()) {
        state.initializedCharts.clear();

        // Reset global zoom state when changing sections
        state.globalZoom = {
          start: 0,
          end: 100,
          isZoomed: false
        };

        // Clear the charts needing update set
        state.chartsNeedingZoomUpdate.clear();
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
            return m(Main, {...data,
              activeSection
            });
          },
          oncreate(vnode) {
            // Set up scroll handler to check for charts needing updates
            const scrollHandler = () => {
              if (state.chartsNeedingZoomUpdate.size > 0 && !state.isZoomSyncing) {
                // Get all chart IDs that need updates
                const chartIdsToUpdate = [...state.chartsNeedingZoomUpdate];

                // Check each chart if it's now visible
                for (const chartId of chartIdsToUpdate) {
                  const chart = state.initializedCharts.get(chartId);
                  if (chart && isChartVisible(chart.getDom())) {
                    // Chart is now visible, apply zoom update
                    try {
                      state.isZoomSyncing = true;
                      chart.dispatchAction({
                        type: 'dataZoom',
                        start: state.globalZoom.start,
                        end: state.globalZoom.end
                      });

                      // Remove from update list
                      state.chartsNeedingZoomUpdate.delete(chartId);
                    } finally {
                      setTimeout(() => {
                        state.isZoomSyncing = false;
                      }, 0);
                    }
                  }
                }
              }
            };

            // Attach scroll listener
            window.addEventListener('scroll', scrollHandler, {
              passive: true
            });
            vnode.state.scrollHandler = scrollHandler;
          },
          onremove(vnode) {
            // Clean up scroll handler
            if (vnode.state.scrollHandler) {
              window.removeEventListener('scroll', vnode.state.scrollHandler);
            }
          }
        });
      });
    }
  }
});