// metric_browser.js — interactive catalog for a foreign (non-Rezolus)
// source. Renders a searchable metric table; selecting a row runs the
// type-appropriate default query and renders the resulting chart through
// the SAME section pipeline the dashboard uses.
//
// Charts are rendered via the shared `Group` component (passed in from
// app.js), so selected metrics get titles, style switching, the heatmap
// toggle, and the histogram Full/Tail spectrum controls for free — the
// controls' fetch wiring keys off `spec.promql_query`, which the older
// bare-`Chart` render omitted (that's why titles were blank and Full/Tail
// did nothing). The per-metric query is run via `runQuery` (app.js's
// processDashboardData wrapper) which populates the plot spec in place.

import { buildDefaultQuery } from '../charts/metric_types.js';
import { ViewerApi } from '../viewer_api.js';
import { DEFAULT_SORT, cycleSortKeys, sortMetrics } from './metric_sort.js';

// Build a section-style plot spec for a metric. Carrying `promql_query`
// (not just opts) is what lets the section pipeline populate data and the
// scatter Full/Tail controls fetch their spectra. Histograms need
// opts.subtype so resolveStyle() picks 'scatter'; gauge and counter infer
// their style from the result shape.
const specForMetric = (info) => {
    const opts = {
        id: `source-metric-${info.name}`,
        title: info.name,
        description: info.description || '',
        type: info.metric_type,
    };
    // The section pipeline (buildEffectiveQuery) wraps histograms itself from
    // opts.type/subtype, so a histogram's promql_query must be the RAW metric —
    // passing buildDefaultQuery's already-wrapped histogram_quantiles(...) would
    // double-wrap it (`histogram_quantiles(..., histogram_quantiles(...))`) and
    // return no data. Counters/gauges are not re-wrapped by the pipeline, so they
    // carry their buildDefaultQuery form (rate(...) / raw).
    let promql_query;
    if (info.metric_type === 'histogram') {
        opts.subtype = 'percentiles';
        promql_query = info.name;
    } else {
        promql_query = buildDefaultQuery(info);
    }
    return { promql_query, opts };
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
            // Multi-key table sort (systemslab-style). Plain-click a header to
            // sort by it; shift-click to add secondary/tertiary keys.
            st.sortKeys = DEFAULT_SORT.slice();
            st.onSort = (col, shift) => { st.sortKeys = cycleSortKeys(st.sortKeys, col, shift); };
            // name -> { plot, status: 'loading'|'ready'|'error', error? }
            st.selected = new Map();

            ViewerApi.getMetrics(sourceName)
                .then((resp) => {
                    // The `source` label is universal within this section (it's
                    // what scopes the section), so drop it from every metric's
                    // displayed/sorted label list.
                    st.metrics = ((resp && resp.metrics) || []).map((mi) => ({
                        ...mi,
                        label_keys: (mi.label_keys || []).filter((k) => k !== 'source'),
                    }));
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

                // runQuery is app.js's processDashboardData wrapper: it runs
                // the plot's promql_query and mutates the plot in place with
                // data/_resolvedStyle, exactly like a real section.
                const runQuery = vnode.attrs.runQuery;
                try {
                    await runQuery(plot);
                    // The row may have been deselected while the query was
                    // in flight; don't resurrect it.
                    if (st.selected.get(info.name) !== entry) return;
                    const hasData = Array.isArray(plot.data)
                        && plot.data.some((s) => Array.isArray(s) && s.length > 0);
                    if (hasData) {
                        entry.status = 'ready';
                    } else {
                        entry.status = 'error';
                        entry.error = 'Query returned no data';
                    }
                } catch (e) {
                    if (st.selected.get(info.name) !== entry) return;
                    entry.status = 'error';
                    entry.error = e.message || 'Query failed';
                }
                m.redraw();
            };
        },

        view(vnode) {
            const st = vnode.state;
            const { interval, Group, sectionRoute } = vnode.attrs;
            const f = st.filter.trim().toLowerCase();
            const filtered = f
                ? st.metrics.filter((x) => x.name.toLowerCase().includes(f))
                : st.metrics;
            const rows = sortMetrics(filtered, st.sortKeys);

            const entries = [...st.selected.entries()];
            const readyPlots = entries
                .filter(([, e]) => e.status === 'ready')
                .map(([, e]) => e.plot);

            // Ready charts render through the shared Group component so they
            // get titles + style switching + heatmap/spectrum controls. The
            // group name becomes the section's <h2> heading; title-prefixing
            // keys off sectionName (kept '' below) not the group name, so this
            // heading doesn't prefix the per-chart titles (see
            // createGroupComponent's titlePrefix logic). Group renders null for
            // an all-empty group, but readyPlots are non-empty by construction.
            const group = { name: 'Selected metrics', id: 'source-metrics', subgroups: [{ name: null, description: null, plots: readyPlots }] };

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
                    m('thead', m('tr', [
                        m('th', ''), // checkbox column — not sortable
                        ...['name', 'type', 'series', 'labels', 'description'].map((col) => {
                            const idx = st.sortKeys.findIndex((k) => k.col === col);
                            const dir = idx !== -1 ? st.sortKeys[idx].dir : null;
                            return m('th.sortable', {
                                onclick: (e) => { st.onSort(col, e.shiftKey); },
                                title: 'Click to sort; Shift-click to add a sort key',
                            }, [
                                col,
                                dir && m('span.sort-ind', [
                                    dir === 'asc' ? ' ▲' : ' ▼',
                                    st.sortKeys.length > 1 && m('sup.sort-rank', String(idx + 1)),
                                ]),
                            ]);
                        }),
                    ])),
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

                m('div.metric-charts', [
                    // Loading / error rows for metrics whose query is in
                    // flight or failed. Group only renders plots with data,
                    // so these transient states get their own lightweight
                    // rows (keeping title + feedback visible), rather than a
                    // bare-Chart render that would lose title/controls. Kept
                    // in a wrapper so this list's keyed rows don't sit as
                    // siblings of the unkeyed Group (Mithril forbids mixing).
                    m('div.metric-pending', entries.flatMap(([name, entry]) => {
                        if (entry.status === 'loading') {
                            return [m('div.query-chart', { key: name }, [
                                m('h3', name),
                                m('p', 'Loading…'),
                            ])];
                        }
                        if (entry.status === 'error') {
                            return [m('div.query-chart', { key: name }, [
                                m('h3', name),
                                m('div.error-message', entry.error || 'Query failed'),
                            ])];
                        }
                        return [];
                    })),
                    // Ready charts, rendered through the section pipeline.
                    readyPlots.length > 0 && Group && m('div#groups',
                        m(Group, {
                            ...group,
                            sectionRoute,
                            sectionName: '',
                            interval,
                        }),
                    ),
                ]),
            ]);
        },
    };

    componentBySource.set(sourceName, component);
    return component;
}
