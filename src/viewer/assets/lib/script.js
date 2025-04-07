// TODO:
// - Heatmap hover value display
// - Linked hover across charts
// - Linked zoom across charts
// - Improve plot colors
// - Improve plot tick labels
// - Allow both log and non-log line plots
// - Fix the spacing between the heatmap canvas rects (in screen space)
// - Fix the value-size-dependent overflow behavior of the "legend hovers".
// - Allow a "href" specification for group names, so eg. the overview groups can link to their respective sections

import uPlot from './uPlot.esm.js';

const log = console.log.bind(console);

const state = {
  // for synchronizing plot state (eg. hovers)
  sync: uPlot.sync("groups"),
};

const cursorSyncOpts = {
  key: state.sync.key,
  setSeries: true,
  match: [
    (own, ext) => own == ext, // x
    (own, ext) => own == ext, // y
  ],
  filters: {
    pub(type) {
      return type != "mouseup" && type != "mousedown";
    },
  },
  scales: ["x", null], // Explicitly tell sync which scales to sync
  values: [null, null],
};

const Sidebar = {
  view({ attrs }) {
    return m("div#sidebar", [
      attrs.sections.map((section) => m('div.section', m(m.route.Link, { 
        class: attrs.activeSection === section ? 'selected' : '', 
        href: section.route,
      }, section.name)))
    ]);
  }
};

const Main = {
  view({ attrs: { activeSection, groups, sections } }) {
    return m("div", 
      m("header", [
        m('h1', 'Rezolus', m('span.div', ' » '), activeSection.name),
      ]),
      m("main", [
        m(Sidebar, { activeSection, sections }),
        m('div#groups', 
          groups.map((group) => m(Group, group))
        )
      ]));
  }
};

const Group = {
  view({ attrs }) {
    return m("div.group", { id: attrs.id }, [
      m("h2", `${attrs.name}`),
      m("div.plots", attrs.plots.map(spec => m(Plot, spec))),
    ]);
  }
};

function syncZoom(plots) {
  let xMin = null;
  let xMax = null;
  let zooming = false;

  function onZoom(u) {
    if (zooming) return; // Prevent recursion

    zooming = true;

    const scales = u.scales.x;

    const newMin = scales.min;
    const newMax = scales.max;

    // Update all other plots to match this min/max
    plots.forEach(p => {
      if (p !== u && p.scales.x) {
        p.setScale('x', {
          min: newMin,
          max: newMax
        });
      }
    });

    zooming = false;
  }

  return onZoom;
}

function throttle(func, limit) {
  let inThrottle;
  let lastFunc;
  let lastRan;

  return function() {
    const context = this;
    const args = arguments;

    if (!inThrottle) {
      func.apply(context, args);
      lastRan = Date.now();
      inThrottle = true;
    } else {
      clearTimeout(lastFunc);
      lastFunc = setTimeout(function() {
        if ((Date.now() - lastRan) >= limit) {
          func.apply(context, args);
          lastRan = Date.now();
        }
      }, limit - (Date.now() - lastRan));
    }
  };
}

function formatTime() {
  // Custom time formatter for 24-hour format
  const timeFormat = uPlot.fmtDate("{HH}:{mm}:{ss}");
  const dateFormat = uPlot.fmtDate("{YYYY}-{MM}-{DD}");

  // Combined format with newline
  return (self, splits, axisIdx, foundSpace, foundIncr) => {
    return splits.map(split => {
      const date = new Date(split * 1000); // convert seconds to milliseconds
      return timeFormat(date) + "\n" + dateFormat(date);
    });
  };
}

