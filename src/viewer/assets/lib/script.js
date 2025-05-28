import {
    ChartsState, Chart
} from './charts/chart.js';

// Sidebar component
const Sidebar = {
    view({
        attrs
    }) {
        return m("div#sidebar", [
            attrs.sections.map((section) => m(m.route.Link, {
                class: attrs.activeSection === section ? 'selected' : '',
                href: section.route,
            }, section.name))
        ]);
    }
};

// Main component
const Main = {
    view({
        attrs: {
            activeSection,
            groups,
            sections,
            source,
            version,
            filename
        }
    }) {
        return m("div",
            m("header", [
                m('h1', 'Rezolus', m('span.div', ' » '), activeSection.name),
                m('div.metadata', [
                    m('p.filename', `File: ${filename}`),
                    m('p.version', `Source: ${source} • Version: ${version}`),
                ]),
            ]),
            m("main", [
                m(Sidebar, {
                    activeSection,
                    sections
                }),
                m(SectionContent, {
                    section: activeSection,
                    groups
                })
            ]));
    }
};

const SectionContent = {
    view({
        attrs
    }) {
        return m("div#section-content", [
            attrs.section.name === "cgroups" ? m(CgroupsControls) : undefined,
            m("div#groups",
                attrs.groups.map((group) => m(Group, group))
            )
        ]);
    }
};

const CgroupsControls = {
    view({
        attrs
    }) {
        return m("div#cgroups-controls", [
            m("label.checkbox", [
                m("input[type=checkbox]", {
                    checked: chartsState.colorMapper.getUseConsistentCgroupColors(),
                    onchange: (e) => {
                        chartsState.colorMapper.setUseConsistentCgroupColors(e.target.checked);
                        // All cgroups section charts need to be reinitialized
                        chartsState.charts.forEach(chart => chart.isInitialized() && chart.reinitialize());
                    }
                }),
                "Keep cgroup colors consistent across charts"
            ])
        ]);
    }
};

// Group component
const Group = {
    view({
        attrs
    }) {
        return m("div.group", {
            id: attrs.id
        }, [
            m("h2", `${attrs.name}`),
            m("div.charts", attrs.plots.map(spec => m(Chart, { spec, chartsState }))),
        ]);
    }
};

// Application state management
const chartsState = new ChartsState();

const sectionResponseCache = {};

// Fetch data for a section and cache it.
const preloadSection = (section) => {
    if (sectionResponseCache[section]) {
        return Promise.resolve();
    }

    const url = `/data/${section}.json`;
    console.time(`Preload ${url}`);
    return m.request({
        method: "GET",
        url,
        withCredentials: true,
    }).then(data => {
        console.timeEnd(`Preload ${url}`);
        sectionResponseCache[section] = data;
    });
};

// Preload data for all sections in the background.
const preloadSections = (allSections) => {
    // Create a queue of sections to preload
    const sectionsToPreload = allSections
        .filter(section => !sectionResponseCache[section.route])
        .map(section => section.route.substring(1));

    const preloadNext = () => {
        if (sectionsToPreload.length === 0) return;

        const nextSection = sectionsToPreload.shift();
        preloadSection(nextSection).then(() => {
            // Schedule the next preload during idle time
            if (window.requestIdleCallback) {
                window.requestIdleCallback(preloadNext);
            } else {
                // Subsequent delays can be small. We're making the requests serially anyway.
                setTimeout(preloadNext, 100);
            }
        });
    };

    // Start preloading the first section
    // We use requestIdleCallback if available to minimize performance impact.
    if (window.requestIdleCallback) {
        window.requestIdleCallback(preloadNext);
    } else {
        // Fallback to a fixed initial delay if requestIdleCallback is not supported (e.g. Safari)
        setTimeout(preloadNext, 2000);
    }
};

// Main application entry point
m.route.prefix = ""; // use regular paths for navigation, eg. /overview
m.route(document.body, "/overview", {
    "/:section": {
        onmatch(params, requestedPath) {
            // Prevent a route change if we're already on this route
            if (m.route.get() === requestedPath) {
                return new Promise(function () { });
            }

            if (requestedPath !== m.route.get()) {
                // Reset charts state.
                chartsState.clear();

                // Reset scroll position.
                window.scrollTo(0, 0);
            }

            if (sectionResponseCache[params.section]) {
                const data = sectionResponseCache[params.section];
                const activeSection = data.sections.find(section => section.route === requestedPath);
                return ({
                    view() {
                        return m(Main, {
                            ...data,
                            activeSection
                        });
                    }
                });
            }

            const url = `/data/${params.section}.json`;
            console.time(`Load ${url}`);
            return m.request({
                method: "GET",
                url,
                withCredentials: true,
            }).then(data => {
                console.timeEnd(`Load ${url}`);
                sectionResponseCache[params.section] = data;
                const activeSection = data.sections.find(section => section.route === requestedPath);

                // Preload other sections after initial load
                preloadSections(data.sections);

                return ({
                    view() {
                        return m(Main, {
                            ...data,
                            activeSection
                        });
                    }
                });
            });
        }
    }
});