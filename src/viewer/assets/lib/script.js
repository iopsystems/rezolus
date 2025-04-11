// Helper function to format dates consistently across chart types
function formatDateTime(timestamp, format = 'time') {
  const date = new Date(timestamp * 1000);
  const isoString = date.toISOString().replace('T', ' ').substr(0, 19);

  if (format === 'time') {
    // Return only the time portion (HH:MM:SS)
    return isoString.split(' ')[1];
  } else if (format === 'short') {
    // Return HH:MM format for compact display
    return isoString.split(' ')[1].substr(0, 5);
  } else {
    // Return the full datetime
    return isoString;
  }
}

// Helper function to check if a chart element is visible in the viewport
function isChartVisible(chartDom) {
  if (!chartDom) return false;

  const rect = chartDom.getBoundingClientRect();
  const windowHeight = window.innerHeight || document.documentElement.clientHeight;
  const windowWidth = window.innerWidth || document.documentElement.clientWidth;

  // Consider charts partially in view to be visible
  return (
    rect.top <= windowHeight &&
    rect.bottom >= 0 &&
    rect.left <= windowWidth &&
    rect.right >= 0
  );
}

// Calculate shared visible ticks for consistent tick spacing across all charts
function calculateSharedVisibleTicks(dataLength, zoomStart, zoomEnd) {
  // Full view zoom (special case to prevent label pile-up)
  if (zoomStart === 0 && zoomEnd === 100) {
    // For full view, create fewer evenly spaced ticks
    const maxTicks = Math.min(8, dataLength);
    const interval = Math.max(1, Math.floor(dataLength / maxTicks));

    const ticks = [];
    for (let i = 0; i < dataLength; i += interval) {
      ticks.push(i);
    }

    // Add last tick if not already included
    if (dataLength > 0 && (dataLength - 1) % interval !== 0) {
      ticks.push(dataLength - 1);
    }

    return ticks;
  }

  // Normal zoom case:
  // Convert start and end percentages to indices
  let startIdx = Math.floor(dataLength * (zoomStart / 100));
  let endIdx = Math.ceil(dataLength * (zoomEnd / 100));

  // Ensure bounds
  startIdx = Math.max(0, startIdx);
  endIdx = Math.min(dataLength - 1, endIdx);

  // Calculate number of visible data points
  const visiblePoints = endIdx - startIdx;

  // Determine desired number of ticks - 8-10 is usually good for readability
  const desiredTicks = Math.min(10, Math.max(4, visiblePoints));

  // Calculate tick interval
  const interval = Math.max(1, Math.floor(visiblePoints / desiredTicks));

  // Generate tick array
  const ticks = [];
  for (let i = startIdx; i <= endIdx; i += interval) {
    ticks.push(i);
  }

  // Ensure we have the end tick if not already included
  if (ticks.length > 0 && ticks[ticks.length - 1] !== endIdx) {
    ticks.push(endIdx);
  }

  return ticks;
}

// Directly force chart updates after zooming - simpler approach
function updateChartsAfterZoom(start, end) {
  // Clear existing tick configuration
  state.sharedAxisConfig.visibleTicks = [];
  state.sharedAxisConfig.lastUpdate = 0;

  // Get the first chart to calculate shared ticks (if available)
  let sharedTicks = [];
  const firstChart = state.initializedCharts.values().next().value;
  if (firstChart) {
    const chartOption = firstChart.getOption();
    if (chartOption.xAxis && chartOption.xAxis[0] && chartOption.xAxis[0].data) {
      const dataLength = chartOption.xAxis[0].data.length;
      sharedTicks = calculateSharedVisibleTicks(dataLength, start, end);

      // Store in shared config for other charts
      state.sharedAxisConfig.visibleTicks = sharedTicks;
      state.sharedAxisConfig.lastUpdate = Date.now();
    }
  }

  // Update all charts with new zoom level
  state.initializedCharts.forEach((chart) => {
    // Apply zoom to all charts
    chart.dispatchAction({
      type: 'dataZoom',
      start: start,
      end: end
    });

    // Update the chart with new axis configuration
    chart.setOption({
      xAxis: {
        axisLabel: {
          interval: function(index) {
            return state.sharedAxisConfig.visibleTicks.includes(index);
          }
        }
      }
    });
  });
}

