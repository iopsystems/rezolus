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

          const config = {
            // Dimensions of data matrix
            rows: data[0].length,
            cols: data.length,
              
            // Color palette (purple to orange to white)
            colorPalette: [
              "rgb(131,58,180)", // Dark purple
              "rgb(154,65,159)",
              "rgb(178,67,136)",
              "rgb(202,63,111)",
              "rgb(228,53,80)",
              "rgb(253,29,29)", // Red
              "rgb(255,76,37)",
              "rgb(256,106,45)",
              "rgb(256,131,53)",
              "rgb(255,154,61)",
              "rgb(252,176,69)",
              "rgb(254,193,115)",
              "rgb(255,209,153)",
              "rgb(256,224,188)",
              "rgb(256,240,222)"
            ].reverse(),

            // Minimum cell size in pixels (prevents cells from becoming too small)
            minCellSize: 4
          };

          // Flatten the 2D data matrix
          const xValues = Array.from({ length: config.cols }, (_, i) => i);
          const yValues = Array.from({ length: config.rows }, (_, i) => i);
          const zValues = [];
          for (let i = 0; i < config.cols; i++) {
            const row = data[i];
            for (let j = 0; j < config.rows; j++) {
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
                range: [0, config.cols - 1],
              },
              y: {
                time: false,
                // auto: false,
                range: [0, config.rows - 1],
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

