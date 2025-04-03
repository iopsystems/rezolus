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

const state = {
  //
};

const Sidebar = {
  view() {
    return m("div#sidebar", { onclick: state.inc }, [
      layout.sections.map((section) => m('div.section', m('a', { href: `#${section.id}` }, section.name)))
    ]);
  }
};

const Main = {
  view() {
    return m("div", 
      m("header", [
        m('h1', 'Rezolus'),
      ]),
      m("main", [
        m(Sidebar),
        m('div#sections', 
          layout.sections.map((section) => m(Section, section))
        )
      ]));
  }
};

const Section = {
  view({ attrs }) {
    return m("div.section", { id: attrs.id }, [
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
      plot = makePlot(vnode.dom);
      // We maintain a resize observer per plot.
      resizeObserver = new ResizeObserver(entries => {
        for (const entry of entries) {
          plot.setSize(entry.contentRect);
        }
      });
      resizeObserver.observe(vnode.dom);
    },
  
    onremove(vnode) {
      resizeObserver?.disconnect();
      resizeObserver = null;
    },

    view: function () {
      return m('div.plot', 'Hello world');
    }
  };
}


function makePlot(parent) {
  const data = [
    [1546300800, 1546387200], // x-values (timestamps)
    [35, 71], // y-values (series 1)
    [90, 15] // y-values (series 2)
  ];

  let { width, height } = parent.getBoundingClientRect();
  log(width, height);

  const opts = {
    title: "Plot",
    width: width,
    height: height,
    legend: {
      show: true,

    },
    axes: [
      {
        stroke: () => "#ABABAB",
        ticks: {
          stroke: () => "#333333",
        },
        grid: {
          stroke: () => "#333333",
        }
      },
      {
        stroke: () => "#ABABAB",
        ticks: {
          stroke: () => "#333333",
        },
        grid: {
          stroke: () => "#333333",
        }
      },
    ],
    series: [
      {},
      {
        stroke: "blue",
        width: 1,
        fill: "#333",
        dash: [10, 5]
      }
    ]
  };

  return uPlot(opts, data, parent);
}

m.mount(document.body, Main);
