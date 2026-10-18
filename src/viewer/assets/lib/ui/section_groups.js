// The sidebar's grouping of the navigation. Built-in sections are found by
// route, not by name: a section template names its sections, and a
// recording's template may rename them.

// Routes the sidebar lists on their own, above the sampler sections.
const PINNED_ROUTES = ['/query', '/overview', '/exceptions'];

// Group `sections` (`[{ name, route }]`) for the sidebar. In compare mode
// the cgroup section is hidden (v1 scope).
export const groupSections = (sections, compareMode) => {
    const visible = compareMode
        ? sections.filter((s) => s.route !== '/cgroups')
        : sections;
    return {
        queryExplorer: visible.find((s) => s.route === '/query'),
        overview: visible.find((s) => s.route === '/overview'),
        exceptions: visible.find((s) => s.route === '/exceptions'),
        services: visible.filter((s) => s.route.startsWith('/service/')),
        // Foreign (non-Rezolus) source sections get their own "Metrics"
        // group; "Samplers" is reserved for the Rezolus agent's built-in
        // samplers.
        metrics: visible.filter((s) => s.route.startsWith('/source/')),
        samplers: visible.filter(
            (s) => !PINNED_ROUTES.includes(s.route)
                && !s.route.startsWith('/service/')
                && !s.route.startsWith('/source/'),
        ),
    };
};
