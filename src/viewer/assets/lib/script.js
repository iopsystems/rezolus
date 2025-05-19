import {
    ChartsState, Chart
} from './chart.js';

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
            sections
        }
    }) {
        return m("div",
            m("header", [
                m('h1', 'Rezolus', m('span.div', ' » '), activeSection.name),
            ]),
            m("main", [
                m(Sidebar, {
                    activeSection,
                    sections
                }),
                m('div#groups',
                    groups.map((group) => m(Group, group))
                )
            ]));
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

            const url = `/data/${params.section}.json`;
            console.time(`Load ${url}`);
            return m.request({
                method: "GET",
                url,
                withCredentials: true,
            }).then(data => {
                console.timeEnd(`Load ${url}`);
                const activeSection = data.sections.find(section => section.route === requestedPath);
                return ({
                    view() {
                        return m(Main, {
                            ...data,
                            activeSection
                        });
                    },
                    oncreate(vnode) {
                    },
                    onremove(vnode) {
                    }
                });
            });
        }
    }
});