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

          const config = {              
            // Color palette (purple to orange to white)
            colorPalette: [
              "#440154",
              "#481b6d",
              "#46327e",
              "#3f4788",
              "#365c8d",
              "#2e6e8e",
              "#277f8e",
              "#21918c",
              "#1fa187",
              "#2db27d",
              "#4ac16d",
              "#73d056",
              "#a0da39",
              "#d0e11c",
              "#fde725"
            ],

            // Minimum cell size in pixels (prevents cells from becoming too small)
            minCellSize: 4
          };

          // Flatten the 2D data matrix
          const xValues = Array.from({ length: numCols }, (_, i) => i);
          const yValues = Array.from({ length: numRows }, (_, i) => i);
          const zValues = [];
          for (let i = 0; i < numCols; i++) {
            const row = data[i];
            for (let j = 0; j < numRows; j++) {
              zValues.push(row[j]);
            }
          }
          log(xValues, yValues, zValues);

          uPlotOpts = {
            ...attrs.opts,

            cursor: {
              lock: false,
              points: {
                show: false,
              },
              drag: {
                setScale: true,
                x: true,
                y: false,
              },
            },
            scales: {
              x: {
                time: false,
                // auto: false,
                range: [0, numCols - 1],
              },
              y: {
                time: false,
                // auto: false,
                range: [0, numRows - 1],
                dir: -1, // Invert y-axis so (0,0) is at top-left
              }
            },
            axes: [
              {
                // X axis options
                label: "Time",
                stroke: () => "#ABABAB",
                ticks: { stroke: () => "#333333", },
                grid: { show: false },
                scale: 'x',
                values: (u, vals) => vals.map(v => v.toFixed(0)),
              },
              {
                // Y axis options
                label: "Value",
                stroke: () => "#ABABAB",
                ticks: { stroke: () => "#333333", },
                grid: { show: false },
                scale: "y",
                values: (u, vals) => vals.map(v => v.toFixed(0)),
              },
            ],

            series: [
              {},
              {
                scale: 'y',
                paths: () => null, // No line drawing
                points: { show: false },
              },
              {
                scale: 'y',
                paths: () => null, // No line drawing
                points: { show: false },
              }
            ],
            plugins: [
              heatmapPlugin(config),
            ],
          };
          
          uPlotData = [xValues, yValues, zValues];
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

