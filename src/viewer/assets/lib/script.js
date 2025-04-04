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

      let opts;
      switch (attrs.opts.style) {
        case 'line':
          opts = {
            title: attrs.opts.title,
            id: attrs.opts.id,

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
          break;
        case 'heatmap':
          // todo
          break;
      }
      if (opts !== undefined) {
        // log('opts', opts, attrs.opts.style);
        plot = new uPlot(opts, attrs.data, vnode.dom);

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
      console.log('params', params);
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

