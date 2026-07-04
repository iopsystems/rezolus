// metric_browser.js — interactive catalog for a foreign (non-Rezolus)
// source. Renders a searchable metric table; selecting a row runs the
// type-appropriate default query and mounts an inline Chart.
//
// This is the Query Explorer's query→Chart path (features/explorers.js)
// driven by table selection instead of a text box. The Chart/ChartsState
// wiring mirrors SingleChartView: one ChartsState for the section, a plot
// object per selected metric populated via applyResultToPlot after the
// query resolves, and a freshly-spread spec passed to Chart each render so
// echarts reconfigures when the data reference changes.

import { ChartsState, Chart } from '../charts/chart.js';
import { executePromQLRangeQuery, applyResultToPlot } from '../data.js';
import { buildDefaultQuery } from '../charts/metric_types.js';
import { ViewerApi } from '../viewer_api.js';

// Histograms need opts.subtype so resolveStyle() picks 'scatter'; gauge
// and counter infer their style from the result shape.
const specForMetric = (info) => {
    const opts = {
        id: `source-metric-${info.name}`,
        title: info.name,
        description: info.description || '',
        type: info.metric_type,
    };
    if (info.metric_type === 'histogram') opts.subtype = 'percentiles';
    return { opts };
};

// The component must keep a stable vnode identity across redraws or Mithril
// tears it down and remounts it (losing filter/selection/chartsState and
// re-firing getMetrics). A plain module-level object would share state
// across sources, so memoize one stable component per sourceName.
const componentBySource = new Map();

export function MetricBrowserView(sourceName) {
    let component = componentBySource.get(sourceName);
    if (component) return component;

    component = {
        oninit(vnode) {
            const st = vnode.state;
            st.filter = '';
            st.metrics = [];
            st.loading = true;
            st.error = null;
            // name -> { plot, status: 'loading'|'ready'|'error', error? }
            st.selected = new Map();
            st.chartsState = new ChartsState();

            ViewerApi.getMetrics(sourceName)
                .then((resp) => {
                    st.metrics = (resp && resp.metrics) || [];
                    st.loading = false;
                    m.redraw();
                })
                .catch((e) => {
                    st.error = e.message || 'Failed to load metrics';
                    st.loading = false;
                    m.redraw();
                });

            st.toggle = async (info) => {
                if (st.selected.has(info.name)) {
                    st.selected.delete(info.name);
                    m.redraw();
                    return;
                }

                const plot = specForMetric(info);
                const entry = { plot, status: 'loading' };
                st.selected.set(info.name, entry);
                m.redraw();

                try {
                    const response = await executePromQLRangeQuery(buildDefaultQuery(info));
                    // The row may have been deselected while the query was
                    // in flight; don't resurrect it.
                    if (st.selected.get(info.name) !== entry) return;
                    if (response && response.status === 'success' && response.data && response.data.result) {
                        applyResultToPlot(plot, response);
                        entry.status = 'ready';
                    } else {
                        entry.status = 'error';
                        entry.error = (response && response.error) || 'Query returned no data';
                    }
                } catch (e) {
                    if (st.selected.get(info.name) !== entry) return;
                    entry.status = 'error';
                    entry.error = e.message || 'Query failed';
                }
                m.redraw();
            };
        },

        onremove(vnode) {
            if (vnode.state.chartsState) vnode.state.chartsState.clear();
        },

        view(vnode) {
            const st = vnode.state;
            const interval = vnode.attrs.interval;
            const f = st.filter.trim().toLowerCase();
            const rows = f
                ? st.metrics.filter((x) => x.name.toLowerCase().includes(f))
                : st.metrics;

            return m('div.metric-browser', [
                m('div.section-header-row', [
                    m('h1.section-title', `source: ${sourceName}`),
                ]),
                m('input.metric-search', {
                    type: 'text',
                    placeholder: 'Search metrics…',
                    value: st.filter,
                    oninput: (e) => { st.filter = e.target.value; },
                }),

                st.error && m('div.error-message', st.error),
                st.loading && m('p', 'Loading metrics…'),

                !st.loading && !st.error && m('table.metric-table', [
                    m('thead', m('tr',
                        ['', 'name', 'type', 'series', 'labels', 'description']
                            .map((h) => m('th', h)))),
                    m('tbody', rows.map((info) => {
                        const isSel = st.selected.has(info.name);
                        return m('tr', {
                            key: info.name,
                            class: isSel ? 'selected' : '',
                            onclick: () => st.toggle(info),
                        }, [
                            m('td', m('input[type=checkbox]', {
                                checked: isSel,
                                // The row onclick already toggles; swallow
                                // the checkbox's own click so it doesn't
                                // double-fire.
                                onclick: (e) => e.preventDefault(),
                            })),
                            m('td', info.name),
                            m('td', info.metric_type),
                            m('td', String(info.series_count)),
                            m('td', (info.label_keys || []).join(', ')),
                            m('td', info.description || ''),
                        ]);
                    })),
                ]),

                m('div.metric-charts', [...st.selected.entries()].map(([name, entry]) => {
                    if (entry.status === 'error') {
                        return m('div.query-chart', { key: name }, [
                            m('h3', name),
                            m('div.error-message', entry.error || 'Query failed'),
                        ]);
                    }
                    // Spread a fresh spec per render so Chart.onupdate sees a
                    // changed spec/data reference and reconfigures echarts;
                    // passing the same mutated object would make dataChanged
                    // always false.
                    return m('div.query-chart', { key: name }, [
                        m(Chart, {
                            spec: { ...entry.plot, opts: { ...entry.plot.opts } },
                            chartsState: st.chartsState,
                            interval,
                        }),
                    ]);
                })),
            ]);
        },
    };

    componentBySource.set(sourceName, component);
    return component;
}
