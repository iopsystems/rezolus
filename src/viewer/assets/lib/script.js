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
      attrs.sections.map((section) => m('div.section', m(m.route.Link, { href: section.route, }, section.name)))
    ]);
  }
};

const Main = {
  view({ attrs: { groups, sections } }) {
    return m("div", 
      m("header", [
        m('h1', 'Rezolus'),
      ]),
      m("main", [
        m(Sidebar, { sections }),
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
          // note: assumes nonempty data
          const data = attrs.data;
          const numRows = data[0].length;
          const numCols = data.length;

          // Define your color palette and config
          const config = {
            colorPalette: [
              "#440154", "#481b6d", "#46327e", /* ...your colors... */ "#fde725"
            ],
            minCellSize: 4
          };

          // Create x/y coordinate arrays
          const xData = [];
          const yData = [];
          for (let i = 0; i < numRows; i++) {
            for (let j = 0; j < numCols; j++) {
              xData.push(j);
              yData.push(i);
            }
          }

          // Define heatmapPaths function
          function heatmapPaths(opts) {
            const { disp } = opts;

            return (u, seriesIdx, idx0, idx1) => {
              // This is a simplified version of the function from the example
              return u.orient(u, seriesIdx, (series, dataX, dataY, scaleX, scaleY, valToPosX, valToPosY, xOff, yOff, xDim, yDim, moveTo, lineTo, rect) => {
                let d = u.data[seriesIdx];
                let [xs, ys, counts] = d;
                let dlen = xs.length;

                // Get fill colors
                let fills = disp.fill.values(u, seriesIdx);
                let fillPalette = disp.fill.lookup;
                let fillPaths = fillPalette.map(color => new Path2D());

                // Calculate number of y bins by finding where pattern repeats
                let yBinCount = 0;
                for (let i = 1; i < dlen; i++) {
                  if (xs[i] === xs[0]) {
                    yBinCount = i;
                    break;
                  }
                }
      
                let xBinCount = dlen / yBinCount;
      
                // Calculate bin sizes
                let xBinSize = Math.abs(valToPosX(xs[yBinCount], scaleX, xDim, xOff) - valToPosX(xs[0], scaleX, xDim, xOff));
                let yBinSize = Math.abs(valToPosY(ys[1], scaleY, yDim, yOff) - valToPosY(ys[0], scaleY, yDim, yOff));
      
                // Minimum cell size
                xBinSize = Math.max(xBinSize, opts.minCellSize || 2);
                yBinSize = Math.max(yBinSize, opts.minCellSize || 2);

                // Pre-calculate positions
                let xPositions = [];
                let yPositions = [];
      
                for (let i = 0; i < xBinCount; i++) {
                  xPositions.push(Math.round(valToPosX(xs[i * yBinCount], scaleX, xDim, xOff) - xBinSize / 2));
                }
      
                for (let i = 0; i < yBinCount; i++) {
                  yPositions.push(Math.round(valToPosY(ys[i], scaleY, yDim, yOff) - yBinSize / 2));
                }

                // Draw each cell
                for (let i = 0; i < dlen; i++) {
                  // Skip cells with null values or out of view
                  if (counts[i] === null || fills[i] === -1 || 
                    xs[i] < scaleX.min || xs[i] > scaleX.max ||
                    ys[i] < scaleY.min || ys[i] > scaleY.max) {
                    continue;
                  }

                  let xIdx = Math.floor(i / yBinCount);
                  let yIdx = i % yBinCount;
        
                  let xPos = xPositions[xIdx];
                  let yPos = yPositions[yIdx];

                  let fillPath = fillPaths[fills[i]];
                  rect(fillPath, xPos, yPos, xBinSize, yBinSize);
                }

                // Apply colors and return paths
                u.ctx.save();
                u.ctx.rect(u.bbox.left, u.bbox.top, u.bbox.width, u.bbox.height);
                u.ctx.clip();
      
                fillPaths.forEach((p, i) => {
                  u.ctx.fillStyle = fillPalette[i];
                  u.ctx.fill(p);
                });
      
                u.ctx.restore();

                return null;
              });
            };
          }

          // Helper function to flatten 2D data into a heatmap format
          // Generate counts data
          function heatmap(xs, ys, opts) {
            // Find min/max
            let minX = Math.min(...xs);
            let maxX = Math.max(...xs);
            let minY = Math.min(...ys);
            let maxY = Math.max(...ys);

            // Calculate bins
            let xBinSize = opts.x.binSize;
            let yBinSize = opts.y.binSize;
  
            // Round to bin boundaries
            let minXBin = opts.x.bin(minX);
            let maxXBin = opts.x.bin(maxX);
            let minYBin = opts.y.bin(minY);
            let maxYBin = opts.y.bin(maxY);
  
            // Number of bins in each dimension
            let xBinCount = Math.ceil((maxXBin - minXBin) / xBinSize) + 1;
            let yBinCount = Math.ceil((maxYBin - minYBin) / yBinSize) + 1;
  
            // Initialize result arrays
            let xs2 = [];
            let ys2 = [];
            let counts = [];
  
            // Initialize the counts map
            for (let x = minXBin; x <= maxXBin; x += xBinSize) {
              for (let y = minYBin; y <= maxYBin; y += yBinSize) {
                xs2.push(x);
                ys2.push(y);
                counts.push(0);
              }
            }
  
            // Fill the counts from data
            for (let i = 0; i < xs.length; i++) {
              let xBin = opts.x.bin(xs[i]);
              let yBin = opts.y.bin(ys[i]);
    
              // Find index in the counts array
              let xIdx = Math.floor((xBin - minXBin) / xBinSize);
              let yIdx = Math.floor((yBin - minYBin) / yBinSize);
              let idx = yIdx * xBinCount + xIdx;
    
              if (idx >= 0 && idx < counts.length) {
                counts[idx]++;
              }
            }
  
            // Create lookup for your values
            let valueMap = new Map();
            for (let i = 0; i < numRows; i++) {
              for (let j = 0; j < numCols; j++) {
                valueMap.set(`${j},${i}`, data[j][i]);
              }
            }
  
            // Now replace counts with actual values
            for (let i = 0; i < xs2.length; i++) {
              let key = `${xs2[i]},${ys2[i]}`;
              if (valueMap.has(key)) {
                counts[i] = valueMap.get(key);
              }
            }
  
            return {
              xs: xs2,
              ys: ys2,
              counts: counts
            };
          }

          // Create heatmap data structure
          const hmData = heatmap(xData, yData, {
            x: { binSize: 1, bin: v => Math.floor(v), sorted: true },
            y: { binSize: 1, bin: v => Math.floor(v) }
          });

          // Final uPlot options
          uPlotOpts = {
            ...attrs.opts,
            mode: 2,
            cursor: {
              lock: false,
              points: { show: false },
              drag: { setScale: true, x: true, y: false },
            },
            scales: {
              x: {
                time: false,
                range: [0, numCols - 1],
              },
              y: {
                time: false,
                range: [0, numRows - 1],
                dir: -1,
              }
            },
            axes: [
              { 
                label: "Time",
                stroke: () => "#ABABAB",
                ticks: { stroke: () => "#333333" },
                grid: { show: false },
                scale: 'x',
                values: (u, vals) => vals.map(v => v.toFixed(0)),
              },
              {
                label: "Value",
                stroke: () => "#ABABAB",
                ticks: { stroke: () => "#333333" },
                grid: { show: false },
                scale: "y",
                values: (u, vals) => vals.map(v => v.toFixed(0)),
              }
            ],
            series: [
              {},
              {
                label: "Heatmap",
                paths: heatmapPaths({
                  disp: {
                    fill: {
                      lookup: config.colorPalette,
                      values: (u, seriesIdx) => {
                        let counts = u.data[seriesIdx][2];
                        // Normalize values to palette indices
                        // (implementation from above)
                      }
                    }
                  },
                  minCellSize: config.minCellSize
                }),
                facets: [
                  { scale: 'x', auto: true, sorted: 1 },
                  { scale: 'y', auto: true }
                ],
              }
            ],
          };

          // Set data
          uPlotData = [null, [hmData.xs, hmData.ys, hmData.counts]];

          
          // const config = {              
          //   // Color palette (purple to orange to white)
          //   colorPalette: [
          //     "#440154",
          //     "#481b6d",
          //     "#46327e",
          //     "#3f4788",
          //     "#365c8d",
          //     "#2e6e8e",
          //     "#277f8e",
          //     "#21918c",
          //     "#1fa187",
          //     "#2db27d",
          //     "#4ac16d",
          //     "#73d056",
          //     "#a0da39",
          //     "#d0e11c",
          //     "#fde725"
          //   ],

          //   // Minimum cell size in pixels (prevents cells from becoming too small)
          //   minCellSize: 4
          // };

          // // Flatten the 2D data matrix
          // const xValues = Array.from({ length: numCols }, (_, i) => i);
          // const yValues = Array.from({ length: numRows }, (_, i) => i);
          // const zValues = [];
          // for (let i = 0; i < numCols; i++) {
          //   const row = data[i];
          //   for (let j = 0; j < numRows; j++) {
          //     zValues.push(row[j]);
          //   }
          // }
          // log(xValues, yValues, zValues);

          // uPlotOpts = {
          //   ...attrs.opts,

          //   cursor: {
          //     lock: false,
          //     points: {
          //       show: false,
          //     },
          //     drag: {
          //       setScale: true,
          //       x: true,
          //       y: false,
          //     },
          //   },
          //   scales: {
          //     x: {
          //       time: false,
          //       // auto: false,
          //       range: [0, numCols - 1],
          //     },
          //     y: {
          //       time: false,
          //       // auto: false,
          //       range: [0, numRows - 1],
          //       dir: -1, // Invert y-axis so (0,0) is at top-left
          //     }
          //   },
          //   axes: [
          //     {
          //       // X axis options
          //       label: "Time",
          //       stroke: () => "#ABABAB",
          //       ticks: { stroke: () => "#333333", },
          //       grid: { show: false },
          //       scale: 'x',
          //       values: (u, vals) => vals.map(v => v.toFixed(0)),
          //     },
          //     {
          //       // Y axis options
          //       label: "Value",
          //       stroke: () => "#ABABAB",
          //       ticks: { stroke: () => "#333333", },
          //       grid: { show: false },
          //       scale: "y",
          //       values: (u, vals) => vals.map(v => v.toFixed(0)),
          //     },
          //   ],

          //   series: [
          //     {},
          //     {
          //       scale: 'y',
          //       paths: () => null, // No line drawing
          //       points: { show: false },
          //     },
          //     {
          //       scale: 'y',
          //       paths: () => null, // No line drawing
          //       points: { show: false },
          //     }
          //   ],
          //   plugins: [
          //     heatmapPlugin(config),
          //   ],
          // };
          
          // uPlotData = [xValues, yValues, zValues];
          break;
      }
      if (uPlotOpts !== undefined) {
        // log('opts', opts, attrs.opts.style);
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



// m.route.prefix = "";

m.route(document.body, "/overview", {
  "/:section": {
    onmatch(params) {
      return m.request({
        method: "GET",
        url: `/data/${params.section}.json`,
        withCredentials: true,
      }).then(data => ({
        view() {
          return m(Main, data);
        }
      }));
    }
  }
});