function Plot() {
  let resizeObserver, plot;

  // TODO:
  // - for updates, see plot.setData(data, resetScales)
  // - figure out if we need to do anything onremove (I don't think so?)

  return {
    oncreate: function (vnode) {
      const { attrs } = vnode;

      let uPlotOpts, uPlotData;
      switch (attrs.opts.style) {
        case 'line':
          uPlotOpts = {
            ...attrs.opts,
            cursor: {
              lock: true,
              focus: { prox: 16, },
              bind: {
                // throttling to reduce processing load and smooth mouse
                // movement over data-dense area
                mousemove: (self, targ, handler) => {
                  const throttledHandler = throttle((e) => {
                    handler(e);
                  }, 35);

                  return (e) => {
                    throttledHandler(e);
                  };
                },
                // For other events, use the default behavior
                mousedown: (self, targ, handler) => (e) => handler(e),
                mouseup: (self, targ, handler) => (e) => handler(e),
                click: (self, targ, handler) => (e) => handler(e),
                dblclick: (self, targ, handler) => (e) => handler(e),
                mouseenter: (self, targ, handler) => (e) => handler(e),
                mouseleave: (self, targ, handler) => (e) => handler(e),
              },
              sync: cursorSyncOpts,
            },
            series: 
              attrs.data.map((d, i) => i === 0 ? {
                // X-axis
                label: "Time",
                scale: "x"
              } : i === 1 ? {
                // First line
                label: "Line 1",
                stroke: "red",
                width: 2
              } : i >= 2 ? 
                {
                  // Second line
                  label: "Line 2",
                  stroke: "blue",
                  width: 2
                } : null
              ),
            axes: [
              {
                // X axis options
                label: "Time",
                stroke: () => "#ABABAB",
                ticks: { stroke: () => "#333333", },
                grid: { stroke: () => "#333333", },
                values: formatTime()
              },
              {
                // Y axis options
                label: "Value",
                stroke: () => "#ABABAB",
                ticks: { stroke: () => "#333333", },
                grid: { stroke: () => "#333333", },
                scale: "y",
                values: (self, ticks) => {
                  // Format the tick values in scientific notation
                  return ticks.map(v => v ? v.toExponential(1) : v); // 1 decimal place in exponent
                }
              },
            ],
            scales: {
              y: {
                log: 10,
                distr: 3, // 1 = linear, 2 = ordinal, 3 = log, 4 = asinh, 100 = custom
              },
            }
          };
          uPlotData = attrs.data;
          break;
        case 'heatmap':
          // code mostly adapted from https://leeoniya.github.io/uPlot/demos/latency-heatmap.html
          function heatmapPaths(opts) {
            const { disp } = opts;

            return (u, seriesIdx, idx0, idx1) => {
              uPlot.orient(u, seriesIdx, (series, dataX, dataY, scaleX, scaleY, valToPosX, valToPosY, xOff, yOff, xDim, yDim, moveTo, lineTo, rect, arc) => {
                let d = u.data[seriesIdx];
                let [xs, ys, counts] = d;
                let dlen = xs.length;

                // fill colors are mapped from interpolating densities / counts along some gradient
                // (should be quantized to 64 colors/levels max. e.g. 16)
                let fills = disp.fill.values(u, seriesIdx);

                let fillPalette = disp.fill.lookup ?? [...new Set(fills)];

                let fillPaths = fillPalette.map(color => new Path2D());

                // detect x and y bin qtys by detecting layout repetition in x & y data
                let yBinQty = dlen - ys.lastIndexOf(ys[0]);
                let xBinQty = dlen / yBinQty;
                let yBinIncr = ys[1] - ys[0];
                let xBinIncr = xs[yBinQty] - xs[0];

                // uniform tile sizes based on zoom level
                let xSize = valToPosX(xBinIncr, scaleX, xDim, xOff) - valToPosX(0, scaleX, xDim, xOff);
                let ySize = valToPosY(yBinIncr, scaleY, yDim, yOff) - valToPosY(0, scaleY, yDim, yOff);

                // pre-compute x and y offsets
                let cys = ys.slice(0, yBinQty).map(y => Math.round(valToPosY(y, scaleY, yDim, yOff) - ySize / 2));
                let cxs = Array.from({ length: xBinQty }, (v, i) => Math.round(valToPosX(xs[i * yBinQty], scaleX, xDim, xOff) - xSize / 2));
       
                for (let i = 0; i < dlen; i++) {
                  // filter out 0 counts and out of view
                  if (
                    counts[i] > 0 &&
                    xs[i] >= scaleX.min && xs[i] <= scaleX.max &&
                    ys[i] >= scaleY.min && ys[i] <= scaleY.max
                  ) {
                    let cx = cxs[~~(i / yBinQty)];
                    let cy = cys[i % yBinQty];

                    let fillPath = fillPaths[fills[i]];

                    rect(fillPath, cx, cy, xSize, ySize);
                  }
                }

                u.ctx.save();
                u.ctx.rect(u.bbox.left, u.bbox.top, u.bbox.width, u.bbox.height);
                u.ctx.clip();
                fillPaths.forEach((p, i) => {
                  u.ctx.fillStyle = fillPalette[i];
                  u.ctx.fill(p);
                });
                u.ctx.restore();
              });
            };
          }

          // 16-color gradient (viridis)
          const colors = [
            "#440154",
            "#481a6c",
            "#472f7d",
            "#414487",
            "#39568c",
            "#31688e",
            "#2a788e",
            "#23888e",
            "#1f988b",
            "#22a884",
            "#35b779",
            "#54c568",
            "#7ad151",
            "#a5db36",
            "#d2e21b",
            "#fde725"
          ];

          let palette = colors;

          const countsToFills = (u, seriesIdx) => {
            let counts = u.data[seriesIdx][2];

            // TODO: integrate 1e-9 hideThreshold?
            const hideThreshold = 0;

            let minCount = Infinity;
            let maxCount = -Infinity;

            for (let i = 0; i < counts.length; i++) {
              if (counts[i] > hideThreshold) {
                minCount = Math.min(minCount, counts[i]);
                maxCount = Math.max(maxCount, counts[i]);
              }
            }

            let range = maxCount - minCount;

            let paletteSize = palette.length;

            let indexedFills = Array(counts.length);

            for (let i = 0; i < counts.length; i++)
              indexedFills[i] = counts[i] === 0 ? -1 : Math.min(paletteSize - 1, Math.floor((paletteSize * (counts[i] - minCount)) / range));

            return indexedFills;
          };

          // note: assumes nonempty data
          const data = attrs.data;
          const timeData = data[0];
          const rows = data.slice(1);
          const numRows = rows.length;
          const numCols = rows[0].length;

          // Flatten the 2D data matrix into triples: (xValues, yValues, zValues)
          const xValues = [];
          const yValues = [];
          const zValues = [];

          // This is an inefficient access pattern but it looks like uPlot requires
          // sorted data (even if you specify sorted: false for the x facet)
          for (let colIndex = 0; colIndex < numCols; colIndex++) {
            for (let rowIndex = 0; rowIndex < numRows; rowIndex++) {
              const row = rows[rowIndex];
              // Quantize time samples to display on the grid
              xValues.push(Math.round(timeData[colIndex] + 0.5));
              yValues.push(rowIndex);
              zValues.push(row[colIndex]);
            }
          }

          uPlotOpts = {
            ...attrs.opts,
            mode: 2,
            ms: 1e-3,
            cursor: {
              points: { show: false },
              drag: { x: true, y: false },
              lock: true,
              focus: { prox: 16, },
              sync: cursorSyncOpts,
            },
            scales: {
              x: { time: true, }
            },
            axes: [
              {
                // X axis options
                label: "Time",
                stroke: () => "#ABABAB",
                ticks: { stroke: () => "#333333", },
                grid: { show: false },
                scale: 'x',
                values: formatTime()
              },
              {
                // Y axis options
                label: "Value",
                stroke: () => "#ABABAB",
                ticks: { stroke: () => "#333333", },
                grid: { show: false },
                scale: "y",
              },
            ],
            series: [
              {},
              {
                label: "Value",
                paths: heatmapPaths({
                  disp: {
                    fill: {
                      lookup: colors,
                      values: countsToFills
                    }
                  }
                }),
                facets: [
                  {
                    scale: 'x',
                    auto: true,
                    sorted: true,
                  },
                  {
                    scale: 'y',
                    auto: true,
                  },
                ],
              },
            ],
          };
          uPlotData = [null, [xValues, yValues, zValues]];
          break;
        default:
          throw new Error(`undefined style in provided plot opts: ${attrs.opts.style}`);
      }

      if (uPlotOpts) {
        // Enable zooming on all plots
        uPlotOpts.hooks = uPlotOpts.hooks || {};

        // Hook into setScale to detect when a user zooms
        // This is the key part that synchronizes the zoom
        const existingSetScale = uPlotOpts.hooks.setScale;

        uPlotOpts.hooks.setScale = (u, key) => {
          if (existingSetScale)
            existingSetScale(u, key);

          // Only sync zoom for x-axis changes
          if (key === 'x') {
            // Get all plots that are part of the sync group
            const syncKey = state.sync.key;
            const plots = syncKey != null ? uPlot.sync(syncKey).plots : [];

            // Call the syncZoom handler to update all other plots
            syncZoom(plots)(u);
          }
        };
      }

      if (uPlotOpts !== undefined) {
        // Add the sync plugin for zoom
        let zoomingSync = false;

        // Initialize hooks object properly
        uPlotOpts.hooks = uPlotOpts.hooks || {};

        // Add setScale hook to synchronize zoom
        uPlotOpts.hooks.setScale = [(u, scaleKey) => {
          // Only sync x-axis changes and prevent recursion
          if (scaleKey === 'x' && !zoomingSync) {
            zoomingSync = true;

            const syncKey = state.sync.key;
            const plots = syncKey != null ? uPlot.sync(syncKey).plots : [];

            // Get the new scale values
            const newMin = u.scales.x.min;
            const newMax = u.scales.x.max;

            // Update all other plots to match
            plots.forEach(p => {
              if (p !== u && p.scales.x) {
                p.setScale('x', {
                  min: newMin,
                  max: newMax
                });
              }
            });

            zoomingSync = false;
          }
        }];

        // Add double-click hook to reset zoom on all plots
        uPlotOpts.hooks.dblclick = [(u, e) => {
          if (!zoomingSync) {
            zoomingSync = true;

            const syncKey = state.sync.key;
            const plots = syncKey != null ? uPlot.sync(syncKey).plots : [];

            // Reset all plots
            plots.forEach(p => {
              if (p !== u) {
                p.setScale('x', null);
                p.redraw();
              }
            });

            zoomingSync = false;
          }
        }];

        plot = new uPlot(uPlotOpts, uPlotData, vnode.dom);
        state.sync.sub(plot);

        // We maintain a resize observer per plot.
        resizeObserver = new ResizeObserver(entries => {
          for (const entry of entries) {
            plot.setSize(entry.contentRect);
          }
        });
        resizeObserver.observe(vnode.dom);
      }
    },

    onremove(vnode) {
      resizeObserver?.disconnect();
      resizeObserver = null;
    },

    view: function () {
      return m('div.plot');
    }
  };
}

m.route.prefix = ""; // use regular paths for nagivation, eg. /overview
m.route(document.body, "/overview", {
  "/:section": {
    onmatch(params, requestedPath) {
      // Prevent a route change if we're already on this route
      if (m.route.get() === requestedPath) {
        return new Promise(function () {});
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
            return m(Main, { ...data, activeSection });
          }
        });
      });
    }
  }
});
