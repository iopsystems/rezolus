import { heatmapPlugin } from './heatmap.js';
import uPlot from './uPlot.esm.js';

const log = console.log.bind(console);

const layout = {
  sections: [
    { name: "Summary", id: "summary", plots: [1, 2] },
    { name: "CPU", id: "cpu", plots: [1, 2] },
    { name: "GPU", id: "gpu", plots: [1, 2] },
    { name: "Network", id: "network", plots: [1, 2] },
    { name: "Block I/O", id: "block-io", plots: [1, 2] },
  ]
};

const Sidebar = {
  view({ attrs }) {
    return m("div#sidebar", [
      // todo: check slash thungs
      attrs.sections.map((section) => m('div.section', m(m.route.Link, { class: attrs.route === section.route || (attrs.route === '/overview' && section.route === '/') ? 'selected' : '', href: section.route, }, section.name)))
    ]);
  }
};

const Main = {
  view({ attrs: { route, groups, sections } }) {
    return m("div", 
      m("header", [
        m('h1', 'Rezolus'),
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

  // todo: for updates, see plot.setData(data, resetScales)

  return {
    oncreate: function (vnode) {
      const { attrs } = vnode;

      let uPlotOpts, uPlotData;
      switch (attrs.opts.style) {
        case 'line':
          uPlotOpts = {
            ...attrs.opts,

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

          // 16-color gradient (white -> orange -> red -> purple)
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
          throw new Error(`undefined style: ${attrs.opts.style}`);
      }

      if (uPlotOpts !== undefined) {
        plot = new uPlot(uPlotOpts, uPlotData, vnode.dom);

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



m.route.prefix = "";

m.route(document.body, "/overview", {
  "/:section": {
    onmatch(params) {
      return m.request({
        method: "GET",
        url: `/data/${params.section}.json`,
        withCredentials: true,
      }).then(data => ({
        view() {
          return m(Main, { ...data, route: '/' + params.section });
        }
      }));
    }
  }
});

