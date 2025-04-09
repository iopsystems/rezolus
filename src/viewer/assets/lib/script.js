// Rezolus Performance Visualization with Apache ECharts
// This replaces the original uPlot implementation

const log = console.log.bind(console);

const state = {
  // for tracking current visualization state
  current: null,
  // Store initialized charts to prevent re-rendering
  initializedCharts: new Map()
};

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

            // Enable brush select for zooming
            chart.dispatchAction({
              type: 'takeGlobalCursor',
              key: 'dataZoomSelect',
              dataZoomSelectActive: true
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

  onupdate: function(vnode) {
    // Update chart if data changed and chart is initialized
    if (vnode.state.chart) {
      const option = createChartOption(vnode.attrs);
      vnode.state.chart.setOption(option);
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
    opts,
    data
  } = plotSpec;

  // Basic option template
  const option = {
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
      start: 0,
      end: 100,
      zoomLock: false
    }, {
      // Brush select zoom
      type: 'slider',
      show: false,
      xAxisIndex: 0,
      filterMode: 'none',
      start: 0,
      end: 100
    }],
    textStyle: {
      color: '#E0E0E0'
    },
    darkMode: true,
    backgroundColor: 'transparent'
  };

  // Handle different plot types
  if (opts.style === 'line') {
    return createLineChartOption(option, plotSpec);
  } else if (opts.style === 'heatmap') {
    return createHeatmapOption(option, plotSpec);
  }

  return option;
}

function createLineChartOption(baseOption, plotSpec) {
  const {
    data
  } = plotSpec;

  if (!data || data.length < 2) {
    return baseOption;
  }

  const timeData = data[0];
  const valueData = data[1];

  // Format time for x-axis
  const formattedTimeData = timeData.map(timestamp => {
    const date = new Date(timestamp * 1000);
    return date.toISOString().replace('T', ' ').substr(0, 19);
  });

  // Return line chart configuration
  return {
    ...baseOption,
    xAxis: {
      type: 'category',
      data: formattedTimeData,
      axisLine: {
        lineStyle: {
          color: '#ABABAB'
        }
      },
      axisLabel: {
        color: '#ABABAB',
        formatter: function(value) {
          // Show just time for short format
          return value.split(' ')[1];
        }
      }
    },
    yAxis: {
      type: 'value',
      scale: true,
      axisLine: {
        lineStyle: {
          color: '#ABABAB'
        }
      },
      axisLabel: {
        color: '#ABABAB',
        formatter: function(value) {
          // Use scientific notation for large/small numbers
          if (Math.abs(value) > 10000 || (Math.abs(value) > 0 && Math.abs(value) < 0.01)) {
            return value.toExponential(1);
          }
          return value;
        }
      },
      splitLine: {
        lineStyle: {
          color: 'rgba(171, 171, 171, 0.2)'
        }
      }
    },
    series: [{
      data: valueData,
      type: 'line',
      name: plotSpec.opts.title,
      showSymbol: false,
      emphasis: {
        focus: 'series'
      },
      lineStyle: {
        width: 2
      },
      animationDuration: 0
    }]
  };
}

function createHeatmapOption(baseOption, plotSpec) {
  const {
    data
  } = plotSpec;

  if (!data || data.length < 3) {
    return baseOption;
  }

  const timeData = data[0]; // X axis (time)
  // Create y-indices correctly accounting for all data rows (CPUs)
  const yIndices = Array.from({
    length: data.length - 1
  }, (_, i) => i); // Y axis (CPU indices)

  // Process data for heatmap format - converts from series of arrays to array of [x, y, value] items
  const heatmapData = [];

  // Start from 1 to skip the time array (data[0])
  for (let y = 1; y < data.length; y++) {
    const rowData = data[y];
    if (!rowData) continue;

    for (let x = 0; x < timeData.length; x++) {
      if (rowData[x] !== undefined && rowData[x] !== null) {
        // Adjust y-index to be zero-based (y-1) since we're skipping the first row (time data)
        heatmapData.push([x, y - 1, rowData[x]]);
      }
    }
  }

  // Format time for x-axis
  const formattedTimeData = timeData.map(timestamp => {
    const date = new Date(timestamp * 1000);
    return date.toISOString().replace('T', ' ').substr(0, 19);
  });

  // Calculate value range for color scale
  let minValue = Infinity;
  let maxValue = -Infinity;

  heatmapData.forEach(item => {
    const value = item[2];
    if (value < minValue) minValue = value;
    if (value > maxValue) maxValue = value;
  });

  return {
    ...baseOption,
    tooltip: {
      position: 'top',
      formatter: function(params) {
        const value = params.data[2];
        const time = formattedTimeData[params.data[0]];
        const cpu = params.data[1];
        return `Time: ${time}<br>CPU: ${cpu}<br>Value: ${value.toFixed(6)}`;
      }
    },
    grid: {
      height: '70%',
      top: '60'
    },
    xAxis: {
      type: 'category',
      data: formattedTimeData,
      splitArea: {
        show: true
      },
      axisLabel: {
        color: '#ABABAB',
        formatter: function(value) {
          // Show just time for short format
          return value.split(' ')[1];
        }
      }
    },
    yAxis: {
      type: 'category',
      data: yIndices,
      splitArea: {
        show: true
      },
      axisLabel: {
        color: '#ABABAB'
      }
    },
    visualMap: {
      min: minValue,
      max: maxValue,
      calculable: false,
      show: false,
      orient: 'horizontal',
      left: 'center',
      bottom: '0%',
      textStyle: {
        color: '#E0E0E0'
      },
      inRange: {
        color: [
          '#440154', '#481a6c', '#472f7d', '#414487', '#39568c',
          '#31688e', '#2a788e', '#23888e', '#1f988b', '#22a884',
          '#35b779', '#54c568', '#7ad151', '#a5db36', '#d2e21b', '#fde725'
        ]
      }
    },
    series: [{
      name: plotSpec.opts.title,
      type: 'heatmap',
      data: heatmapData,
      emphasis: {
        itemStyle: {
          shadowBlur: 10,
          shadowColor: 'rgba(0, 0, 0, 0.5)'
        }
      },
      progressive: 2000,
      animation: false
    }]
  };
}