// Rezolus Performance Visualization with Apache ECharts
const log = console.log.bind(console);

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
            setupChartSync([chart]);

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

  // Handle different plot types
  if (opts.style === 'line') {
    return createLineChartOption(option, plotSpec);
  } else if (opts.style === 'heatmap') {
    return createHeatmapOption(option, plotSpec);
  } else if (opts.style === 'scatter') {
    return createScatterChartOption(option, plotSpec);
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

  // For line charts, we expect the classic 2-row format: [times, values]
  const timeData = data[0];

  // Use consistent formatting for timestamps
  const formattedTimeData = timeData.map(timestamp => formatDateTime(timestamp, 'time'));

  const valueData = data[1];

  // Use shared ticks if already calculated, otherwise calculate new ones
  if (state.sharedAxisConfig.visibleTicks.length === 0 ||
    Date.now() - state.sharedAxisConfig.lastUpdate > 1000) {
    // For full view (no zoom), use fewer ticks to prevent label pile-up
    if (state.globalZoom.start === 0 && state.globalZoom.end === 100) {
      const maxTicks = Math.min(8, timeData.length);
      const interval = Math.max(1, Math.floor(timeData.length / maxTicks));

      const ticks = [];
      for (let i = 0; i < timeData.length; i += interval) {
        ticks.push(i);
      }

      // Add last tick if not already included
      if (timeData.length > 0 && (timeData.length - 1) % interval !== 0) {
        ticks.push(timeData.length - 1);
      }

      state.sharedAxisConfig.visibleTicks = ticks;
    } else {
      // For zoomed view, calculate normal ticks
      state.sharedAxisConfig.visibleTicks = calculateSharedVisibleTicks(
        timeData.length,
        state.globalZoom.start,
        state.globalZoom.end
      );
    }
    state.sharedAxisConfig.lastUpdate = Date.now();
  }

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
          return value; // Already formatted properly by formatDateTime
        },
        // Using custom ticks based on our shared configuration
        interval: function(index) {
          return state.sharedAxisConfig.visibleTicks.includes(index);
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
    time_data,
    data,
    min_value,
    max_value
  } = plotSpec;

  if (!data || data.length < 1) {
    return baseOption;
  }

  // Get unique x indices (timestamps) and y indices (CPUs)
  const xIndices = new Set();
  const yIndices = new Set();

  // Extract all unique CPU IDs and timestamp indices
  data.forEach(item => {
    xIndices.add(item[0]); // timestamp index
    yIndices.add(item[1]); // CPU ID
  });

  // Convert to array and sort numerically
  const cpuIds = Array.from(yIndices).sort((a, b) => a - b);

  // Ensure we have a continuous range of CPUs from 0 to max
  const maxCpuId = cpuIds.length > 0 ? Math.max(...cpuIds) : 0;
  const continuousCpuIds = Array.from({
    length: maxCpuId + 1
  }, (_, i) => i);

  // Use consistent formatting for time values
  const formattedTimeData = time_data.map(timestamp => formatDateTime(timestamp, 'time'));

  // Calculate min/max values if not provided by backend
  let minValue = min_value !== undefined ? min_value : Infinity;
  let maxValue = max_value !== undefined ? max_value : -Infinity;

  if (minValue === Infinity || maxValue === -Infinity) {
    data.forEach(item => {
      const value = item[2];
      minValue = Math.min(minValue, value);
      maxValue = Math.max(maxValue, value);
    });
  }

  // Use shared ticks for formatting
  if (state.sharedAxisConfig.visibleTicks.length === 0 ||
    Date.now() - state.sharedAxisConfig.lastUpdate > 1000) {
    // For full view (no zoom), use fewer ticks to prevent label pile-up
    if (state.globalZoom.start === 0 && state.globalZoom.end === 100) {
      const maxTicks = Math.min(8, time_data.length);
      const interval = Math.max(1, Math.floor(time_data.length / maxTicks));

      const ticks = [];
      for (let i = 0; i < time_data.length; i += interval) {
        ticks.push(i);
      }

      // Add last tick if not already included
      if (time_data.length > 0 && (time_data.length - 1) % interval !== 0) {
        ticks.push(time_data.length - 1);
      }

      state.sharedAxisConfig.visibleTicks = ticks;
    } else {
      // For zoomed view, calculate normal ticks
      state.sharedAxisConfig.visibleTicks = calculateSharedVisibleTicks(
        time_data.length,
        state.globalZoom.start,
        state.globalZoom.end
      );
    }
    state.sharedAxisConfig.lastUpdate = Date.now();
  }

  // Ensure maxValue is always at least slightly higher than minValue for visualization
  if (maxValue === minValue) {
    maxValue = minValue + 0.001;
  }

  return {
    ...baseOption,
    tooltip: {
      position: 'top',
      formatter: function(params) {
        const value = params.data[2];
        const timeIndex = params.data[0];
        const fullTime = time_data[timeIndex];
        const formattedTime = formatDateTime(fullTime, 'full'); // Use full format for tooltip
        const cpu = params.data[1];
        return `Time: ${formattedTime}<br>CPU: ${cpu}<br>Value: ${value.toFixed(6)}`;
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
          // Show time only format for x-axis labels, already properly formatted
          return value;
        },
        // Use the same tick interval configuration as line charts
        interval: function(index) {
          return state.sharedAxisConfig.visibleTicks.includes(index);
        }
      }
    },
    yAxis: {
      type: 'category',
      data: continuousCpuIds, // Use the continuous range of CPU IDs
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
      data: data,
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

// Create a scatter chart option for percentile data
function createScatterChartOption(baseOption, plotSpec) {
  const {
    data
  } = plotSpec;

  if (!data || data.length < 2) {
    return baseOption;
  }

  // For percentile data, the format is [times, percentile1Values, percentile2Values, ...]
  const timeData = data[0];

  // Use consistent formatting for timestamps
  const formattedTimeData = timeData.map(timestamp => formatDateTime(timestamp, 'time'));

  // Use shared ticks if already calculated, otherwise calculate new ones
  if (state.sharedAxisConfig.visibleTicks.length === 0 ||
    Date.now() - state.sharedAxisConfig.lastUpdate > 1000) {
    // For full view (no zoom), use fewer ticks to prevent label pile-up
    if (state.globalZoom.start === 0 && state.globalZoom.end === 100) {
      const maxTicks = Math.min(8, timeData.length);
      const interval = Math.max(1, Math.floor(timeData.length / maxTicks));

      const ticks = [];
      for (let i = 0; i < timeData.length; i += interval) {
        ticks.push(i);
      }

      // Add last tick if not already included
      if (timeData.length > 0 && (timeData.length - 1) % interval !== 0) {
        ticks.push(timeData.length - 1);
      }

      state.sharedAxisConfig.visibleTicks = ticks;
    } else {
      // For zoomed view, calculate normal ticks
      state.sharedAxisConfig.visibleTicks = calculateSharedVisibleTicks(
        timeData.length,
        state.globalZoom.start,
        state.globalZoom.end
      );
    }
    state.sharedAxisConfig.lastUpdate = Date.now();
  }

  // Create series for each percentile
  const series = [];

  // Determine percentiles based on the data structure
  // Assuming data format: [timestamps, p50values, p99values, ...]
  const percentileLabels = ['p50', 'p90', 'p99', 'p99.9', 'p99.99']; // Default labels, can be customized

  for (let i = 1; i < data.length; i++) {
    const percentileData = [];
    const percentileValues = data[i];

    // Create data points in the format [time, value]
    for (let j = 0; j < timeData.length; j++) {
      if (percentileValues[j] !== undefined && !isNaN(percentileValues[j])) {
        percentileData.push([formattedTimeData[j], percentileValues[j]]);
      }
    }

    // Add a series for this percentile
    series.push({
      name: percentileLabels[i - 1] || `Percentile ${i}`,
      type: 'scatter',
      data: percentileData,
      symbolSize: 6,
      emphasis: {
        focus: 'series',
        itemStyle: {
          shadowBlur: 10,
          shadowColor: 'rgba(255, 255, 255, 0.5)'
        }
      }
    });
  }

  // Return scatter chart configuration
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
          return value; // Already formatted properly by formatDateTime
        },
        // Using custom ticks based on our shared configuration
        interval: function(index) {
          return state.sharedAxisConfig.visibleTicks.includes(index);
        }
      }
    },
    yAxis: {
      type: 'log',
      logBase: 10,
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
    series: series
  };
}

