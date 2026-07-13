// Route map for foreign (non-Rezolus) "simple capture" source sections.
// Mirrors features/service.js's createServiceRoutes so app.js can spread it in.
//
// Extracted from app.js (rather than inlined) so the single-chart
// reconstruction — the part that was broken in Bug 1 — is unit-testable with
// stubbed deps (see tests/source_routes.test.mjs).
//
// A source section has NO server-rendered groups: the MetricBrowser fetches its
// own catalog and runs per-metric queries client-side. So the expanded
// single-chart route can't resolve a plot from a cached section the way the
// built-in / service chart routes do — it reconstructs the one chart from the
// catalog. The chart id encodes the metric (charts/source_metric.js), so
// specForChartId re-derives the same spec the MetricBrowser rendered.

import { specForChartId } from '../charts/source_metric.js';

export function createSourceRoutes(deps) {
    const {
        sectionResponseCache,
        ViewerApi,
        processDashboardData,
        applyResultToPlot,
        SingleChartView,
        TopNav,
        topNavAttrs,
        Main,
        getSections,
        getCompareMode,
        chartsState,
    } = deps;

    return {
        '/source/:sourceName/chart/:chartId': {
            onmatch(params) {
                const sourceName = params.sourceName;
                const chartId = decodeURIComponent(params.chartId);
                const sectionRoute = `/source/${sourceName}`;
                // Same one-plot pipeline wrapper the MetricBrowser mount uses,
                // so the reconstructed plot is populated identically (data +
                // _resolvedStyle) and renders with a title + working controls.
                const runQuery = (plot) => processDashboardData(
                    { groups: [{ name: '', subgroups: [{ name: null, plots: [plot] }] }] },
                    null,
                    sectionRoute,
                );

                // Closure state, mirroring app.js's '/:section/chart/:chartId':
                // fetch + query asynchronously, redraw when resolved. `ready`
                // is exposed so tests can await the reconstruction.
                const st = { plot: null, loading: true, error: null };
                const ready = ViewerApi.getMetrics(sourceName)
                    .then(async (resp) => {
                        const spec = specForChartId(chartId, (resp && resp.metrics) || []);
                        if (!spec) {
                            st.error = `Chart "${chartId}" not found`;
                        } else {
                            try {
                                await runQuery(spec);
                                st.plot = spec;
                            } catch (e) {
                                st.error = (e && e.message) || 'Query failed';
                            }
                        }
                        st.loading = false;
                        m.redraw();
                    })
                    .catch((e) => {
                        st.error = (e && e.message) || 'Failed to load metrics';
                        st.loading = false;
                        m.redraw();
                    });

                return {
                    ready,
                    view() {
                        const activeSection = getSections().find((s) => s.route === sectionRoute)
                            || { name: `source: ${sourceName}`, route: sectionRoute };
                        if (st.loading) {
                            return m('div#splash', m('div.card', [
                                m('h1', activeSection.name),
                                m('p.subtitle', 'Loading…'),
                                m('div.progress-bar',
                                    m('div.progress-fill.indeterminate'),
                                ),
                            ]));
                        }
                        // Borrow file-level metadata from any cached section for
                        // the TopNav chrome; source sections don't populate the
                        // cache, so this is best-effort.
                        const anyCached = Object.values(sectionResponseCache)[0] || {};
                        const data = {
                            groups: st.plot ? [{ subgroups: [{ plots: [st.plot] }] }] : [],
                            filename: anyCached.filename,
                            source: anyCached.source,
                            version: anyCached.version,
                            interval: anyCached.interval,
                            filesize: anyCached.filesize,
                            num_series: anyCached.num_series,
                        };
                        return m('div', [
                            m(TopNav, topNavAttrs(data, activeSection.route)),
                            m('main.single-chart-main', [
                                st.error
                                    ? m('div.single-chart-view', m('p', st.error))
                                    : m(SingleChartView, { data, chartId, applyResultToPlot }),
                            ]),
                        ]);
                    },
                };
            },
        },

        '/source/:sourceName': {
            onmatch(params, requestedPath) {
                if (m.route.get() === requestedPath) {
                    return new Promise(function () {});
                }
                if (requestedPath !== m.route.get()) {
                    chartsState.charts.clear();
                    window.scrollTo(0, 0);
                }
                const sectionRoute = `/source/${params.sourceName}`;
                return {
                    view() {
                        const activeSection = getSections().find(
                            (s) => s.route === sectionRoute,
                        ) || { name: `source: ${params.sourceName}`, route: sectionRoute };
                        // Interval isn't strictly required — Chart tolerates
                        // its absence (QueryExplorer passes none) and the query
                        // window is derived from metadata. Borrow it from any
                        // cached section when present.
                        const anyCached = Object.values(sectionResponseCache)[0];
                        return m(Main, {
                            activeSection,
                            groups: [],
                            sections: getSections(),
                            compareMode: getCompareMode(),
                            interval: anyCached?.interval,
                        });
                    },
                };
            },
        },
    };
}
