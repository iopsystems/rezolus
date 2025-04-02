import uplot from './uPlot.esm.js';

const data = [
  [1546300800, 1546387200], // x-values (timestamps)
  [35, 71], // y-values (series 1)
  [90, 15] // y-values (series 2)
];

const opts = {
  title: "Plot",
  width: 400,
  height: 300,
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

const elem = document.body;
uplot(opts, data, elem);