// Handle the synchronization of cursors between charts
function setupChartSync(charts) {
  charts.forEach(mainChart => {
    // Setup brush events for zooming
    mainChart.on('brushSelected', function(params) {
      // Prevent infinite recursion
      if (state.isZoomSyncing) return;

      try {
        // Set synchronization flag
        state.isZoomSyncing = true;

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

            // Update the global zoom state
            state.globalZoom.start = startPercent;
            state.globalZoom.end = endPercent;
            state.globalZoom.isZoomed = true;

            // Apply zoom only to visible charts, mark others for lazy update
            state.initializedCharts.forEach((chart, chartId) => {
              const chartDom = chart.getDom();

              if (isChartVisible(chartDom)) {
                // Update visible charts immediately
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

                // Remove from charts needing update
                state.chartsNeedingZoomUpdate.delete(chartId);
              } else {
                // Mark invisible charts for lazy update
                state.chartsNeedingZoomUpdate.add(chartId);
              }
            });
          }
        }
      } finally {
        // Reset flag after a short delay
        setTimeout(() => {
          state.isZoomSyncing = false;
        }, 0);
      }
    });

    // Setup double-click handler for zoom reset
    mainChart.getZr().on('dblclick', function() {
      // Prevent infinite recursion
      if (state.isZoomSyncing) return;

      try {
        // Set synchronization flag
        state.isZoomSyncing = true;

        // Reset the global zoom state
        state.globalZoom.start = 0;
        state.globalZoom.end = 100;
        state.globalZoom.isZoomed = false;

        // Clear the charts needing update set
        state.chartsNeedingZoomUpdate.clear();

        // Reset shared tick configuration to force recalculation
        state.sharedAxisConfig.visibleTicks = [];
        state.sharedAxisConfig.lastUpdate = 0;

        // Use the dedicated function to update all charts with reset zoom
        updateChartsAfterZoom(0, 100);
      } finally {
        // Reset flag after a short delay
        setTimeout(() => {
          state.isZoomSyncing = false;
        }, 0);
      }
    });

    // Sync cursor across charts
    mainChart.on('updateAxisPointer', function(event) {
      // Prevent infinite recursion
      if (state.isCursorSyncing) return;

      try {
        // Set synchronization flag
        state.isCursorSyncing = true;

        // Update all other charts when this chart's cursor moves
        state.initializedCharts.forEach(chart => {
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
        // Reset flag after a short delay
        setTimeout(() => {
          state.isCursorSyncing = false;
        }, 0);
      }
    });

    // Sync zooming across charts and update global state
    mainChart.on('dataZoom', function(event) {
      // Prevent infinite recursion by using a flag
      if (state.isZoomSyncing) return;

      // Only sync zooming actions initiated by user interaction
      if (event.batch) {
        try {
          // Set synchronization flag to prevent recursion
          state.isZoomSyncing = true;

          // Get zoom range from the event
          const {
            start,
            end
          } = event.batch[0];

          // Update global zoom state
          state.globalZoom.start = start;
          state.globalZoom.end = end;
          state.globalZoom.isZoomed = start !== 0 || end !== 100;

          // Update all charts with new zoom level and tick settings
          updateChartsAfterZoom(start, end);
        } finally {
          // Always reset the synchronization flag to allow future events
          setTimeout(() => {
            state.isZoomSyncing = false;
          }, 0);
        }
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