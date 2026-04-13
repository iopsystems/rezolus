// Shared service extension components and route builders.
// Used by both the binary viewer and the static site viewer.

/**
 * Render service section content (metadata table, KPI groups, unavailable list).
 * Returns a mithril vnode for use inside SectionContent.
 *
 * @param {object} attrs - SectionContent attrs (must include .metadata, .groups)
 * @param {Function} Group - Group component
 * @param {string} sectionRoute
 * @param {string} sectionName
 * @param {number} interval
 */
const renderServiceSection = (attrs, Group, sectionRoute, sectionName, interval) => {
    const meta = attrs.metadata || {};
    const serviceName = meta.service_name || 'Service';
    const serviceMeta = meta.service_metadata || {};
    const unavailable = meta.unavailable_kpis || [];
    return m('div#section-content', [
        m('h1', serviceName),
        Object.keys(serviceMeta).length > 0
            ? m('table.sysinfo-table', [
                m('tbody', Object.entries(serviceMeta).map(([k, v]) =>
                    m('tr', [m('td.sysinfo-key', k), m('td', v)])
                )),
            ])
            : null,
        m('div#groups',
            (attrs.groups || []).map((group) =>
                m(Group, { ...group, sectionRoute, sectionName, interval })
            )
        ),
        unavailable.length > 0 && m('div.service-notes', [
            m('h3', 'Unavailable KPIs'),
            m('p', 'The following KPIs have no matching data in this recording:'),
            m('ul', unavailable.map((kpi) =>
                m('li', [
                    m('strong', kpi.title),
                    ` (${kpi.role})`,
                ])
            )),
        ]),
    ]);
};

/**
 * Build mithril route definitions for service sections.
 *
 * @param {object} deps
 * @param {object} deps.sectionResponseCache
 * @param {Function} deps.loadSection - (sectionKey) => Promise<data>
 * @param {Function} deps.preloadSections - (sections) => void
 * @param {object} deps.chartsState
 * @param {object} deps.Main - Main layout component
 * @param {object} deps.TopNav - TopNav component
 * @param {Function} deps.topNavAttrs - (data, route) => attrs
 * @param {object} deps.SingleChartView
 * @param {Function} deps.applyResultToPlot
 * @returns {object} route map with '/service/:serviceName' and '/service/:serviceName/chart/:chartId'
 */
const createServiceRoutes = (deps) => {
    const {
        sectionResponseCache,
        loadSection,
        preloadSections,
        chartsState,
        Main,
        TopNav,
        topNavAttrs,
        SingleChartView,
        applyResultToPlot,
    } = deps;

    return {
        '/service/:serviceName/chart/:chartId': {
            onmatch(params) {
                const svcKey = `service/${params.serviceName}`;

                const makeView = () => ({
                    view() {
                        const data = sectionResponseCache[svcKey];
                        if (!data) return m('div', 'Loading...');
                        const activeSection = data.sections.find(s => s.route === `/service/${params.serviceName}`);
                        return m('div', [
                            m(TopNav, topNavAttrs(data, activeSection?.route)),
                            m('main.single-chart-main', [
                                m(SingleChartView, {
                                    data,
                                    chartId: decodeURIComponent(params.chartId),
                                    applyResultToPlot,
                                }),
                            ]),
                        ]);
                    },
                });

                if (sectionResponseCache[svcKey]) {
                    return makeView();
                }
                return loadSection(svcKey).then(() => makeView());
            },
        },
        '/service/:serviceName': {
            onmatch(params, requestedPath) {
                if (m.route.get() === requestedPath) {
                    return new Promise(function () {});
                }
                if (requestedPath !== m.route.get()) {
                    chartsState.charts.clear();
                    window.scrollTo(0, 0);
                }

                const svcKey = `service/${params.serviceName}`;

                const makeView = () => ({
                    view() {
                        const data = sectionResponseCache[svcKey];
                        if (!data) return m('div', 'Loading...');
                        const activeSection = data.sections.find(
                            (section) => section.route === `/service/${params.serviceName}`,
                        );
                        return m(Main, { ...data, activeSection });
                    },
                });

                if (sectionResponseCache[svcKey]) {
                    return makeView();
                }
                return loadSection(svcKey).then((data) => {
                    if (data?.sections) preloadSections(data.sections);
                    return makeView();
                });
            },
        },
    };
};

export { renderServiceSection, createServiceRoutes };
