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

import { specForSourceMetric } from '../charts/source_metric.js';
import { jitterSpec } from '../charts/jitter.js';
import { ViewerApi } from '../viewer_api.js';
import { DEFAULT_SORT, cycleSortKeys, sortMetrics } from './metric_sort.js';
import { expandLink } from '../ui/chart_controls.js';

// The catalog has no real "timestamp" metric — it's a synthetic row so the
// table offers a way into the jitter chart (inter-sample delta) alongside
// the source's actual metrics.
//
// jitterSpec (charts/jitter.js) sets promql_query: null, so selectButton
// (ui/chart_controls.js) omits the Pin icon — pinning assumes a re-runnable
// query, which the jitter pseudo-metric doesn't have. expandLink special-cases
// TIMESTAMP_JITTER_CHART_ID to show Expand anyway; the chart route
// (features/source_routes.js) reconstructs the plot from raw timestamps
// instead of a catalog lookup.
export const withTimestampRow = (metrics) => ([
    {
        name: 'timestamp',
        metric_type: 'timestamp',
        series_count: 1,
        label_keys: [],
        description: 'Per-sample collection time; charted as inter-sample delta (jitter).',
    },
    ...metrics,
]);

// Plot-spec construction lives in charts/source_metric.js so this inline render
// and the /source/:sourceName/chart/:chartId single-chart route derive the SAME
// spec (and the same opts.id, which is the chart-URL handle).

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

            // Jitter-specific state, populated on first selection of the
            // synthetic timestamp row and reused (no re-fetch) across
            // Absolute/Deviation toggling.
            st.jitterMode = 'absolute';
            st.jitterTimestamps = null;
            st.jitterNominalMs = 0;
            // Fallback nominal interval when vnode.attrs.interval isn't
            // available (e.g. a pure foreign capture that never populated
            // the section-interval cache — see source_routes.js). Fetched
            // lazily, only if/when the timestamp row is actually selected.
            st.fileMetadataMs = null;
            // oninit only sees the vnode as of first creation; Mithril does
            // not refresh it on later redraws. view(vnode) re-stashes this
            // every render so resolveNominalMs (below) sees the live value
            // instead of a permanently-undefined one on routes that derive
            // `interval` asynchronously (source_routes.js).
            st.interval = vnode.attrs.interval;

            ViewerApi.getMetrics(sourceName)
                .then((resp) => {
                    // The `source` label is universal within this section (it's
                    // what scopes the section), so drop it from every metric's
                    // displayed/sorted label list.
                    const mapped = ((resp && resp.metrics) || []).map((mi) => ({
                        ...mi,
                        label_keys: (mi.label_keys || []).filter((k) => k !== 'source'),
                    }));
                    st.metrics = withTimestampRow(mapped);
                    st.loading = false;
                    m.redraw();
                })
                .catch((e) => {
                    st.error = e.message || 'Failed to load metrics';
                    st.loading = false;
                    m.redraw();
                });

            // interval (seconds) is a best-effort borrow from another cached
            // section (see source_routes.js) and can be absent; fall back to
            // the file-level sampling_interval_ms metadata.
            st.resolveNominalMs = async () => {
                if (st.interval) return st.interval * 1000;
                if (st.fileMetadataMs != null) return st.fileMetadataMs;
                try {
                    const meta = await ViewerApi.getFileMetadata();
                    st.fileMetadataMs = (meta && meta.sampling_interval_ms) || 0;
                } catch {
                    st.fileMetadataMs = 0;
                }
                return st.fileMetadataMs;
            };

            // Rebuilds the jitter plot from the already-fetched timestamps —
            // no network round-trip. jitterSpec always returns a brand-new
            // object (fresh `data`/`opts`), so this never mutates a spec
            // Chart may still be holding a reference to.
            st.rebuildJitter = () => {
                const entry = st.selected.get('timestamp');
                if (!entry || !st.jitterTimestamps) return;
                entry.plot = jitterSpec(st.jitterTimestamps, {
                    mode: st.jitterMode,
                    nominalMs: st.jitterNominalMs,
                });
            };

            st.selectTimestampRow = async (info) => {
                const entry = { plot: null, status: 'loading' };
                st.selected.set(info.name, entry);
                m.redraw();

                try {
                    const [nominalMs, resp] = await Promise.all([
                        st.resolveNominalMs(),
                        ViewerApi.getTimestamps(sourceName),
                    ]);
                    if (st.selected.get(info.name) !== entry) return;
                    st.jitterNominalMs = nominalMs;
                    st.jitterTimestamps = (resp && resp.timestamps) || [];
                    if (st.jitterTimestamps.length > 1) {
                        entry.plot = jitterSpec(st.jitterTimestamps, {
                            mode: st.jitterMode,
                            nominalMs: st.jitterNominalMs,
                        });
                        entry.status = 'ready';
                    } else {
                        entry.status = 'error';
                        entry.error = 'Not enough samples to compute jitter';
                    }
                } catch (e) {
                    if (st.selected.get(info.name) !== entry) return;
                    entry.status = 'error';
                    entry.error = e.message || 'Failed to load timestamps';
                }
                m.redraw();
            };

            st.toggle = async (info) => {
                if (st.selected.has(info.name)) {
                    st.selected.delete(info.name);
                    m.redraw();
                    return;
                }

                if (info.metric_type === 'timestamp') {
                    await st.selectTimestampRow(info);
                    return;
                }

                const plot = specForSourceMetric(info);
                const entry = { plot, status: 'loading' };
                st.selected.set(info.name, entry);
                m.redraw();

                // runQuery is app.js's processDashboardData wrapper: it runs
                // the plot's promql_query and mutates the plot in place with
                // data/_resolvedStyle, exactly like a real section. The jitter
                // plot never goes through this path — its promql_query is
                // null and jitterSpec already populates `data` directly, so
                // calling runQuery on it would be a no-op at best (data.js
                // skips plots with a null promql_query) and a layer of
                // pointless indirection at worst.
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
            const { interval, Group, sectionRoute, Chart, chartsState } = vnode.attrs;
            // Fresh vnode.attrs only arrive here, not in oninit's closures;
            // restash each render so resolveNominalMs tracks the live value.
            st.interval = interval;
            const f = st.filter.trim().toLowerCase();
            const filtered = f
                ? st.metrics.filter((x) => x.name.toLowerCase().includes(f))
                : st.metrics;
            const rows = sortMetrics(filtered, st.sortKeys);

            const entries = [...st.selected.entries()];
            // The jitter (timestamp) entry gets its own chart-cell box (below,
            // mirroring renderChart) instead of routing through Group — it
            // carries a mode toggle in its own header, unlike a normal metric.
            const timestampEntry = st.selected.get('timestamp');
            const isTimestampReady = !!timestampEntry && timestampEntry.status === 'ready';
            const otherReadyPlots = entries
                .filter(([name, e]) => name !== 'timestamp' && e.status === 'ready')
                .map(([, e]) => e.plot);

            // Ready charts render through the shared Group component so they
            // get titles + style switching + heatmap/spectrum controls. The
            // group name becomes the section's <h2> heading; title-prefixing
            // keys off sectionName (kept '' below) not the group name, so this
            // heading doesn't prefix the per-chart titles (see
            // createGroupComponent's titlePrefix logic). Group renders null for
            // an all-empty group, but otherReadyPlots are non-empty by
            // construction where used below.
            const group = { name: 'Selected metrics', id: 'source-metrics', subgroups: [{ name: null, description: null, plots: otherReadyPlots }] };

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
                    // Jitter chart, boxed like a normal chart-cell (mirrors
                    // viewer_core.js's renderChart) so the mode toggle lives
                    // in the chart's own header instead of floating above
                    // the whole "Selected metrics" block. The toggle flips
                    // st.jitterMode and rebuilds the cached jitter plot in
                    // place — no re-fetch of timestamps — always REASSIGNING
                    // entry.plot to a fresh object (see rebuildJitter) so
                    // Chart never has an old spec mutated out from under it.
                    isTimestampReady && Chart && m('div.chart-cell.full-width', [
                        timestampEntry.plot.opts.description
                            && m('p.chart-description', timestampEntry.plot.opts.description),
                        m('div.chart-wrapper', [
                            m('div.chart-header', m('div.chart-title-row', [
                                m('span.chart-title', timestampEntry.plot.opts.title),
                                m('label.compare-toggle', {
                                    title: 'Show deviation from the nominal sampling interval (jitter) instead of the absolute interval',
                                }, [
                                    m('input[type=checkbox]', {
                                        checked: st.jitterMode === 'deviation',
                                        onchange: () => {
                                            st.jitterMode = st.jitterMode === 'deviation' ? 'absolute' : 'deviation';
                                            st.rebuildJitter();
                                            m.redraw();
                                        },
                                    }),
                                    m('span', 'jitter'),
                                ]),
                            ])),
                            m(Chart, { spec: timestampEntry.plot, chartsState, interval }),
                            expandLink(timestampEntry.plot, sectionRoute),
                        ]),
                    ]),

                    // Other ready charts, rendered through the section pipeline.
                    otherReadyPlots.length > 0 && Group && m('div#groups',
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
