// TODO:
// - Heatmap hover value display
// - Linked hover across charts
// - Linked zoom across charts
// - Improve plot colors
// - Improve plot tick labels
// - Allow both log and non-log line plots
// - Fix the spacing between the heatmap canvas rects (in screen space)
// - Fix the value-size-dependent overflow behavior of the "legend hovers".

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
  }
};

function toTitleCase(str) {
  return str.replace(
    /\w\S*/g,
    text => text.charAt(0).toUpperCase() + text.substring(1).toLowerCase()
  );
}

const Sidebar = {
  view({ attrs }) {
    return m("div#sidebar", [
      attrs.sections.map((section) => m('div.section', m(m.route.Link, { 
        class: attrs.route === section.route ? 'selected' : '', 
        href: section.route,
      }, section.name)))
    ]);
  }
};

const Main = {
  view({ attrs: { route, groups, sections } }) {
    let title;
    switch (route) {
      case '/cpu': title = 'CPU Metrics'; break;
      default: title = toTitleCase(route.slice(1)); break;
    }

    return m("div", 
      m("header", [
        m('h1', 'Rezolus', m('span.div', ' » '), title),
      ]),
      m("main", [
        m(Sidebar, { route, sections }),
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
                grid: { stroke: () => "#333333", }
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

      if (uPlotOpts !== undefined) {
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
        return ({
          view() {
            return m(Main, { ...data, route: requestedPath });
          }
        });
      });
    }
  }
});
