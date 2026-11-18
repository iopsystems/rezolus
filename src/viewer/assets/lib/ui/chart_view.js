// The viewer's Mithril component for one chart: m(ChartView, { spec,
// chartsState, interval }). It owns a charts/chart.js Chart and drives its
// mount/update/destroy from the Mithril lifecycle.
//
// Loading this module also gives the chart modules the viewer's theme
// version, Mithril redraw and event annotations through charts/host.js.
// data.js registers the data functions the same way.

import { Chart } from '../charts/chart.js';
import { setChartHost } from '../charts/host.js';
import { themeVersion } from './theme.js';
import { eventsStore } from '../events/events_store.js';
import { openEventForm, openEventInfo } from '../events/event_form.js';

// Events (hairlines and description tags) are shown only in the Notebook and
// Report views, never in the raw metric sections.
function isEventDisplayAllowed() {
    if (typeof m === 'undefined' || !m.route?.get) return false;
    const route = m.route.get() || '';
    return route.startsWith('/notebook') || route.startsWith('/report');
}

// Adding and deleting events is Notebook only, so a Report reader cannot
// change what they are reading.
function isEventEditingAllowed() {
    if (typeof m === 'undefined' || !m.route?.get) return false;
    const route = m.route.get() || '';
    return route.startsWith('/notebook');
}

// The host functions for the viewer, built from the events store and the two
// event popovers it is given. Exported so tests can pass stand-ins.
export function viewerChartHost({ eventsStore, openEventForm, openEventInfo }) {
    return {
        getThemeVersion: () => themeVersion,
        redraw: () => m.redraw(),
        eventsForChart: ({ chartId, scope }) =>
            (isEventDisplayAllowed() ? eventsStore.filterForChart({ chartId, scope }) : []),
        subscribeEvents: (fn) => eventsStore.subscribe(fn),
        eventEditingAllowed: isEventEditingAllowed,
        openEventForm: ({ anchorEl, prefill, onSubmit }) => openEventForm({
            anchorEl,
            prefill,
            onSubmit: (event) => {
                eventsStore.add(event);
                onSubmit(event);
            },
        }),
        openEventInfo: ({ anchorPoint, event }) => openEventInfo({
            anchorPoint,
            event,
            onDelete: isEventEditingAllowed()
                ? () => eventsStore.remove({
                    timestamp: event.timestamp,
                    description: event.description,
                })
                : null,
        }),
    };
}

setChartHost(viewerChartHost({ eventsStore, openEventForm, openEventInfo }));

export class ChartView {
    constructor(vnode) {
        this.chart = new Chart(vnode.attrs);
    }

    oncreate(vnode) {
        this.chart.mount(vnode.dom);
    }

    onupdate(vnode) {
        this.chart.update(vnode.attrs);
    }

    onremove() {
        this.chart.destroy();
    }

    view() {
        return m('div.chart');
    }
}
