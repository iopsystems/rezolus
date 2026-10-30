// What the chart modules need from the page that embeds them.
//
// Chart modules import only from inside charts/;
// tests/charts_boundary.test.mjs fails on any other import.
//
// The rezolus viewer fills this object in two places when they load: data.js
// registers the data functions, and ui/chart_view.js registers the theme,
// redraw and event functions. A page that embeds the charts without the
// viewer calls setChartHost with its own.
//
// Without a host, a chart draws only the data in its spec. The line chart
// leaves out its total toggle, the scatter chart leaves out its spectrum
// checkboxes, gap detection has no step override, a chart shows no events
// and offers no "+ Add Event", and a chart takes up a theme change only when
// it next reconfigures for another reason (new data or format) or the page
// rebuilds it.

const DEFAULTS = Object.freeze({
    // Data (line.js, scatter.js, util/utils.js).
    //
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

    // Page (chart.js).
    //
    // () -> number. A chart reconfigures when this differs from the value it
    // last drew with, so the page bumps it on a theme change.
    getThemeVersion: () => 0,
    // () -> void. Called after a chart changes shared zoom state, so the page
    // can re-render what depends on it.
    redraw: () => {},

    // Events (chart.js, base.js).
    //
    // ({ chartId, scope }) -> events to mark on this chart; [] for none.
    eventsForChart: () => [],
    // (callback) -> unsubscribe. The callback runs when the events change.
    subscribeEvents: () => () => {},
    // () -> boolean. With openEventForm, offers "+ Add Event" on a frozen
    // tooltip.
    eventEditingAllowed: () => false,
    // ({ anchorEl, prefill, onSubmit }) -> void. Shows the add-event form;
    // the host stores the event, then calls onSubmit(event).
    openEventForm: null,
    // ({ anchorPoint, event }) -> void. Shows an event's details when its tag
    // is clicked; without it a click on a tag does nothing.
    openEventInfo: null,
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
