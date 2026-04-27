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
const renderServiceSection = (attrs, Group, sectionRoute, sectionName, interval, instanceOpts = {}) => {
    const meta = attrs.metadata || {};
    const serviceName = meta.service_name || 'Service';
    const serviceMeta = meta.service_metadata || {};
    const unavailable = meta.unavailable_kpis || [];
    const bridgeMembers = Array.isArray(meta.bridge_members) ? meta.bridge_members : null;
    const bridgeUnavailable = Array.isArray(meta.bridge_unavailable) ? meta.bridge_unavailable : [];
    const { instances = [], selectedInstance = null, onInstanceChange } = instanceOpts;
    const hasMultiInstance = instances.length > 1;

    const headerTitle = bridgeMembers
        ? `${serviceName} — ${bridgeMembers.join(' vs ')}`
        : serviceName;

    return m('div#section-content', [
        m('h1', headerTitle),
        // Instance selector (only for multi-instance)
        hasMultiInstance && m('div.instance-selector', [
            m('select.instance-select', {
                value: selectedInstance || '__all__',
                onchange: (e) => {
                    const val = e.target.value === '__all__' ? null : e.target.value;
                    if (onInstanceChange) onInstanceChange(val);
                },
            }, [
                m('option', { value: '__all__' }, 'All Instances'),
                ...instances.map(inst => {
                    const label = inst.node
                        ? `Instance ${inst.id} (${inst.node})`
                        : `Instance ${inst.id}`;
                    return m('option', { value: inst.id }, label);
                }),
            ]),
        ]),
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
        unavailable.length > 0 && m('div.section-notes', [
            m('h3', 'Unavailable KPIs'),
            m('p', 'The following KPIs have no matching data in this recording:'),
            m('ul', unavailable.map((kpi) =>
                m('li', [m('strong', kpi.title), ` (${kpi.role})`])
            )),
        ]),
        bridgeUnavailable.length > 0 && m('div.section-notes', [
            m('h3', 'Bridge Skipped'),
            m('p', 'The following bridge KPIs were skipped because one member did not have a matching chart:'),
            m('ul', bridgeUnavailable.map((entry) =>
                m('li', [
                    m('strong', entry.title),
                    ' — missing in ',
                    m('code', entry.missing_member),
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
 * @param {Function} deps.getSections - () => shared sections array
 * @param {Function} deps.withSharedSections - (data) => data + shared sections
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
        getCompareMode,
        getSections,
        withSharedSections,
    } = deps;
    const readCompareMode = () => (typeof getCompareMode === 'function' ? !!getCompareMode() : false);
    const readSections = (data) => {
        const sharedSections = typeof getSections === 'function' ? getSections() : [];
        if (Array.isArray(sharedSections) && sharedSections.length > 0) {
            return sharedSections;
        }
        if (Array.isArray(data?.sections)) {
            return data.sections;
        }
        return [];
    };
    const hydrateSections = (data) => {
        if (!data || typeof data !== 'object') return data;

        const hydrated = typeof withSharedSections === 'function'
            ? withSharedSections(data)
            : data;

        if (Array.isArray(hydrated?.sections)) {
            return hydrated;
        }

        const sections = readSections(data);
        if (sections.length === 0) {
            return hydrated;
        }

        return {
            ...hydrated,
            sections,
        };
    };

    return {
        '/service/:serviceName/chart/:chartId': {
            onmatch(params) {
                const svcKey = `service/${params.serviceName}`;

                const makeView = () => ({
                    view() {
                        const data = sectionResponseCache[svcKey];
                        if (!data) return m('div', 'Loading...');
                        const viewData = hydrateSections(data);
                        const activeSection = readSections(viewData)
                            .find(s => s.route === `/service/${params.serviceName}`);
                        return m('div', [
                            m(TopNav, topNavAttrs(viewData, activeSection?.route, { compareMode: readCompareMode() })),
                            m('main.single-chart-main', [
                                m(SingleChartView, {
                                    data: viewData,
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
                        const viewData = hydrateSections(data);
                        const activeSection = readSections(viewData).find(
                            (section) => section.route === `/service/${params.serviceName}`,
                        );
                        return m(Main, {
                            ...viewData,
                            activeSection,
                            compareMode: readCompareMode(),
                        });
                    },
                });

                if (sectionResponseCache[svcKey]) {
                    return makeView();
                }
                return loadSection(svcKey).then((data) => {
                    const sections = readSections(data);
                    if (sections.length > 0) preloadSections(sections);
                    return makeView();
                });
            },
        },
    };
};

export { renderServiceSection, createServiceRoutes };
