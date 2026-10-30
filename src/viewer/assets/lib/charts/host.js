// What the chart modules need from the page that embeds them.
//
// Chart modules other than chart.js and compare.js import only from inside
// charts/. tests/charts_boundary.test.mjs lists those two files' imports and
// fails on any other.
//
// line.js, scatter.js and util/utils.js get four functions from this object.
// data.js registers the viewer's versions when it loads. A page that embeds
// the charts without data.js calls setChartHost with its own.
//
// Without a host, a chart draws only the data in its spec. The line chart
// leaves out its total toggle, the scatter chart leaves out its spectrum
// checkboxes, and gap detection has no step override.

const DEFAULTS = Object.freeze({
    // (query) -> Promise<PromQL range result>. With applyResultToPlot, draws
    // the line chart's total toggle.
    executePromQLRangeQuery: null,
    // (plot, result) -> void; writes the result into plot.data
    applyResultToPlot: null,
    // (plot, quantiles) -> Promise<spectrum | null>. Draws the scatter
    // chart's spectrum checkboxes.
    fetchQuantileSpectrumForPlot: null,
    // () -> seconds | null
    getStepOverride: () => null,
});

let host = DEFAULTS;

// Merges `impl` over the current host, so two callers can each set their own
// keys. Refuses a key not in DEFAULTS and a value that is not a function.
export function setChartHost(impl) {
    if (impl === null || typeof impl !== 'object') throw new Error('setChartHost: expected an object');
    for (const [key, value] of Object.entries(impl)) {
        if (!(key in DEFAULTS)) throw new Error(`setChartHost: unknown key '${key}'`);
        if (typeof value !== 'function') throw new Error(`setChartHost: '${key}' is not a function`);
    }
    host = Object.freeze({ ...host, ...impl });
}

export function resetChartHost() {
    host = DEFAULTS;
}

export const chartHost = () => host;
