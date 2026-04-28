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
    const categoryMembers = Array.isArray(meta.category_members) ? meta.category_members : null;
    const categoryUnavailable = Array.isArray(meta.category_unavailable) ? meta.category_unavailable : [];
    const { instances = [], selectedInstance = null, onInstanceChange } = instanceOpts;
    const hasMultiInstance = instances.length > 1;

    const headerTitle = categoryMembers
        ? `${serviceName} — ${categoryMembers.join(' vs ')}`
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
        categoryUnavailable.length > 0 && m('div.section-notes', [
            m('h3', 'Skipped Comparisons'),
            m('p', 'The following category KPIs were skipped because one member did not have a matching chart:'),
            m('ul', categoryUnavailable.map((entry) =>
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
        getDefaultRoute,
    } = deps;
    const readCompareMode = () => (typeof getCompareMode === 'function' ? !!getCompareMode() : false);
    // Recover from a missing service section (stale URL pointing at a
    // service that this capture doesn't render) by sending the user to
    // the dashboard's default route instead of letting the "Unknown
    // section" error bubble out of mithril's loop.
    //
    // m.route.get() returns the last *successfully resolved* path, so
    // when this is the very first route resolution it stays empty no
    // matter how many times we redirect. That makes the
    // `target !== m.route.get()` guard insufficient on its own — if
    // getDefaultRoute() itself points at a section that isn't in
    // dashboard_sections (e.g. compare-mode-without-category, where
    // alias-driven section keys diverge from `serviceInstances` keys
    // derived from per_source_metadata), we'd bounce between the
    // broken route and itself indefinitely. Fall back to /overview
    // when the redirect target matches the failing route — overview
    // is always generated.
    const recoverFromMissingSection = (svcKey, err) => {
        console.warn(`[viewer] section ${svcKey} not available; redirecting to default route`, err);
        const failingRoute = `/${svcKey}`;
        let target = typeof getDefaultRoute === 'function' ? getDefaultRoute() : '/overview';
        if (target === failingRoute) target = '/overview';
        if (target && target !== m.route.get()) {
            m.route.set(target);
        }
        return new Promise(function () {});
    };
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
                return loadSection(svcKey)
                    .then(() => makeView())
                    .catch((err) => recoverFromMissingSection(svcKey, err));
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
                return loadSection(svcKey)
                    .then((data) => {
                        const sections = readSections(data);
                        if (sections.length > 0) preloadSections(sections);
                        return makeView();
                    })
                    .catch((err) => recoverFromMissingSection(svcKey, err));
            },
        },
    };
};

export { renderServiceSection, createServiceRoutes };