// Handle the synchronization of cursors between charts
function setupChartSync(charts) {
  // Flag to prevent infinite recursion
  let isSyncing = false;
  // Flag for zoom synchronization
  let isZooming = false;

  charts.forEach(mainChart => {
    // Setup brush events for zooming
    mainChart.on('brushSelected', function(params) {
      if (isZooming) return;
      isZooming = true;

      try {
        // Only handle rectangle brush type (for zooming)
        if (params.brushType === 'rect') {
          // Get the range from the brush
          const areas = params.areas[0];
          if (areas && areas.coordRange) {
            const [start, end] = areas.coordRange;

            // Get x-axis data range
            const xAxis = mainChart.getModel().getComponent('xAxis', 0);
            const axisExtent = xAxis.axis.scale.getExtent();
            const axisRange = axisExtent[1] - axisExtent[0];

            // Calculate percentage
            const startPercent = ((start - axisExtent[0]) / axisRange) * 100;
            const endPercent = ((end - axisExtent[0]) / axisRange) * 100;

            // Apply zoom to all charts
            charts.forEach(chart => {
              chart.dispatchAction({
                type: 'dataZoom',
                start: startPercent,
                end: endPercent
              });

              // Clear the brush
              chart.dispatchAction({
                type: 'brush',
                command: 'clear',
                areas: []
              });
            });
          }
        }
      } finally {
        setTimeout(() => {
          isZooming = false;
        }, 0);
      }
    });

    // Setup double-click handler for zoom reset
    mainChart.getZr().on('dblclick', function() {
      if (isZooming) return;
      isZooming = true;

      try {
        // Reset zoom on all charts
        charts.forEach(chart => {
          chart.dispatchAction({
            type: 'dataZoom',
            start: 0,
            end: 100
          });
        });
      } finally {
        setTimeout(() => {
          isZooming = false;
        }, 0);
      }
    });

    // Sync cursor across charts
    mainChart.on('updateAxisPointer', function(event) {
      // Skip if we're already in a synchronization process
      if (isSyncing) return;

      // Set flag to indicate we're synchronizing
      isSyncing = true;

      try {
        // Update all other charts when this chart's cursor moves
        charts.forEach(chart => {
          if (chart !== mainChart) {
            chart.dispatchAction({
              type: 'updateAxisPointer',
              currTrigger: 'mousemove',
              x: event.topX,
              y: event.topY
            });
          }
        });
      } finally {
        // Reset flag after synchronization
        setTimeout(() => {
          isSyncing = false;
        }, 0);
      }
    });

    // Sync zooming across charts
    mainChart.on('dataZoom', function(event) {
      // Skip if we're already in a zooming process
      if (isZooming) return;

      // Set flag to indicate we're zooming
      isZooming = true;

      try {
        // Only sync zooming actions initiated by user interaction
        if (event.batch) {
          // Get zoom range from the event
          const {
            start,
            end
          } = event.batch[0];

          // Apply the same zoom to all other charts
          charts.forEach(chart => {
            if (chart !== mainChart) {
              chart.dispatchAction({
                type: 'dataZoom',
                start: start,
                end: end,
                // Use 'dataZoomId' from the chart's first dataZoom instance
                dataZoomId: chart.getOption().dataZoom[0].id
              });
            }
          });
        }
      } finally {
        // Reset flag after synchronization
        setTimeout(() => {
          isZooming = false;
        }, 0);
      }
    });
  });
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
            // After the view is rendered, set up chart synchronization
            setTimeout(() => {
              // Only sync charts that are already initialized (visible)
              const syncedCharts = Array.from(state.initializedCharts.values());

              if (syncedCharts.length > 1) {
                setupChartSync(syncedCharts);
              }

              // Set up a MutationObserver to handle new charts being initialized
              const chartContainer = document.getElementById('groups');
              if (chartContainer) {
                const chartObserver = new MutationObserver(() => {
                  // Get all currently initialized charts
                  const currentCharts = Array.from(state.initializedCharts.values());
                  if (currentCharts.length > 1) {
                    setupChartSync(currentCharts);
                  }
                });

                // Observe changes to the chart container
                chartObserver.observe(chartContainer, {
                  subtree: true,
                  childList: true
                });

                // Store for cleanup
                vnode.state.chartObserver = chartObserver;
              }
            }, 100);
          },
          onremove(vnode) {
            // Clean up the observer when changing routes
            if (vnode.state.chartObserver) {
              vnode.state.chartObserver.disconnect();
            }
          }
        });
      });
    }
  }
});